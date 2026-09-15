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
    deletes: AtomicUsize,
    message: Mutex<Value>,
}
async fn create(State(s): State<Arc<Mock>>, Json(mut body): Json<Value>) -> Response {
    let count = s.posts.fetch_add(1, Ordering::SeqCst);
    body["id"] = json!((100 + count).to_string());
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
    exercise_delivery("answer").await;
}
#[tokio::test]
async fn resolved_status_is_removed_without_posting_success_message() {
    exercise_delivery("status").await;
}
#[tokio::test]
async fn resolved_resource_notice_is_removed_without_reposting() {
    exercise_delivery("notice").await;
}
async fn exercise_delivery(kind: &str) {
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
            "/channels/4/messages/{message}",
            get(
                |State(s): State<Arc<Mock>>| async move { Json(s.message.lock().unwrap().clone()) },
            )
            .patch(edit)
            .delete(|State(s): State<Arc<Mock>>| async move {
                s.deletes.fetch_add(1, Ordering::SeqCst);
                *s.message.lock().unwrap() = json!({});
                StatusCode::NO_CONTENT
            }),
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
            .text("request", "4", kind, 0, "最初", json!([]))
            .await
            .unwrap()
    );
    assert!(
        delivery
            .text("request", "4", kind, 0, "最初", json!([]))
            .await
            .unwrap()
    );
    assert_eq!(state.posts.load(Ordering::SeqCst), 1);
    assert!(
        !delivery
            .text("request", "4", kind, 0, "最初と続き", json!([]))
            .await
            .unwrap()
    );
    assert!(
        delivery
            .text("request", "4", kind, 0, "最初と続き", json!([]))
            .await
            .unwrap()
    );
    assert_eq!(state.patches.load(Ordering::SeqCst), 1);
    // Final saved output can be shorter than streamed output. Delete only the
    // persisted, verified bot-owned surplus; repeated cleanup must not delete twice.
    if kind == "status" {
        assert!(delivery.clear_status("request", "4").await.unwrap());
    } else if kind == "notice" {
        assert!(delivery.clear_notice("request", "4").await.unwrap());
    } else {
        assert!(delivery.trim_answer("request", "4", 0).await.unwrap());
    }
    if kind == "status" {
        assert!(delivery.clear_status("request", "4").await.unwrap());
    } else if kind == "notice" {
        assert!(delivery.clear_notice("request", "4").await.unwrap());
    } else {
        assert!(delivery.trim_answer("request", "4", 0).await.unwrap());
    }
    assert_eq!(state.deletes.load(Ordering::SeqCst), 1);
    assert_eq!(state.posts.load(Ordering::SeqCst), 1);
    if kind == "status" {
        delivery
            .store
            .call(true, |c| {
                c.execute(
                    "UPDATE deliveries SET kind='status' WHERE state='DELETED'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            !delivery
                .text("request", "4", "status", 0, "新しい確認待ち", json!([]))
                .await
                .unwrap()
        );
        assert!(
            delivery
                .text("request", "4", "status", 0, "新しい確認待ち", json!([]))
                .await
                .unwrap()
        );
        assert_eq!(state.posts.load(Ordering::SeqCst), 2);
        delivery.store.call(false,|c|{assert_eq!(c.query_row("SELECT count(*) FROM deliveries WHERE kind LIKE 'retired-status-%' AND state='DELETED'",[],|r|r.get::<_,i64>(0))?,1);Ok(())}).await.unwrap();
    }

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

#[tokio::test]
async fn successful_answer_has_no_status_or_completion_posts() {
    exercise_final_answer(false).await;
}
#[tokio::test]
async fn final_answer_is_posted_after_approval_and_draft_is_removed() {
    exercise_final_answer(true).await;
}
async fn exercise_final_answer(streamed: bool) {
    use codex_hoshikage_gateway::{
        application::{App, Output},
        proxy::Proxy,
    };
    use std::time::{Duration, Instant};
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let r = received.clone();
    let reads = received.clone();
    let deletes = received.clone();
    let router = Router::new()
        .route(
            "/users/@me",
            get(|| async { Json(json!({"id":"99","bot":true})) }),
        )
        .route(
            "/channels/4/messages/{mid}",
            get(
                move |axum::extract::Path(mid): axum::extract::Path<String>| {
                    let r = reads.clone();
                    async move {
                        Json(
                            r.lock()
                                .unwrap()
                                .iter()
                                .find(|v| v["id"] == mid)
                                .unwrap()
                                .clone(),
                        )
                    }
                },
            )
            .delete(
                move |axum::extract::Path(mid): axum::extract::Path<String>| {
                    let r = deletes.clone();
                    async move {
                        r.lock()
                            .unwrap()
                            .iter_mut()
                            .find(|v| v["id"] == mid)
                            .unwrap()["deleted"] = json!(true);
                        StatusCode::NO_CONTENT
                    }
                },
            ),
        )
        .route(
            "/channels/4/messages",
            post(move |Json(mut body): Json<Value>| {
                let r = r.clone();
                async move {
                    let mut rows = r.lock().unwrap();
                    body["id"] = json!((100 + rows.len()).to_string());
                    body["channel_id"] = json!("4");
                    body["author"] = json!({"id":"99","bot":true});
                    rows.push(body.clone());
                    Json(body)
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (store, _lock) = common::store(&cfg).await;
    let id = common::queued(&store, &cfg, "123").await;
    let rid = id.clone();
    store
        .call(true, move |c| {
            c.execute("UPDATE requests SET state='COMPLETED' WHERE id=?1", [&rid])?;
            c.execute("INSERT INTO output_state VALUES(?1,'VOLATILE')", [rid])?;
            Ok(())
        })
        .await
        .unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("test".into(), endpoint.clone()).unwrap(),
        Proxy::new(endpoint, "test".into()).unwrap(),
    )
    .unwrap();
    app.discord.identify_bot().await.unwrap();
    app.connected.store(true, Ordering::SeqCst);
    app.output.lock().await.insert(
        id.clone(),
        Output {
            thread: "4".into(),
            text: "こんにちは☺️".into(),
            done: !streamed,
            lost: false,
            created: Instant::now(),
            last_progress: Instant::now(),
            retention: Duration::from_secs(60),
        },
    );
    let a = app.clone();
    let task = tokio::spawn(async move { a.delivery_loop().await });
    if streamed {
        tokio::time::timeout(Duration::from_secs(5), async {
            while received.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            app.delivery
                .text("approval_a", "4", "approval", 0, "承認待ち", json!([]))
                .await
                .unwrap()
        );
        let mut cache = app.output.lock().await;
        let output = cache.get_mut(&id).unwrap();
        output.text = "更新しました。実測値は25.7Mbpsです。".into();
        output.done = true;
    }

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rid = id.clone();
            let done = app
                .store
                .call(true, move |c| {
                    Ok(c.query_row(
                        "SELECT state='DELIVERED' FROM output_state WHERE request_id=?1",
                        [rid],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    app.cancel.cancel();
    task.await.unwrap().unwrap();
    let messages = received.lock().unwrap();
    if streamed {
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["deleted"], true);
        assert_eq!(messages[1]["content"], "承認待ち");
        assert_eq!(
            messages[2]["content"],
            "更新しました。実測値は25.7Mbpsです。"
        );
        assert!(messages[1].get("deleted").is_none());
        assert!(messages[2].get("deleted").is_none());
    } else {
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "こんにちは☺️");
        assert_eq!(messages[0]["components"], json!([]));
    }
    server.abort();
}
