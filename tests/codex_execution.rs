use codex_hoshikage_gateway::{
    codex_execution::{CodexExecution, ExecutionOptions},
    codex_transport::{CodexTransport, LaunchConfig},
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[tokio::test]
async fn typed_execution_preserves_thread_and_turn_identity() {
    let transport = CodexTransport::launch(&LaunchConfig {
        command: "python3".into(),
        args: vec![
            format!(
                "{}/tests/fixtures/mock_app_server.py",
                env!("CARGO_MANIFEST_DIR")
            ),
            "--enforce-sandbox".into(),
        ],
        codex_home: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        initialize_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(2),
        experimental_api: true,
    })
    .await
    .unwrap();
    let execution = CodexExecution::new(transport.clone());
    let options = ExecutionOptions {
        cwd: PathBuf::from("/tmp"),
        model: "gpt-5.6-luna".into(),
        model_provider: "openai".into(),
        sandbox: "workspace-write".into(),
        approval_policy: "on-request".into(),
        network_access: false,
    };
    let thread = execution.start_thread(&options).await.unwrap();
    execution.resume_thread(&thread, &options).await.unwrap();
    let turn = execution
        .start_turn(
            &thread,
            &options,
            vec![json!({"type":"text","text":"hello"})],
        )
        .await
        .unwrap();
    let snapshot = execution.read_turn(&turn).await.unwrap();
    assert_eq!(snapshot.identity, turn);
    assert_eq!(snapshot.status, "completed");
    assert_eq!(snapshot.final_text.as_deref(), Some("DONE"));
    execution.interrupt(&turn).await.unwrap();
    execution
        .steer(&turn, vec![json!({"type":"text","text":"more"})])
        .await
        .unwrap();
    transport.shutdown().await.unwrap();
}
