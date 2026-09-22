mod common;
use axum::{
    Json, Router,
    routing::{get, post},
};
use codex_hoshikage_gateway::{
    codex_transport::{CodexRuntimePool, LaunchConfig},
    delivery::Delivery,
    direct_application::DirectApplication,
    direct_approval::{InteractionKind, ManualDecision},
    direct_config::{Codex, DirectConfig},
    direct_content::DirectContent,
    direct_run::DirectRunService,
    direct_run_actor::{self, RunCommand, RunEvent},
    discord::Discord,
    files::Files,
    storage::{self, StateLock, Store},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[tokio::test]
async fn actor_routes_exact_approval_then_delivers_after_turn_completion() {
    actor_accepts("--request-approval", Some(InteractionKind::CommandApproval)).await;
}

#[tokio::test]
async fn actor_routes_mcp_tool_confirmation_with_its_own_reply_schema() {
    actor_accepts("--request-mcp-approval", Some(InteractionKind::UserInput)).await;
}

#[tokio::test]
async fn actor_applies_explicit_run_grant_to_second_verified_call_only() {
    actor_accepts(
        "--request-mcp-run-grant",
        Some(InteractionKind::McpElicitation),
    )
    .await;
}

#[tokio::test]
async fn actor_invalidates_run_grant_before_steer() {
    actor_accepts(
        "--request-mcp-run-grant-steer",
        Some(InteractionKind::McpElicitation),
    )
    .await;
}

#[tokio::test]
async fn actor_rejects_unknown_server_request_without_hanging_the_turn() {
    actor_accepts("--request-unknown-method", None).await;
}

async fn actor_accepts(flag: &str, expected_kind: Option<InteractionKind>) {
    let steer_invalidates_grant = flag == "--request-mcp-run-grant-steer";
    let temp = tempfile::tempdir().unwrap();
    let legacy = common::config(&temp);
    let home = temp.path().join("codex-home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let cfg = DirectConfig {
        discord: legacy.discord,
        codex: Codex {
            command: std::env::current_exe().unwrap(),
            home,
            workspace_root: None,
            model_provider: "openai".into(),
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            network_access: false,
        },
        storage: legacy.storage,
        limits: legacy.limits,
        default_model: "gpt-5.6-luna".into(),
        default_reasoning_effort: "high".into(),
    };
    storage::initialize_direct(&cfg).unwrap();
    let _lock = StateLock::acquire(&cfg.storage.state_dir).unwrap();
    let (store, _) = Store::open_direct(&cfg).unwrap();
    store
        .add_conversation("4".into(), storage::PROXY_SCOPE.into())
        .await
        .unwrap();
    let request = store
        .reserve("901".into(), "4".into(), "meta".into(), cfg.limits.clone())
        .await
        .unwrap()
        .unwrap();
    store
        .finalize(request.clone(), "meta".into(), "input".into(), vec![])
        .await
        .unwrap();
    let posts = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = posts.clone();
    let router = Router::new()
        .route(
            "/channels/4",
            get(|| async { Json(json!({"id":"4","guild_id":"1","type":0})) }),
        )
        .route(
            "/channels/4/messages",
            post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(body);
                    Json(json!({"id":"1000","channel_id":"4"}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let discord = Discord::with_endpoint("token".into(), format!("http://{address}")).unwrap();
    let pool = CodexRuntimePool::new(LaunchConfig {
        command: "python3".into(),
        args: vec![
            format!(
                "{}/tests/fixtures/mock_app_server.py",
                env!("CARGO_MANIFEST_DIR")
            ),
            flag.into(),
        ],
        codex_home: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        initialize_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(2),
        experimental_api: true,
    });
    let app = DirectApplication {
        cfg: cfg.clone(),
        store: store.clone(),
        discord: discord.clone(),
        files: Files::new().unwrap(),
        runs: DirectRunService {
            store: store.clone(),
            pool,
            content: DirectContent::new(&cfg.storage.state_dir).unwrap(),
            state_dir: cfg.storage.state_dir.clone(),
            workspace_root: cfg.workspace_root(),
            output_limit: cfg.limits.output_bytes,
            image_max_count: 16,
            image_max_bytes: cfg.limits.artifact_bytes,
        },
        delivery: Delivery {
            store: store.clone(),
            discord,
        },
    };
    let run = app
        .runs
        .start(
            request.clone(),
            "4".into(),
            cfg.execution(),
            vec![json!({"type":"text","text":"hello"})],
        )
        .await
        .unwrap();
    let mut actor = direct_run_actor::spawn(app, run);
    let event = tokio::time::timeout(Duration::from_secs(5), actor.events.recv())
        .await
        .unwrap()
        .unwrap();
    if expected_kind.is_none() {
        assert!(matches!(event, RunEvent::UnsupportedApproval));
        let terminal = tokio::time::timeout(Duration::from_secs(15), actor.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(terminal, RunEvent::Terminal(result) if result.answer_delivered));
        actor.task.await.unwrap().unwrap();
        assert_eq!(posts.lock().unwrap()[0]["content"], "DONE");
        server.abort();
        return;
    }
    let RunEvent::Approval {
        interaction_id,
        operation,
        input_generation,
    } = event
    else {
        panic!("expected approval")
    };
    assert_eq!(Some(operation.kind.clone()), expected_kind);
    if expected_kind == Some(InteractionKind::CommandApproval) {
        assert_eq!(operation.params["command"], "cat report.txt");
    } else if expected_kind == Some(InteractionKind::UserInput) {
        assert_eq!(
            operation.params["questions"][0]["id"],
            "mcp_tool_call_approval_item-one"
        );
    } else {
        assert_eq!(
            operation.run_grant_tool(),
            Some(("playwright".into(), "browser_click".into()))
        );
    }
    let (reply, result) = tokio::sync::oneshot::channel();
    let command = if matches!(
        expected_kind,
        Some(InteractionKind::UserInput | InteractionKind::McpElicitation)
    ) {
        RunCommand::McpToolApproval {
            interaction_id,
            operation: *operation,
            decision: ManualDecision::AcceptOnce,
            run_grant: expected_kind == Some(InteractionKind::McpElicitation),
            input_generation,
            reply,
        }
    } else {
        RunCommand::Approval {
            interaction_id,
            fingerprint: operation.fingerprint,
            decision: ManualDecision::AcceptOnce,
            input_generation,
            reply,
        }
    };
    actor.commands.send(command).await.unwrap();
    result.await.unwrap().unwrap();
    let resolved = tokio::time::timeout(Duration::from_secs(15), actor.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(resolved, RunEvent::ApprovalResolved { .. }));
    if steer_invalidates_grant {
        let (steer_reply, steer_result) = tokio::sync::oneshot::channel();
        actor
            .commands
            .send(RunCommand::Steer {
                interaction_id: "902".into(),
                discord_thread_id: "4".into(),
                input: vec![json!({"type":"text","text":"follow up"})],
                reply: steer_reply,
            })
            .await
            .unwrap();
        steer_result.await.unwrap().unwrap();

        let invalidated = tokio::time::timeout(Duration::from_secs(15), actor.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            invalidated,
            RunEvent::ApprovalInvalidated {
                input_generation: 1
            }
        ));

        let second = tokio::time::timeout(Duration::from_secs(15), actor.events.recv())
            .await
            .unwrap()
            .unwrap();
        let RunEvent::Approval {
            interaction_id,
            operation,
            input_generation,
        } = second
        else {
            panic!("steer must invalidate the Run grant and require a new approval")
        };
        assert_eq!(
            operation.run_grant_tool(),
            Some(("playwright".into(), "browser_click".into()))
        );

        let (reply, result) = tokio::sync::oneshot::channel();
        actor
            .commands
            .send(RunCommand::McpToolApproval {
                interaction_id,
                operation: *operation,
                decision: ManualDecision::Decline,
                run_grant: false,
                input_generation,
                reply,
            })
            .await
            .unwrap();
        result.await.unwrap().unwrap();
        let second_resolved = tokio::time::timeout(Duration::from_secs(15), actor.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(second_resolved, RunEvent::ApprovalResolved { .. }));
    } else if expected_kind == Some(InteractionKind::McpElicitation) {
        let second = tokio::time::timeout(Duration::from_secs(15), actor.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(second, RunEvent::ApprovalResolved { .. }),
            "second call must be auto-approved without a Discord card"
        );
    }
    let event = tokio::time::timeout(Duration::from_secs(15), actor.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, RunEvent::Terminal(result) if result.answer_delivered));
    actor.task.await.unwrap().unwrap();
    let approval_state: String = store
        .call(true, move |connection| {
            Ok(connection.query_row(
                "SELECT state FROM direct_interactions WHERE request_id=?1",
                [&request],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(approval_state, "RESOLVED");
    assert_eq!(posts.lock().unwrap().len(), 1);
    assert_eq!(posts.lock().unwrap()[0]["content"], "DONE");
    if expected_kind == Some(InteractionKind::McpElicitation) {
        let kinds:Vec<String>=store.call(false,|c|{
            let mut q=c.prepare("SELECT kind FROM admin_audit WHERE kind LIKE 'direct_mcp_run_grant_%' ORDER BY created_at,id")?;
            Ok(q.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await.unwrap();
        assert!(kinds.contains(&"direct_mcp_run_grant_selected".into()));
        if steer_invalidates_grant {
            assert_eq!(kinds.len(), 1);
            assert!(!kinds.contains(&"direct_mcp_run_grant_applied".into()));
        } else {
            assert_eq!(kinds.len(), 2);
            assert!(kinds.contains(&"direct_mcp_run_grant_applied".into()));
        }
    }
    server.abort();
}
