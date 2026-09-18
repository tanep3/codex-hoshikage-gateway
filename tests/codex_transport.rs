use codex_hoshikage_gateway::codex_transport::{
    CodexRuntimePool, CodexTransport, Event, LaunchConfig, TransportError,
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

fn config() -> LaunchConfig {
    LaunchConfig {
        command: "python3".into(),
        args: vec![format!(
            "{}/tests/fixtures/mock_app_server.py",
            env!("CARGO_MANIFEST_DIR")
        )],
        codex_home: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        initialize_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(2),
        experimental_api: true,
    }
}

#[tokio::test]
async fn null_response_and_server_request_id_collision() {
    let transport = CodexTransport::launch(&config()).await.unwrap();
    let mut events = transport.subscribe();
    assert_eq!(
        transport.request("test/null", json!({})).await.unwrap(),
        json!(null)
    );
    assert_eq!(
        transport
            .request("test/collision", json!({}))
            .await
            .unwrap(),
        json!({"ok":true})
    );
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, Event::ServerRequest { method, .. } if method == "item/commandExecution/requestApproval")
    );
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_hung_request_is_unknown_but_not_resent() {
    let transport = CodexTransport::launch(&config()).await.unwrap();
    assert!(matches!(
        transport.request("test/hang", json!({})).await,
        Err(TransportError::ResultUnknown(_))
    ));
    assert!(!transport.is_closed());
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn process_exit_closes_pending_requests() {
    let transport = CodexTransport::launch(&config()).await.unwrap();
    assert!(matches!(
        transport.request("test/exit", json!({})).await,
        Err(TransportError::ResultUnknown(_))
    ));
    assert!(transport.is_closed());
    assert!(matches!(
        transport.request("test/echo", json!({})).await,
        Err(TransportError::NotSent(_))
    ));
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_requests_are_correlated() {
    let transport = CodexTransport::launch(&config()).await.unwrap();
    let first = transport.request("test/echo", json!({"run":"one"}));
    let second = transport.request("test/echo", json!({"run":"two"}));
    let (a, b) = tokio::join!(first, second);
    assert_eq!(a.unwrap(), json!({"run":"one"}));
    assert_eq!(b.unwrap(), json!({"run":"two"}));
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn initialization_hang_does_not_start_a_ready_transport() {
    let mut config = config();
    config.args.push("--hang-init".into());
    config.initialize_timeout = Duration::from_millis(100);
    assert!(matches!(
        CodexTransport::launch(&config).await,
        Err(TransportError::ResultUnknown(_))
    ));
}

#[tokio::test]
async fn explicit_shutdown_reaps_the_child() {
    let transport = CodexTransport::launch(&config()).await.unwrap();
    let pid = transport.pid();
    transport.shutdown().await.unwrap();
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
}

#[tokio::test]
async fn runtime_pool_limits_only_new_children() {
    let pool = CodexRuntimePool::new(config());
    let left = pool.acquire().await.unwrap();
    let right = pool.acquire().await.unwrap();
    assert_ne!(left.transport().pid(), right.transport().pid());
    assert_eq!(pool.available(), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), pool.acquire())
            .await
            .is_err()
    );
    // Control calls to a running child do not need a new execution permit.
    assert_eq!(
        left.transport()
            .request("test/null", json!({}))
            .await
            .unwrap(),
        json!(null)
    );
    left.shutdown().await.unwrap();
    let next = pool.acquire().await.unwrap();
    next.shutdown().await.unwrap();
    right.shutdown().await.unwrap();
    assert_eq!(pool.available(), 2);
}

#[tokio::test]
async fn string_server_id_is_answered_without_confusing_client_requests() {
    let transport = CodexTransport::launch(&config()).await.unwrap();
    let mut events = transport.subscribe();
    let requested = transport.request("test/string-request", json!({}));
    let handled = async {
        let event = events.recv().await.unwrap();
        let Event::ServerRequest { id, method, .. } = event else {
            panic!("expected server request")
        };
        assert_eq!(id, "approval-X");
        assert_eq!(method, "item/commandExecution/requestApproval");
        transport
            .respond(id, json!({"decision":"decline"}))
            .await
            .unwrap();
    };
    let (result, ()) = tokio::join!(requested, handled);
    let result = result.unwrap();
    assert_eq!(result["replyId"], "approval-X");
    assert_eq!(result["decision"]["decision"], "decline");
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn duplicate_approval_argument_is_not_accepted_as_a_valid_request() {
    let transport = CodexTransport::launch(&config()).await.unwrap();
    let mut events = transport.subscribe();
    transport
        .request("test/duplicate-request", json!({}))
        .await
        .unwrap();
    assert!(
        matches!(events.recv().await.unwrap(), Event::InvalidServerRequest { id, .. } if id == "bad")
    );
    transport.shutdown().await.unwrap();
}
