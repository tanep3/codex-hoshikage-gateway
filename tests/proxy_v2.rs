mod common;
use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use codex_hoshikage_gateway::{
    domain::{self, RequestState},
    proxy::Proxy,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
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
async fn conversation_receipt_loss_is_resolved_without_second_create() {
    let count = Arc::new(AtomicUsize::new(0));
    let n = count.clone();
    let router=Router::new().route("/v2/codex/conversations",post(move||{let n=n.clone();async move{n.fetch_add(1,Ordering::SeqCst);StatusCode::SERVICE_UNAVAILABLE}}))
        .route("/v2/codex/operations/by-key/{key}",get(||async{reply(json!({"operation_id":"op_c","state":"succeeded","resource":{"type":"conversation","id":"conv_test"}}))}))
        .route("/v2/codex/conversations/conv_test",get(||async{reply(json!({"conversation_id":"conv_test","workspace_id":"ws_test","state":"ready"}))}));
    let (p, server) = serve(router).await;
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (store, _) = common::store(&cfg).await;
    let p = p.with_store(store.clone());
    p.bind_v2(&common::caps_v2()).await.unwrap();
    assert!(p.ensure_conversation_v2(&store, "4").await.is_err());
    assert_eq!(
        p.ensure_conversation_v2(&store, "4").await.unwrap(),
        "conv_test"
    );
    assert_eq!(
        p.ensure_conversation_v2(&store, "4").await.unwrap(),
        "conv_test"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    server.abort();
}
#[tokio::test]
async fn generation_change_is_durably_blocked_across_new_client() {
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (store, _) = common::store(&cfg).await;
    let p = Proxy::new("http://127.0.0.1:4040".into(), "key".into())
        .unwrap()
        .with_store(store.clone());
    p.bind_v2(&common::caps_v2()).await.unwrap();
    let mut changed = common::caps_v2();
    changed["recovery_generation"] = json!("new_generation");
    assert!(p.bind_v2(&changed).await.is_err());
    let restarted = Proxy::new("http://127.0.0.1:4040".into(), "key".into())
        .unwrap()
        .with_store(store);
    assert!(restarted.bind_v2(&changed).await.is_err());
    assert!(restarted.bind_v2(&common::caps_v2()).await.is_err());
}
#[tokio::test]
async fn stop_uses_original_key_before_turn_and_duplicate_only_polls() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let input = seen.clone();
    let router=Router::new().route("/v2/codex/stops",post(move|h:HeaderMap,Json(v):Json<Value>|{let input=input.clone();async move{
        assert!(h.contains_key("Idempotency-Key"));input.lock().unwrap().push(v);
        reply(json!({"operation_id":"op_s","stop_id":"stop_test","stop_status":"cancelled_before_start"}))
    }}))
        .route("/v2/codex/stops/stop_test",get(||async{reply(json!({"stop_id":"stop_test","stop_status":"cancelled_before_start","execution_status":"not_started"}))}))
        .route("/v2/codex/operations/by-key/{key}",get(|Path(_key):Path<String>|async{reply(json!({"operation_id":"op_s","state":"succeeded","resource":{"type":"stop","id":"stop_test"}}))}));
    let (p, server) = serve(router).await;
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (store, _) = common::store(&cfg).await;
    store.call(true,|c|{c.execute("INSERT INTO proxy_conversations(thread_id,request_key,request_json,conversation_id,workspace_id,state) VALUES('4','conversation-test','{}','conv_test','ws_test','READY')",[])?;Ok(())}).await.unwrap();
    p.bind_v2(&common::caps_v2()).await.unwrap();
    let id = common::queued(&store, &cfg, "10").await;
    let request = store.begin_send(id).await.unwrap();
    assert!(request.turn_id.is_none());
    for _ in 0..2 {
        assert_eq!(
            p.stop_v2(&store, &request).await.unwrap()["stop_status"],
            "cancelled_before_start"
        );
    }
    let values = seen.lock().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(
        values[0]["target"]["request_key"],
        request.client_request_id.unwrap()
    );
    assert_eq!(values[0]["target"]["conversation_id"], "conv_test");
    server.abort();
}
#[tokio::test]
async fn saved_content_requires_same_generation_size_and_hash() {
    let router = Router::new().route(
        "/v2/codex/artifacts/art/content",
        get(|| async {
            (
                [
                    ("X-Proxy-Instance-Id", "pxy_test"),
                    ("X-Proxy-Recovery-Generation", "gen_test"),
                ],
                b"fixed bytes".to_vec(),
            )
        }),
    );
    let (p, server) = serve(router).await;
    p.bind_v2(&common::caps_v2()).await.unwrap();
    let path = "/v2/codex/artifacts/art/content";
    assert_eq!(
        p.content_v2(path, 11, &domain::digest(b"fixed bytes"), 20)
            .await
            .unwrap(),
        b"fixed bytes"
    );
    assert!(
        p.content_v2(path, 11, &domain::digest(b"other bytes"), 20)
            .await
            .is_err()
    );
    assert!(
        p.content_v2(path, 11, &domain::digest(b"fixed bytes"), 10)
            .await
            .is_err()
    );
    server.abort();
}
#[tokio::test]
async fn first_confirmed_failure_can_continue_without_successful_response() {
    let router=Router::new().route("/v2/codex/operations/by-key/{key}",get(||async{reply(json!({"state":"succeeded","resource":{"type":"response","id":"resp_test"}}))}))
        .route("/v2/codex/responses/resp_test",get(||async{reply(json!({"response_id":"resp_test","conversation_id":"conv_test","workspace_id":"ws_test","phase":"finished","execution_status":"failed"}))}))
        .route("/v2/codex/conversations/conv_test",get(||async{reply(json!({"conversation_id":"conv_test","workspace_id":"ws_test","state":"ready"}))}));
    let (p, server) = serve(router).await;
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (store, _) = common::store(&cfg).await;
    store.call(true,|c|{c.execute("INSERT INTO proxy_conversations(thread_id,request_key,request_json,conversation_id,workspace_id,state) VALUES('4','conversation-test','{}','conv_test','ws_test','READY')",[])?;Ok(())}).await.unwrap();
    p.bind_v2(&common::caps_v2()).await.unwrap();
    let id = common::queued(&store, &cfg, "10").await;
    store.begin_send(id.clone()).await.unwrap();
    assert_eq!(
        p.reconcile(&store, &id).await.unwrap(),
        RequestState::Failed
    );
    assert_eq!(store.conversation("4").await.unwrap().continuation, "READY");
    let next = common::queued(&store, &cfg, "11").await;
    assert!(store.begin_send(next).await.is_ok());
    server.abort();
}
async fn serve(router: Router) -> (Proxy, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = Proxy::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "test-key".into(),
    )
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (p, server)
}

