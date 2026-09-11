mod common;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use codex_hoshikage_gateway::{delivery::Delivery, discord::Discord};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Default)]
struct Mock {
    posts: AtomicUsize,
    patches: AtomicUsize,
    message: Mutex<Value>,
}
async fn create(State(s): State<Arc<Mock>>, Json(mut body): Json<Value>) -> Response {
    s.posts.fetch_add(1, Ordering::SeqCst);
    body["id"] = json!("100");
    body["channel_id"] = json!("4");
    body["author"] = json!({"id":"99","bot":true});
    *s.message.lock().unwrap() = body;
    StatusCode::BAD_GATEWAY.into_response()
}
async fn edit(State(s): State<Arc<Mock>>, Json(body): Json<Value>) -> Response {
    s.patches.fetch_add(1, Ordering::SeqCst);
    let mut m = s.message.lock().unwrap();
    m["content"] = body["content"].clone();
    m["components"] = body["components"].clone();
    StatusCode::BAD_GATEWAY.into_response()
}
#[tokio::test]
async fn lost_post_and_patch_receipts_are_reconciled_without_resending() {
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (store, _) = common::store(&cfg).await;
    let state = Arc::new(Mock::default());
    let router = Router::new()
        .route(
            "/users/@me",
            get(|| async { Json(json!({"id":"99","bot":true})) }),
        )
        .route(
            "/applications/99/guilds/1/commands",
            put(|| async { Json(json!([])) }),
        )
        .route(
            "/channels/4/messages",
            post(create).get(|State(s): State<Arc<Mock>>| async move {
                Json(json!([s.message.lock().unwrap().clone()]))
            }),
        )
        .route(
            "/channels/4/messages/100",
            get(
                |State(s): State<Arc<Mock>>| async move { Json(s.message.lock().unwrap().clone()) },
            )
            .patch(edit),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let discord = Discord::with_endpoint(
        "test-token".into(),
        format!("http://{}", listener.local_addr().unwrap()),
    )
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    discord.register("99", "1").await.unwrap();
    let delivery = Delivery { store, discord };
    assert!(
        !delivery
            .text("request", "4", "answer", 0, "最初", json!([]))
            .await
            .unwrap()
    );
    assert!(
        delivery
            .text("request", "4", "answer", 0, "最初", json!([]))
            .await
            .unwrap()
    );
    assert_eq!(state.posts.load(Ordering::SeqCst), 1);
    assert!(
        !delivery
            .text("request", "4", "answer", 0, "最初と続き", json!([]))
            .await
            .unwrap()
    );
    assert!(
        delivery
            .text("request", "4", "answer", 0, "最初と続き", json!([]))
            .await
            .unwrap()
    );
    assert_eq!(state.patches.load(Ordering::SeqCst), 1);
    // No prompt or answer content is retained in any SQLite text value.
    delivery
        .store
        .call(false, |c| {
            let mut st = c.prepare("SELECT confirmed_digest,pending_digest FROM deliveries")?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert!(
                rows.iter()
                    .all(|(c, p)| c.as_ref().is_none_or(|s| s.len() == 64) && p.is_none())
            );
            Ok(())
        })
        .await
        .unwrap();
    server.abort();
    server.await.ok();
}

#[tokio::test]
async fn only_explicit_rate_limit_rejection_allows_transport_retry() {
    let count = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route(
            "/limited",
            post(|State(n): State<Arc<AtomicUsize>>| async move {
                if n.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({"retry_after":0.05})),
                    )
                        .into_response()
                } else {
                    Json(json!({"ok":true})).into_response()
                }
            }),
        )
        .with_state(count.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let d = Discord::with_endpoint(
        "test".into(),
        format!("http://{}", listener.local_addr().unwrap()),
    )
    .unwrap();
    let s = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    assert_eq!(
        d.api(
            reqwest::Method::POST,
            "/limited",
            Some(json!({"test":true}))
        )
        .await
        .unwrap()["ok"],
        true
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    s.abort();
    s.await.ok();
}
