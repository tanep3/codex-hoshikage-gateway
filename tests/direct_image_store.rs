mod common;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use codex_hoshikage_gateway::{
    backup,
    codex_execution::ExecutionOptions,
    codex_transport::{CodexRuntimePool, LaunchConfig},
    delivery::{Delivery, nonce},
    direct_content::DirectContent,
    direct_image_store::ImageRecordState,
    direct_run::DirectRunService,
    discord::Discord,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[tokio::test]
async fn image_only_or_text_plus_image_is_saved_before_terminal_and_in_backup() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "910").await;
    let service = DirectRunService {
        store: store.clone(),
        pool: CodexRuntimePool::new(LaunchConfig {
            command: "python3".into(),
            args: vec![
                format!(
                    "{}/tests/fixtures/mock_app_server.py",
                    env!("CARGO_MANIFEST_DIR")
                ),
                "--generated-image".into(),
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
            vec![json!({"type":"text","text":"make image"})],
        )
        .await
        .unwrap();
    service.confirm_terminal(&run).await.unwrap();
    let (state, images) = store
        .direct_image_inventory(request.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state, "COMPLETE");
    assert_eq!(images.len(), 1);
    let ImageRecordState::Ready(saved) = &images[0].state else {
        panic!("generated image was not persisted")
    };
    assert!(
        service
            .content
            .read_image(saved, cfg.limits.artifact_bytes)
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    let bundle = temp.path().join("backup");
    let manifest = backup::create(&store.path, &bundle).unwrap();
    assert!(
        manifest
            .content
            .iter()
            .any(|item| item.relative_path == saved.relative_path)
    );
    backup::verify(&bundle).unwrap();
    #[derive(Default)]
    struct MockDiscord {
        posts: AtomicUsize,
        message: Mutex<serde_json::Value>,
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
                Json(json!([s.message.lock().unwrap().clone()]))
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
    let first = delivery
        .direct_images(&request, "4", "1", cfg.limits.artifact_bytes)
        .await
        .unwrap();
    assert_eq!(first.pending, 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    let delivery_id: String = store
        .call(true, {
            let req = request.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT id FROM deliveries WHERE target_id=?1 AND kind='direct-image'",
                    [req],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    *mock.message.lock().unwrap() = json!({"id":"300","channel_id":"4","author":{"id":"99","bot":true},"nonce":nonce(&delivery_id),"content":"生成画像 1","attachments":[{"filename":"generated-image-1.png","size":saved.bytes}]});
    assert_eq!(
        delivery
            .recover_direct_images("1", cfg.limits.artifact_bytes)
            .await
            .unwrap(),
        1
    );
    let second = delivery
        .direct_images(&request, "4", "1", cfg.limits.artifact_bytes)
        .await
        .unwrap();
    assert_eq!(second.delivered, 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    server.abort();
}