#[tokio::test]
async fn schema_one_upgrade_preserves_identity_and_quarantines_old_queue() {
    use codex_hoshikage_gateway::storage::{self, Store};
    let tmp = tempfile::tempdir().unwrap();
    let cfg = common::config(&tmp);
    storage::initialize(&cfg).unwrap();
    let (store, done) = Store::open(&cfg).unwrap();
    store
        .add_conversation("4".into(), cfg.projects[0].id.clone())
        .await
        .unwrap();
    let id = common::queued(&store, &cfg, "10").await;
    drop(store);
    let _ = done.await;
    let c = rusqlite::Connection::open(storage::db_path(&cfg)).unwrap();
    c.execute_batch("DELETE FROM schema_migrations WHERE version=15;ALTER TABLE operations DROP COLUMN desired_reasoning_effort;ALTER TABLE requests DROP COLUMN reasoning_effort_revision;ALTER TABLE requests DROP COLUMN reasoning_effort;ALTER TABLE conversations DROP COLUMN effort_revision;ALTER TABLE conversations DROP COLUMN effective_reasoning_effort;ALTER TABLE conversations DROP COLUMN selected_reasoning_effort;ALTER TABLE conversations DROP COLUMN latest_effort_sequence;ALTER TABLE projects DROP COLUMN default_reasoning_effort;DROP TABLE direct_artifacts;DELETE FROM schema_migrations WHERE version=14;DROP TABLE direct_generated_images;DROP TABLE direct_image_inventories;DELETE FROM schema_migrations WHERE version=13;DROP TABLE runtime_mode;DELETE FROM schema_migrations WHERE version=12;DROP TRIGGER direct_interaction_no_rewind;DROP TABLE direct_interactions;DELETE FROM schema_migrations WHERE version=11;DROP TRIGGER direct_dispatch_no_rewind;DROP TABLE direct_answers;DROP TABLE direct_dispatches;DROP TABLE direct_conversations;DELETE FROM schema_migrations WHERE version=10;DROP TABLE mcp_v06_decisions;DROP TABLE mcp_v06_parts;DROP TABLE mcp_v06_pages;DROP TABLE mcp_v06_views;DROP TABLE mcp_v06_runs; DROP TABLE mcp_inline_views; DROP TABLE mcp_inline_runs; DROP TABLE mcp_grant_revokes; DROP TABLE mcp_grant_records; DROP TABLE mcp_detail_views; DROP TABLE mcp_run_context; ALTER TABLE requests DROP COLUMN interaction_scan_done; DROP TABLE mcp_interactions; DROP TABLE artifact_delivery_claims; ALTER TABLE resource_deliveries DROP COLUMN image_request_id; ALTER TABLE resource_deliveries DROP COLUMN image_ordinal; DROP TABLE generated_image_items; DROP TABLE generated_image_watches; DROP TABLE recovery_reviews; DROP TABLE selection_menus; DROP TABLE resource_deliveries; DROP TABLE remote_operations; DROP TABLE proxy_conversations; DROP TABLE proxy_binding; DELETE FROM schema_migrations WHERE version>=2; UPDATE schema_meta SET schema_version=1;").unwrap();
    drop(c);
    let (store, _) = Store::open(&cfg).unwrap();
    assert!(!store.request(&id).await.unwrap().dispatch_eligible);
    assert!(store.conversation("4").await.unwrap().paused);
    assert_eq!(
        store.conversation("4").await.unwrap().continuation,
        "NEW_CONVERSATION_REQUIRED"
    );
    assert!(store.candidates().await.unwrap().is_empty());
    assert_eq!(
        storage::validate_database(&store.path).unwrap().0,
        storage::SCHEMA
    );
}

