mod common;
use axum::{Json, Router, routing::post};
use codex_hoshikage_gateway::{
    codex_execution::TurnIdentity, delivery::Delivery, direct_content::DirectContent,
    discord::Discord,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn saved_answer_is_delivered_once_without_reexecuting_codex() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "100").await;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let intent = store
        .prepare_direct(request.clone(), "4".into(), workspace, "openai".into())
        .await
        .unwrap();
    store
        .begin_direct_send(request.clone(), intent)
        .await
        .unwrap();
    store
        .acknowledge_direct_turn(request.clone(), "thread".into(), "turn".into())
        .await
        .unwrap();
    let content = DirectContent::new(&cfg.storage.state_dir).unwrap();
    let saved = content
        .save_answer(&request, "hello from Codex", cfg.limits.output_bytes)
        .unwrap();
    store
        .finish_direct(
            request.clone(),
            TurnIdentity {
                thread_id: "thread".into(),
                turn_id: "turn".into(),
            },
            "completed".into(),
            Some(saved),
        )
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let route_seen = seen.clone();
    let router = Router::new().route(
        "/channels/4/messages",
        post(move |Json(body): Json<Value>| {
            let route_seen = route_seen.clone();
            async move {
                route_seen.lock().unwrap().push(body);
                Json(json!({"id":"200","channel_id":"4"}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let delivery = Delivery {
        store: store.clone(),
        discord: Discord::with_endpoint("token".into(), format!("http://{address}")).unwrap(),
    };
    assert!(
        delivery
            .direct_answer(&request, "4", cfg.limits.output_bytes)
            .await
            .unwrap()
    );
    assert!(
        delivery
            .direct_answer(&request, "4", cfg.limits.output_bytes)
            .await
            .unwrap()
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(seen.lock().unwrap()[0]["content"], "hello from Codex");
    assert!(
        delivery
            .direct_answer(&request, "5", cfg.limits.output_bytes)
            .await
            .is_err()
    );
    server.abort();
}
