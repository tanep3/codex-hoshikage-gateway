mod common;
use codex_hoshikage_gateway::{
    codex_execution::ExecutionOptions,
    codex_transport::{CodexRuntimePool, Event, LaunchConfig},
    direct_approval::ManualDecision,
    direct_content::DirectContent,
    direct_run::DirectRunService,
    domain::RequestState,
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[tokio::test]
async fn local_dispatch_stores_answer_before_terminal_commit() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "100").await;
    let pool = CodexRuntimePool::new(LaunchConfig {
        command: "python3".into(),
        args: vec![format!(
            "{}/tests/fixtures/mock_app_server.py",
            env!("CARGO_MANIFEST_DIR")
        )],
        codex_home: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        initialize_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(2),
        experimental_api: true,
    });
    let service = DirectRunService {
        store: store.clone(),
        pool,
        content: DirectContent::new(&cfg.storage.state_dir).unwrap(),
        state_dir: cfg.storage.state_dir.clone(),
        output_limit: cfg.limits.output_bytes,
        image_max_count: 16,
        image_max_bytes: cfg.limits.artifact_bytes,
    };
    let options = ExecutionOptions {
        cwd: PathBuf::new(),
        model: "gpt-5.6-luna".into(),
        model_provider: "openai".into(),
        sandbox: "workspace-write".into(),
        approval_policy: "on-request".into(),
        network_access: false,
    };
    let run = service
        .start(
            request.clone(),
            "4".into(),
            options,
            vec![json!({"type":"text","text":"hello"})],
        )
        .await
        .unwrap();
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::Running
    );
    let snapshot = service.confirm_terminal(&run).await.unwrap();
    assert_eq!(snapshot.final_text.as_deref(), Some("DONE"));
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::Completed
    );
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropping_an_unfinished_run_fences_it_as_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "101").await;
    let service = DirectRunService {
        store: store.clone(),
        pool: CodexRuntimePool::new(LaunchConfig {
            command: "python3".into(),
            args: vec![format!(
                "{}/tests/fixtures/mock_app_server.py",
                env!("CARGO_MANIFEST_DIR")
            )],
            codex_home: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            initialize_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(2),
            experimental_api: true,
        }),
        content: DirectContent::new(&cfg.storage.state_dir).unwrap(),
        state_dir: cfg.storage.state_dir.clone(),
        output_limit: cfg.limits.output_bytes,
        image_max_count: 16,
        image_max_bytes: cfg.limits.artifact_bytes,
    };
    let run = service
        .start(
            request.clone(),
            "4".into(),
            ExecutionOptions {
                cwd: PathBuf::new(),
                model: "gpt-5.6-luna".into(),
                model_provider: "openai".into(),
                sandbox: "workspace-write".into(),
                approval_policy: "on-request".into(),
                network_access: false,
            },
            vec![json!({"type":"text","text":"hello"})],
        )
        .await
        .unwrap();
    drop(run);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store.request(&request).await.unwrap().state == RequestState::Unknown {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        store
            .begin_direct_send(request.clone(), "other".into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_manual_approval_replies_to_only_the_original_app_server_call() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "102").await;
    let service = DirectRunService {
        store: store.clone(),
        pool: CodexRuntimePool::new(LaunchConfig {
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
        }),
        content: DirectContent::new(&cfg.storage.state_dir).unwrap(),
        state_dir: cfg.storage.state_dir.clone(),
        output_limit: cfg.limits.output_bytes,
        image_max_count: 16,
        image_max_bytes: cfg.limits.artifact_bytes,
    };
    let mut run = service
        .start(
            request.clone(),
            "4".into(),
            ExecutionOptions {
                cwd: PathBuf::new(),
                model: "gpt-5.6-luna".into(),
                model_provider: "openai".into(),
                sandbox: "workspace-write".into(),
                approval_policy: "on-request".into(),
                network_access: false,
            },
            vec![json!({"type":"text","text":"hello"})],
        )
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), run.recv())
        .await
        .unwrap()
        .unwrap();
    let (id, interaction) = service
        .register_interaction(&run, &event)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::ApprovalRequired
    );
    assert_eq!(interaction.rpc_id, json!("approval-one"));
    service
        .reply_manual(
            &run,
            id.clone(),
            interaction.fingerprint.clone(),
            ManualDecision::AcceptOnce,
        )
        .await
        .unwrap();
    assert!(
        service
            .reply_manual(
                &run,
                id.clone(),
                interaction.fingerprint,
                ManualDecision::AcceptOnce,
            )
            .await
            .is_err()
    );
    let resolved = tokio::time::timeout(Duration::from_secs(2), run.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(resolved, Event::Notification { method, .. } if method == "serverRequest/resolved")
    );
    store.resolve_direct_approval(id).await.unwrap();
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::Running
    );
    service.confirm_terminal(&run).await.unwrap();
    run.shutdown().await.unwrap();
}