#[tokio::test]
async fn partial_download_resumes_only_with_matching_range_and_etag() {
    use axum::body::{Body, Bytes};
    use futures_util::stream;
    let calls = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    let router = Router::new().route(
        "/v2/codex/artifacts/art_a/content",
        get(move |headers: HeaderMap| {
            let n = n.clone();
            async move {
                let count = n.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    let body = stream::unfold(0, |i| async move {
                        match i {
                            0 => Some((Ok::<_, std::io::Error>(Bytes::from_static(b"abc")), 1)),
                            1 => {
                                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                                Some((Err(std::io::Error::other("cut")), 2))
                            }
                            _ => None,
                        }
                    });
                    axum::http::Response::builder()
                        .header("X-Proxy-Instance-Id", "pxy_test")
                        .header("X-Proxy-Recovery-Generation", "gen_test")
                        .header("content-length", "6")
                        .header("etag", "\"fixed\"")
                        .body(Body::from_stream(body))
                        .unwrap()
                } else {
                    assert_eq!(headers["range"], "bytes=3-");
                    assert_eq!(headers["if-range"], "\"fixed\"");
                    axum::http::Response::builder()
                        .status(206)
                        .header("X-Proxy-Instance-Id", "pxy_test")
                        .header("X-Proxy-Recovery-Generation", "gen_test")
                        .header("content-length", "3")
                        .header("content-range", "bytes 3-5/6")
                        .header("etag", "\"fixed\"")
                        .body(Body::from("def"))
                        .unwrap()
                }
            }
        }),
    );
    let (p, server) = serve(router).await;
    p.bind_v2(&common::caps_v2()).await.unwrap();
    assert_eq!(
        p.content_v2(
            "/v2/codex/artifacts/art_a/content",
            6,
            &domain::digest(b"abcdef"),
            10
        )
        .await
        .unwrap(),
        b"abcdef"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn explicit_capture_capacity_rejection_allows_only_same_key_retry() {
    let count = Arc::new(AtomicUsize::new(0));
    let n = count.clone();
    let router=Router::new().route("/v2/codex/conversations/conv_a/artifacts",post(move|headers:HeaderMap|{let n=n.clone();async move{
        assert_eq!(headers["Idempotency-Key"],"capture-test");
        if n.fetch_add(1,Ordering::SeqCst)==0{(StatusCode::TOO_MANY_REQUESTS,reply(json!({"error":{"code":"capture_capacity_busy","retry":{"action":"repeat_same_request"}}}))).into_response()}
        else{reply(json!({"operation_id":"op_a","state":"succeeded","resource":{"type":"artifact","id":"art_a"}})).into_response()}
    }}));
    let (p, server) = serve(router).await;
    let tmp = tempfile::tempdir().unwrap();
    let cfg = common::config(&tmp);
    let (store, _) = common::store(&cfg).await;
    p.bind_v2(&common::caps_v2()).await.unwrap();
    assert!(
        p.metadata_operation(
            &store,
            "capture-test",
            "artifact.create",
            "/v2/codex/conversations/conv_a/artifacts",
            json!({"path":"report.txt"})
        )
        .await
        .is_err()
    );
    p.metadata_operation(
        &store,
        "capture-test",
        "artifact.create",
        "/v2/codex/conversations/conv_a/artifacts",
        json!({"path":"report.txt"}),
    )
    .await
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
    server.abort();
}
