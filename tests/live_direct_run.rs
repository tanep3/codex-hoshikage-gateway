mod common;
use codex_hoshikage_gateway::{
    codex_execution::ExecutionOptions,
    codex_transport::{CodexRuntimePool, Event, LaunchConfig},
    direct_content::DirectContent,
    direct_run::DirectRunService,
    domain::RequestState,
};
use serde_json::json;
use std::{fs, path::PathBuf, time::Duration};

#[tokio::test]
#[ignore = "real Codex model call; run explicitly in an isolated state directory"]
async fn real_codex_answer_is_saved_without_proxy() {
    let root = tempfile::tempdir().unwrap();
    let cfg = common::config(&root);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "100").await;
    let home = root.path().join("codex-home");
    fs::create_dir(&home).unwrap();
    fs::copy(
        PathBuf::from(std::env::var("HOME").unwrap()).join(".codex/auth.json"),
        home.join("auth.json"),
    )
    .unwrap();
    let service = DirectRunService {
        store: store.clone(),
        pool: CodexRuntimePool::new(LaunchConfig {
            command: "codex".into(),
            args: vec!["app-server".into(), "--listen".into(), "stdio://".into()],
            codex_home: home,
            initialize_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            experimental_api: true,
        }),
        content: DirectContent::new(&cfg.storage.state_dir).unwrap(),
        state_dir: cfg.storage.state_dir.clone(),
        output_limit: cfg.limits.output_bytes,
    };
    let mut run=service.start(request.clone(),"4".into(),ExecutionOptions {
        cwd:PathBuf::new(),model:"gpt-5.6-luna".into(),model_provider:"openai".into(),
        sandbox:"workspace-write".into(),approval_policy:"never".into(),
    },vec![json!({"type":"text","text":"Reply with exactly DIRECT_GATEWAY_OK. Do not call tools."})]).await.unwrap();
    tokio::time::timeout(Duration::from_secs(150), async {
        loop {
            match run.recv().await.unwrap() {
                Event::Notification { method, params }
                    if method == "turn/completed"
                        && params["threadId"] == run.identity.thread_id
                        && params["turn"]["id"] == run.identity.turn_id =>
                {
                    break;
                }
                Event::ServerRequest { id, .. } => run
                    .transport()
                    .reject(id, -32601, "No tools are authorized in this probe")
                    .await
                    .unwrap(),
                Event::Closed(reason) => panic!("Codex child closed: {reason}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let snapshot = service.confirm_terminal(&run).await.unwrap();
    assert_eq!(snapshot.status, "completed");
    assert!(
        snapshot
            .final_text
            .as_deref()
            .unwrap_or("")
            .contains("DIRECT_GATEWAY_OK")
    );
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::Completed
    );
    let saved = store.direct_answer(request).await.unwrap().unwrap();
    assert!(
        service
            .content
            .read_answer(&saved, cfg.limits.output_bytes)
            .unwrap()
            .contains("DIRECT_GATEWAY_OK")
    );
    run.shutdown().await.unwrap();
}
