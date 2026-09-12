mod common;
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use codex_hoshikage_gateway::{
    application::App,
    discord::{Discord, Incoming},
    domain,
    proxy::Proxy,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn message() -> Value {
    json!({"id":"10","channel_id":"4","guild_id":"1","author":{"id":"2","bot":false},"content":"星影のテストです","attachments":[]})
}
fn reply(v: Value) -> impl IntoResponse {
    (
        [
            ("X-Proxy-Instance-Id", "pxy_test"),
            ("X-Proxy-Recovery-Generation", "gen_test"),
        ],
        Json(v),
    )
}
#[tokio::test]
async fn unregistered_channel_v2_accepts_once_and_recovers_saved_output() {
    accept_and_recover(false).await;
}
#[tokio::test]
async fn existing_place_accepts_normal_post_without_new_command() {
    accept_and_recover(true).await;
}
async fn accept_and_recover(stale_local_conversation: bool) {
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let output=serde_json::to_vec(&json!({"response_id":"resp_test","model":"chatgpt/test","output":[{"type":"message","content":[{"type":"output_text","text":"検証できました。"}]}]})).unwrap();
    let output_meta = json!({"state":"ready","size_bytes":output.len(),"sha256":domain::digest(&output),"expires_at":"2026-09-18T09:00:00Z"});
    let router=Router::new()
        .route("/readyz",get(||async{Json(json!({"status":"ready"}))}))
        .route("/v2/codex/capabilities",get(||async{Json(common::caps_v2())}))
        .route("/channels/4",get(||async{Json(json!({"id":"4","guild_id":"1","type":0}))}))
        .route("/channels/4/messages/10",get(||async{Json(message())}))
        .route("/v2/codex/conversations",post(|h:HeaderMap,Json(v):Json<Value>|async move{
            assert_eq!(h["X-Proxy-Instance-Id"],"pxy_test");assert_eq!(v["workspace"]["mode"],"automatic");
            (StatusCode::ACCEPTED,reply(json!({"operation_id":"op_c","state":"accepted","resource":{"type":"conversation","id":"conv_test"}})))
        }))
        .route("/v2/codex/conversations/conv_test",get(||async{reply(json!({"conversation_id":"conv_test","workspace_id":"ws_test","state":"ready"}))}))
        .route("/v2/codex/conversations/conv_test/responses",post(move|h:HeaderMap,Json(v):Json<Value>|{let counter=counter.clone();async move{
            assert!(h.contains_key("Idempotency-Key"));assert_eq!(v["model"],"chatgpt/test");assert!(v.get("stream").is_none());assert!(v["metadata"].get("codex.cwd").is_none());assert_eq!(v["input"][0]["content"][0]["text"],"星影のテストです");
            counter.fetch_add(1,Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            (StatusCode::ACCEPTED,reply(json!({"operation_id":"op_r","state":"accepted","resource":{"type":"response","id":"resp_test"}})))
        }}))
        .route("/v2/codex/responses/resp_test",get(move||{let meta=output_meta.clone();async move{reply(json!({"response_id":"resp_test","conversation_id":"conv_test","workspace_id":"ws_test","phase":"finished","execution_status":"completed","output":meta}))}}))
        .route("/v2/codex/leases",post(||async{reply(json!({"operation_id":"op_l","lease_id":"lease_test","state":"active"}))}))
        .route("/v2/codex/leases/lease_test",get(||async{reply(json!({"lease_id":"lease_test","state":"active","resource":{"type":"response_output","id":"resp_test"},"hold_until":"2026-09-18T09:00:00Z"}))}))
        .route("/v2/codex/responses/resp_test/output",get(move||{let bytes=output.clone();async move{([("X-Proxy-Instance-Id","pxy_test"),("X-Proxy-Recovery-Generation","gen_test")],bytes)}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&tmp);
    cfg.proxy.base_url = endpoint.clone();
    cfg.default_model = Some("chatgpt/test".into());
    cfg.projects.clear();
    codex_hoshikage_gateway::storage::initialize(&cfg).unwrap();
    let (store, _) = codex_hoshikage_gateway::storage::Store::open(&cfg).unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("test-token".into(), endpoint.clone()).unwrap(),
        Proxy::new(endpoint, "test-key".into()).unwrap(),
    )
    .unwrap();
    if stale_local_conversation {
        assert!(app.ensure_channel_conversation("4").await.unwrap());
        app.store.call(true, |c| {
            c.execute("UPDATE conversations SET continuation='NEW_CONVERSATION_REQUIRED',paused=1,last_response_id='old-response',proxy_thread_id='old-thread' WHERE thread_id='4'", [])?;
            Ok(())
        }).await.unwrap();
    }
    app.settings().await.proxy.check().await.unwrap();
    app.connected.store(true, Ordering::SeqCst);
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let mut jobs = tokio::task::JoinSet::new();
    let a = app.clone();
    jobs.spawn(async move { a.admit_loop(rx).await });
    let a = app.clone();
    jobs.spawn(async move { a.scheduler_loop().await });
    let a = app.clone();
    jobs.spawn(async move { a.monitor_loop().await });
    let a = app.clone();
    jobs.spawn(async move { a.resource_loop().await });
    tx.send(Incoming::Message(message())).await.unwrap();
    tx.send(Incoming::Message(message())).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if app
                .output
                .lock()
                .await
                .values()
                .any(|o| o.text == "検証できました。")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    app.store
        .call(false, |c| {
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM request_events WHERE new_state='UNKNOWN'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        app.store.conversation("4").await.unwrap().continuation,
        "READY"
    );
    app.cancel.cancel();
    while let Some(r) = jobs.join_next().await {
        r.unwrap().unwrap();
    }
    server.abort();
}
