mod common;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use codex_hoshikage_gateway::{
    backup,
    delivery::{Delivery, nonce},
    direct_artifacts,
    direct_config::{Codex, DirectConfig},
    direct_content::DirectContent,
    direct_workspace::ensure_conversation_workspace,
    discord::Discord,
    storage::{self, StateLock, Store},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

async fn capture_test(
    store: &Store,
    content: &DirectContent,
    call_id: &str,
    path: &str,
    max_bytes: usize,
) -> anyhow::Result<direct_artifacts::Artifact> {
    direct_artifacts::capture(
        store,
        content,
        direct_artifacts::CaptureTarget {
            thread_id: "4",
            request_id: None,
            call_id,
            relative: path,
            display_name: None,
        },
        max_bytes,
    )
    .await
}

#[tokio::test]
async fn artifact_capture_is_conversation_bound_immutable_and_backed_up() {
    let temp = tempfile::tempdir().unwrap();
    let base = common::config(&temp);
    let home = temp.path().join("codex-home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let cfg = DirectConfig {
        discord: base.discord,
        codex: Codex {
            command: std::env::current_exe().unwrap(),
            home,
            workspace_root: None,
            model_provider: "openai".into(),
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            network_access: false,
        },
        storage: base.storage,
        limits: base.limits,
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
        .reserve("100".into(), "4".into(), "meta".into(), cfg.limits.clone())
        .await
        .unwrap()
        .unwrap();
    store
        .finalize(request.clone(), "meta".into(), "input".into(), vec![])
        .await
        .unwrap();
    let workspace = ensure_conversation_workspace(&cfg.storage.state_dir, "4").unwrap();
    store
        .prepare_direct(request, "4".into(), workspace.clone(), "openai".into())
        .await
        .unwrap();
    fs::write(workspace.join("report.txt"), b"v1").unwrap();
    let content = DirectContent::new(&cfg.storage.state_dir).unwrap();
    let first = capture_test(
        &store,
        &content,
        "interaction-1",
        "report.txt",
        cfg.limits.artifact_bytes,
    )
    .await
    .unwrap();
    assert_eq!(
        content
            .read_artifact(&first.saved, cfg.limits.artifact_bytes)
            .unwrap(),
        b"v1"
    );
    assert_eq!(direct_artifacts::list(&store, "4").await.unwrap().len(), 1);
    let duplicate = capture_test(
        &store,
        &content,
        "interaction-1",
        "report.txt",
        cfg.limits.artifact_bytes,
    )
    .await
    .unwrap();
    assert_eq!(first.id, duplicate.id);
    fs::write(workspace.join("report.txt"), b"v2").unwrap();
    let second = capture_test(
        &store,
        &content,
        "interaction-4",
        "report.txt",
        cfg.limits.artifact_bytes,
    )
    .await
    .unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(
        content
            .read_artifact(&second.saved, cfg.limits.artifact_bytes)
            .unwrap(),
        b"v2"
    );
    assert_eq!(
        capture_test(
            &store,
            &content,
            "interaction-1",
            "report.txt",
            cfg.limits.artifact_bytes,
        )
        .await
        .unwrap()
        .id,
        first.id
    );
    assert_eq!(
        content
            .read_artifact(&first.saved, cfg.limits.artifact_bytes)
            .unwrap(),
        b"v1"
    );
    symlink("report.txt", workspace.join("linked.txt")).unwrap();
    assert!(
        capture_test(
            &store,
            &content,
            "interaction-2",
            "linked.txt",
            cfg.limits.artifact_bytes,
        )
        .await
        .is_err()
    );
    assert!(
        capture_test(
            &store,
            &content,
            "interaction-3",
            "../report.txt",
            cfg.limits.artifact_bytes,
        )
        .await
        .is_err()
    );
    let bundle = temp.path().join("backup");
    backup::create(&cfg.storage.state_dir.join("gateway.sqlite3"), &bundle).unwrap();
    let verified = backup::verify(&bundle).unwrap();
    assert_eq!(
        verified
            .content
            .iter()
            .filter(|item| item.relative_path == first.saved.relative_path)
            .count(),
        1
    );
    #[derive(Default)]
    struct MockDiscord {
        posts: AtomicUsize,
        receipt: Mutex<Value>,
    }
    let mock = Arc::new(MockDiscord::default());
    let router = Router::new()
        .route(
            "/users/@me",
            get(|| async { Json(json!({"id":"99","bot":true})) }),
        )
        .route(
            "/channels/4",
            get(|| async {
                Json(json!({"id":"4","guild_id":"1","type":0,"permission_overwrites":[]}))
            }),
        )
        .route(
            "/guilds/1/members/99",
            get(|| async { Json(json!({"user":{"id":"99"},"roles":[]})) }),
        )
        .route(
            "/guilds/1/roles",
            get(|| async { Json(json!([{"id":"1","permissions":"8"}])) }),
        )
        .route(
            "/channels/4/messages",
            post(|State(s): State<Arc<MockDiscord>>| async move {
                s.posts.fetch_add(1, Ordering::SeqCst);
                StatusCode::BAD_GATEWAY
            })
            .get(|State(s): State<Arc<MockDiscord>>| async move {
                Json(json!([s.receipt.lock().unwrap().clone()]))
            }),
        )
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let discord = Discord::with_endpoint("token".into(), format!("http://{address}")).unwrap();
    discord.identify_bot().await.unwrap();
    let delivery = Delivery {
        store: store.clone(),
        discord,
    };
    assert!(
        !delivery
            .direct_artifact(&first, "4", "1", cfg.limits.artifact_bytes)
            .await
            .unwrap()
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(
        capture_test(
            &store,
            &content,
            "interaction-5",
            "report.txt",
            cfg.limits.artifact_bytes,
        )
        .await
        .is_err()
    );
    let delivery_id: String = store
        .call(false, {
            let artifact_id = first.id.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT id FROM deliveries WHERE target_id=?1 AND kind='direct-artifact'",
                    [artifact_id],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    *mock.receipt.lock().unwrap() = json!({"id":"300","channel_id":"4","author":{"id":"99","bot":true},"nonce":nonce(&delivery_id),"content":"指定された成果物です。","attachments":[{"filename":"report.txt","size":2}]});
    assert_eq!(
        delivery
            .recover_direct_artifacts("1", cfg.limits.artifact_bytes)
            .await
            .unwrap(),
        1
    );
    assert!(
        delivery
            .direct_artifact(&first, "4", "1", cfg.limits.artifact_bytes)
            .await
            .unwrap()
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    server.abort();
}
