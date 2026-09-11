mod common;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use codex_hoshikage_gateway::{
    domain::RequestState as S,
    proxy::{Proxy, SseDecoder},
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Default)]
struct Mock {
    posts: AtomicUsize,
    controls: AtomicUsize,
    status: Mutex<String>,
    key: Mutex<String>,
    seen: Mutex<Option<Value>>,
    cap_bad: std::sync::atomic::AtomicBool,
}
fn caps() -> Value {
    json!({"contract_version":"1.0","responses":true,"streaming":true,"conversation_resume":true,"conversation_model_change":true,"identity_on_start":true,"request_lookup":true,"persistent_turn_status":true,"turn_status":true,"turn_events":true,"turn_interrupt":true,"turn_steer":true,"interactive_approval":true,"auto_approval_suppression":true,"event_reconnect":true,"output_retrieval":false,"limits":{"auth_scope":"shared_operator","continuation":"successful_response_only","disconnect_interrupts":true,"event_history_replay":false,"event_reconnect":"snapshot_only","steer_idempotency":false,"model_change_scope":"same_provider"}})
}
async fn start(
    State(s): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    s.posts.fetch_add(1, Ordering::SeqCst);
    assert_eq!(headers["authorization"], "Bearer test-key");
    assert_eq!(headers["idempotency-key"], s.key.lock().unwrap().as_str());
    *s.seen.lock().unwrap() = Some(body);
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}
async fn request(State(s): State<Arc<Mock>>, Path(key): Path<String>) -> Response {
    if s.status.lock().unwrap().as_str() == "missing" {
        return StatusCode::NOT_FOUND.into_response();
    }
    assert_eq!(key, *s.key.lock().unwrap());
    Json(json!({"client_request_id":key,"phase":"started","response_id":"resp_one","thread_id":"thread_one","turn_id":"turn_one"})).into_response()
}
async fn status(State(s): State<Arc<Mock>>) -> Json<Value> {
    Json(
        json!({"response_id":"resp_one","thread_id":"thread_one","turn_id":"turn_one","status":*s.status.lock().unwrap(),"pending_approvals":[]}),
    )
}
async fn server() -> (Proxy, Arc<Mock>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(Mock::default());
    let router=Router::new().route("/readyz",get(||async{Json(json!({"status":"ready"}))})).route("/v1/codex/capabilities",get(|State(s):State<Arc<Mock>>|async move{let mut c=caps();if s.cap_bad.load(Ordering::SeqCst){c["turn_interrupt"]=json!(false);}Json(c)})).route("/v1/responses",post(start)).route("/v1/codex/requests/{key}",get(request)).route("/v1/codex/turns/turn_one/status",get(status)).route("/v1/codex/responses/resp_one",get(||async{Json(json!({"response_id":"resp_one","thread_id":"thread_one","turn_id":"turn_one","continuable":true}))})).route("/v1/codex/turns/turn_one/interrupt",post(|State(s):State<Arc<Mock>>|async move{s.controls.fetch_add(1,Ordering::SeqCst);(StatusCode::ACCEPTED,Json(json!({"status":"accepted"})))})).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = Proxy::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "test-key".into(),
    )
    .unwrap();
    let job = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (proxy, state, job)
}
#[tokio::test]
async fn ambiguous_503_and_404_never_create_a_second_post() {
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (store, _lock) = common::store(&cfg).await;
    let id = common::queued(&store, &cfg, "10").await;
    let r = store.begin_send(id.clone()).await.unwrap();
    let (p, m, server) = server().await;
    *m.key.lock().unwrap() = r.client_request_id.clone().unwrap();
    *m.status.lock().unwrap() = "missing".into();
    p.check().await.unwrap();
    assert!(
        p.start(&r, json!([{ "role":"user","content":"test"}]), "/workspace")
            .await
            .is_err()
    );
    for _ in 0..3 {
        assert_eq!(p.reconcile(&store, &id).await.unwrap(), S::Unknown);
        assert!(store.candidates().await.unwrap().is_empty());
    }
    assert_eq!(m.posts.load(Ordering::SeqCst), 1);
    let body = m.seen.lock().unwrap().clone().unwrap();
    assert_eq!(body["metadata"]["codex.auto_approve_workspace"], "false");
    assert_eq!(body["metadata"]["codex.approval_capability"], "interactive");
    server.abort();
    server.await.ok();
}
#[tokio::test]
async fn current_status_resolves_unknown_without_new_generation() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    let (store, _lock) = common::store(&c).await;
    let id = common::queued(&store, &c, "10").await;
    let r = store.begin_send(id.clone()).await.unwrap();
    store
        .observe(id.clone(), S::Unknown, "crash", false)
        .await
        .unwrap();
    let (p, m, server) = server().await;
    *m.key.lock().unwrap() = r.client_request_id.unwrap();
    *m.status.lock().unwrap() = "inProgress".into();
    assert_eq!(p.reconcile(&store, &id).await.unwrap(), S::Running);
    *m.status.lock().unwrap() = "completed".into();
    assert_eq!(p.reconcile(&store, &id).await.unwrap(), S::Completed);
    assert_eq!(
        store
            .conversation("4")
            .await
            .unwrap()
            .last_response_id
            .as_deref(),
        Some("resp_one")
    );
    assert_eq!(m.posts.load(Ordering::SeqCst), 0);
    server.abort();
    server.await.ok();
}
#[tokio::test]
async fn capability_loss_invalidates_previously_issued_ticket() {
    let (p, m, s) = server().await;
    let ticket = p.check().await.unwrap();
    assert!(p.valid_ticket(&ticket));
    m.cap_bad.store(true, Ordering::SeqCst);
    assert!(p.check().await.is_err());
    assert!(!p.valid_ticket(&ticket));
    s.abort();
    s.await.ok();
}
#[tokio::test]
async fn controls_are_available_with_two_persistent_execution_holds() {
    let t = tempfile::tempdir().unwrap();
    let mut c = common::config(&t);
    let mut p2 = c.projects[0].clone();
    p2.id = "00000000-0000-4000-8000-000000000002".into();
    p2.channel_id = "5".into();
    p2.cwd = t.path().join("work2");
    std::fs::create_dir(&p2.cwd).unwrap();
    c.projects.push(p2.clone());
    let (store, _lock) = common::store(&c).await;
    let a = common::queued(&store, &c, "10").await;
    store.begin_send(a).await.unwrap();
    store.add_conversation("6".into(), p2.id).await.unwrap();
    let b = store
        .reserve("11".into(), "6".into(), "meta".into(), c.limits.clone())
        .await
        .unwrap()
        .unwrap();
    store
        .finalize(b.clone(), "meta".into(), "input".into(), vec![])
        .await
        .unwrap();
    store.begin_send(b).await.unwrap();
    let (p, m, s) = server().await;
    let control = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        p.control("/v1/codex/turns/turn_one/interrupt", None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(control["status"], "accepted");
    assert_eq!(m.controls.load(Ordering::SeqCst), 1);
    assert_eq!(m.posts.load(Ordering::SeqCst), 0);
    s.abort();
    s.await.ok();
}
#[test]
fn sse_handles_utf8_split_and_mixed_frame_endings() {
    let input = "event: response.output_text.delta\r\ndata: {\"delta\":\"星影\"}\r\n\r\nevent: done\ndata: {}\n\n";
    for split in 0..input.len() {
        let mut d = SseDecoder::default();
        let mut out = d.feed(&input.as_bytes()[..split]).unwrap();
        out.extend(d.feed(&input.as_bytes()[split..]).unwrap());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].event, "response.output_text.delta");
        assert_eq!(
            serde_json::from_str::<Value>(&out[0].data).unwrap()["delta"],
            "星影"
        );
    }
}

#[tokio::test]
async fn dispatch_permit_is_bound_to_request_and_configuration_revision() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    let (store, _) = common::store(&c).await;
    let id = common::queued(&store, &c, "10").await;
    let (p, _, server) = server().await;
    let wrong = p.authorize("different_request".into(), 1).await.unwrap();
    assert!(
        store
            .begin_send_authorized(id.clone(), wrong)
            .await
            .is_err()
    );
    assert_eq!(store.request(&id).await.unwrap().state, S::Queued);
    let old = p.authorize(id.clone(), 1).await.unwrap();
    store
        .call(true, |c| {
            c.execute("UPDATE schema_meta SET config_revision=2", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(store.begin_send_authorized(id.clone(), old).await.is_err());
    assert_eq!(store.request(&id).await.unwrap().state, S::Queued);
    let good = p.authorize(id.clone(), 2).await.unwrap();
    store.begin_send_authorized(id, good).await.unwrap();
    server.abort();
    server.await.ok();
}
