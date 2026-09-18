mod common;
use axum::{
    Json, Router,
    routing::{get, post},
};
use codex_hoshikage_gateway::{
    codex_transport::{CodexRuntimePool, LaunchConfig},
    delivery::Delivery,
    direct_application::{DirectApplication, DirectControlOutcome},
    direct_config::{Codex, DirectConfig},
    direct_content::DirectContent,
    direct_run::DirectRunService,
    discord::{Discord, input_message},
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
async fn original_discord_message_runs_locally_and_delivers_its_saved_answer() {
    let temp = tempfile::tempdir().unwrap();
    let old = common::config(&temp);
    let home = temp.path().join("codex-home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let cfg = DirectConfig {
        discord: old.discord,
        codex: Codex {
            command: std::env::current_exe().unwrap(),
            home,
            model_provider: "openai".into(),
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            network_access: false,
        },
        storage: old.storage,
        limits: old.limits,
        default_model: "gpt-5.6-luna".into(),
    };
    storage::initialize_direct(&cfg).unwrap();
    let _lock = StateLock::acquire(&cfg.storage.state_dir).unwrap();
    let (store, _done) = Store::open_direct(&cfg).unwrap();
    store
        .add_conversation("4".into(), storage::PROXY_SCOPE.into())
        .await
        .unwrap();
    let original = json!({"id":"999","channel_id":"4","guild_id":"1","author":{"id":"2","bot":false},"webhook_id":null,"content":"hello","attachments":[],"edited_timestamp":null});
    let message = input_message(&original, "1").unwrap();
    let files = Files::new().unwrap();
    let prepared = files.prepare(&message, &cfg.limits).await.unwrap();
    let request = store
        .reserve(
            message.id.clone(),
            message.thread_id.clone(),
            message.metadata_digest(),
            cfg.limits.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    store
        .finalize(
            request.clone(),
            prepared.metadata_digest,
            prepared.digest,
            prepared.attachments,
        )
        .await
        .unwrap();
    drop(prepared.reservation);
    let posts = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = posts.clone();
    let router = Router::new()
        .route(
            "/channels/4",
            get(|| async { Json(json!({"id":"4","guild_id":"1","type":0})) }),
        )
        .route(
            "/channels/4/messages/999",
            get({
                let original = original.clone();
                move || {
                    let original = original.clone();
                    async move { Json(original) }
                }
            }),
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
        args: vec![format!(
            "{}/tests/fixtures/mock_app_server.py",
            env!("CARGO_MANIFEST_DIR")
        )],
        codex_home: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        initialize_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(2),
        experimental_api: true,
    });
    let delivery = Delivery {
        store: store.clone(),
        discord: discord.clone(),
    };
    let app = DirectApplication {
        cfg: cfg.clone(),
        store: store.clone(),
        discord,
        files,
        runs: DirectRunService {
            store: store.clone(),
            pool,
            content: DirectContent::new(&cfg.storage.state_dir).unwrap(),
            state_dir: cfg.storage.state_dir.clone(),
            output_limit: cfg.limits.output_bytes,
            image_max_count: 16,
            image_max_bytes: cfg.limits.artifact_bytes,
        },
        delivery,
    };
    let run = app.start_queued(&request).await.unwrap();
    assert_eq!(
        app.cancel("1001", "4", Some(&run)).await.unwrap(),
        DirectControlOutcome::InterruptAccepted
    );
    assert!(store.request(&request).await.unwrap().stop_requested);
    let result = app.finish_and_deliver(&run).await.unwrap();
    assert!(result.answer_delivered);
    assert_eq!(result.images.delivered, 0);
    assert_eq!(posts.lock().unwrap().len(), 1);
    assert_eq!(posts.lock().unwrap()[0]["content"], "DONE");
    server.abort();
}
