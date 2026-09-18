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
            model_provider: "openai".into(),
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            network_access: false,
        },
        storage: legacy.storage,
        limits: legacy.limits,
        default_model: "gpt-5.6-luna".into(),
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
            "--request-approval".into(),
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
    let RunEvent::Approval {
        interaction_id,
        operation,
    } = event
    else {
        panic!("expected approval")
    };
    assert_eq!(operation.kind, InteractionKind::CommandApproval);
    assert_eq!(operation.params["command"], "cat report.txt");
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
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .commands
        .send(RunCommand::Approval {
            interaction_id,
            fingerprint: operation.fingerprint,
            decision: ManualDecision::AcceptOnce,
            reply,
        })
        .await
        .unwrap();
    result.await.unwrap().unwrap();
    let event = tokio::time::timeout(Duration::from_secs(15), actor.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, RunEvent::Terminal(result) if result.answer_delivered));
    actor.task.await.unwrap().unwrap();
    assert_eq!(posts.lock().unwrap().len(), 1);
    assert_eq!(posts.lock().unwrap()[0]["content"], "DONE");
    server.abort();
}
