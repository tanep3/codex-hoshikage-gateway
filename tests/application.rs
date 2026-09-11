mod common;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use codex_hoshikage_gateway::{
    application::App,
    discord::{Discord, Incoming},
    domain::RequestState as S,
    proxy::Proxy,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn message() -> Value {
    json!({"id":"10","channel_id":"4","guild_id":"1","author":{"id":"2","bot":false},"content":"星影のテストです","attachments":[],"edited_timestamp":null})
}
async fn generate(
    State(count): State<Arc<AtomicUsize>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert!(h.contains_key("idempotency-key"));
    assert_eq!(body["model"], "chatgpt/test");
    assert_eq!(body["input"][0]["content"][0]["text"], "星影のテストです");
    assert_eq!(body["metadata"]["codex.auto_approve_workspace"], "false");
    count.fetch_add(1, Ordering::SeqCst);
    ([("content-type","text/event-stream"),("x-response-id","resp_test"),("x-codex-thread-id","thread_test"),("x-codex-turn-id","turn_test")],"event: response.output_text.delta\ndata: {\"id\":\"resp_test\",\"delta\":\"検証できました。\"}\n\nevent: response.completed\ndata: {\"id\":\"resp_test\"}\n\n").into_response()
}
#[tokio::test]
async fn discord_admission_through_scheduler_stream_and_proxy_reconciliation() {
    run_conversation(false).await;
}

#[tokio::test]
async fn registered_channel_starts_without_new_or_preexisting_conversation() {
    run_conversation(true).await;
}

async fn run_conversation(channel: bool) {
    let count = Arc::new(AtomicUsize::new(0));
    let caps = json!({"contract_version":"1.0","responses":true,"streaming":true,"conversation_resume":true,"conversation_model_change":true,"identity_on_start":true,"request_lookup":true,"persistent_turn_status":true,"turn_status":true,"turn_events":true,"turn_interrupt":true,"turn_steer":true,"interactive_approval":true,"auto_approval_suppression":true,"event_reconnect":true,"output_retrieval":false,"limits":{"auth_scope":"shared_operator","continuation":"successful_response_only","disconnect_interrupts":true,"event_history_replay":false,"event_reconnect":"snapshot_only","steer_idempotency":false,"model_change_scope":"same_provider"}});
    let router=Router::new().route("/readyz",get(||async{Json(json!({"status":"ready"}))})).route("/v1/codex/capabilities",get(move||{let c=caps.clone();async{Json(c)}})).route("/channels/4",get(move||async move{Json(if channel {json!({"id":"4","guild_id":"1","type":0})} else {json!({"id":"4","guild_id":"1","parent_id":"3","type":11,"thread_metadata":{"archived":false,"locked":false}})})})).route("/channels/4/messages/10",get(||async{Json(message())})).route("/v1/responses",post(generate)).route("/v1/codex/requests/{key}",get(|axum::extract::Path(key):axum::extract::Path<String>,State(n):State<Arc<AtomicUsize>>|async move{if n.load(Ordering::SeqCst)==0{return StatusCode::NOT_FOUND.into_response()}Json(json!({"client_request_id":key,"phase":"started","response_id":"resp_test","thread_id":"thread_test","turn_id":"turn_test"})).into_response()})).route("/v1/codex/turns/turn_test/status",get(||async{Json(json!({"response_id":"resp_test","thread_id":"thread_test","turn_id":"turn_test","status":"completed","pending_approvals":[]}))})).route("/v1/codex/responses/resp_test",get(||async{Json(json!({"response_id":"resp_test","thread_id":"thread_test","turn_id":"turn_test","continuable":true}))})).with_state(count.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let t = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&t);
    cfg.proxy.base_url = endpoint.clone();
    if channel {
        cfg.projects[0].channel_id = "4".into();
    }
    let (store, _lock) = common::store(&cfg).await;
    if channel {
        store
            .call(true, |c| {
                c.execute("DELETE FROM conversations", [])?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let proxy = Proxy::new(endpoint.clone(), "test-key".into()).unwrap();
    proxy.check().await.unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("test-token".into(), endpoint).unwrap(),
        proxy,
    )
    .unwrap();
    app.connected.store(true, Ordering::SeqCst);
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let mut jobs = tokio::task::JoinSet::new();
    let a = app.clone();
    jobs.spawn(async move { a.admit_loop(rx).await });
    let a = app.clone();
    jobs.spawn(async move { a.scheduler_loop().await });
    let a = app.clone();
    jobs.spawn(async move { a.sweep_loop().await });
    tx.send(Incoming::Message(message())).await.unwrap();
    tx.send(Incoming::Message(message())).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let done = app
                .store
                .call(true, |c| {
                    Ok(c.query_row(
                        "SELECT count(*) FROM requests WHERE state='COMPLETED'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .await
                .unwrap();
            if done == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        app.output.lock().await.values().next().unwrap().text,
        "検証できました。"
    );
    assert_eq!(
        app.store
            .conversation("4")
            .await
            .unwrap()
            .last_response_id
            .as_deref(),
        Some("resp_test")
    );
    let id = app
        .store
        .call(true, |c| {
            Ok(c.query_row("SELECT id FROM requests", [], |r| r.get::<_, String>(0))?)
        })
        .await
        .unwrap();
    assert_eq!(app.store.request(&id).await.unwrap().state, S::Completed);
    app.store
        .call(true, |c| {
            let (epoch, hash, revision): (i64, String, i64) = c.query_row(
                "SELECT capability_epoch,capability_digest,config_revision FROM requests",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(epoch, 0);
            assert_eq!(hash.len(), 64);
            assert_eq!(revision, 1);
            Ok(())
        })
        .await
        .unwrap();
    app.cancel.cancel();
    while let Some(result) = jobs.join_next().await {
        result.unwrap().unwrap();
    }
    server.abort();
    server.await.ok();
}
