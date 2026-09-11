mod common;
use axum::{Json, Router, response::IntoResponse, routing::get};
use codex_hoshikage_gateway::{
    application::App, discord::Discord, domain::RequestState, proxy::Proxy,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
#[tokio::test]
async fn generation_accept_requires_fresh_review_and_keeps_old_requests_quarantined() {
    let generation = Arc::new(Mutex::new("gen_test".to_owned()));
    let g = generation.clone();
    let lookup = generation.clone();
    let router = Router::new()
        .route("/readyz", get(|| async { Json(json!({"status":"ready"})) }))
        .route(
            "/v2/codex/capabilities",
            get(move || {
                let g = g.clone();
                async move {
                    let mut caps = common::caps_v2();
                    caps["recovery_generation"] = json!(g.lock().unwrap().clone());
                    Json(caps)
                }
            }),
        )
        .route(
            "/v2/codex/operations/by-key/{key}",
            get(move || {
                let g = lookup.clone();
                async move {
                    (
                        axum::http::StatusCode::NOT_FOUND,
                        [
                            ("X-Proxy-Instance-Id", "pxy_test".to_owned()),
                            ("X-Proxy-Recovery-Generation", g.lock().unwrap().clone()),
                        ],
                        Json(json!({"error":{"code":"not_found","retry":{"action":"none"}}})),
                    )
                        .into_response()
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&tmp);
    cfg.proxy.base_url = endpoint.clone();
    let (store, _lock) = common::store(&cfg).await;
    let id = common::queued(&store, &cfg, "10").await;
    store.begin_send(id.clone()).await.unwrap();
    store
        .observe(id.clone(), RequestState::Unknown, "lost", false)
        .await
        .unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("credential".into(), endpoint.clone()).unwrap(),
        Proxy::new(endpoint, "key".into()).unwrap(),
    )
    .unwrap();
    app.settings().await.proxy.check().await.unwrap();
    *generation.lock().unwrap() = "gen_restored".into();
    assert!(app.settings().await.proxy.check().await.is_err());
    assert!(
        app.accept_proxy_recovery("missing", "test", true)
            .await
            .is_err()
    );
    let review = app.inspect_proxy_recovery().await.unwrap();
    let token = review["review_token"].as_str().unwrap();
    assert_eq!(review["unavailable"], 1);
    assert!(
        app.accept_proxy_recovery(token, "test", false)
            .await
            .is_err()
    );
    // A second generation invalidates the first review.
    *generation.lock().unwrap() = "gen_again".into();
    assert!(
        app.accept_proxy_recovery(token, "test", true)
            .await
            .is_err()
    );
    let review = app.inspect_proxy_recovery().await.unwrap();
    let token = review["review_token"].as_str().unwrap();
    app.accept_proxy_recovery(token, "verified official restore", true)
        .await
        .unwrap();
    let request = app.store.request(&id).await.unwrap();
    assert_eq!(request.state, RequestState::Unknown);
    assert!(!request.dispatch_eligible);
    assert!(app.store.conversation("4").await.unwrap().paused);
    assert!(app.store.active("4").await.unwrap().is_some());
    assert!(
        app.accept_proxy_recovery(token, "replay", true)
            .await
            .is_err()
    );
    app.settings().await.proxy.check().await.unwrap();
    server.abort();
}
