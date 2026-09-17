mod common;
use axum::{
    Json, Router,
    extract::Query,
    response::IntoResponse,
    routing::{get, patch, post},
};
use codex_hoshikage_gateway::{
    application::App,
    discord::{Discord, Incoming},
    proxy::Proxy,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
fn bound(v: Value) -> impl IntoResponse {
    (
        [
            ("X-Proxy-Instance-Id", "pxy_test"),
            ("X-Proxy-Recovery-Generation", "gen_test"),
        ],
        Json(v),
    )
}

#[tokio::test]
async fn workspace_paging_selection_and_forum_creation_through_discord_control() {
    let replies = Arc::new(Mutex::new(Vec::<String>::new()));
    let menus = Arc::new(Mutex::new(Vec::<Value>::new()));
    let r = replies.clone();
    let m = menus.clone();
    let router=Router::new()
        .route("/readyz",get(||async{Json(json!({"status":"ready"}))}))
        .route("/v2/codex/capabilities",get(||async{Json(common::caps_v2())}))
        .route("/channels/4",get(||async{Json(json!({"id":"4","type":11,"parent_id":"3","guild_id":"1"}))}))
        .route("/channels/3",get(||async{Json(json!({"id":"3","type":15,"guild_id":"1"}))}))
        .route("/channels/4/messages",post(move|Json(body):Json<Value>|{let m=m.clone();async move{m.lock().unwrap().push(body);Json(json!({"id":"500","channel_id":"4"}))}}))
        .route("/channels/3/threads",post(|Json(body):Json<Value>|async move{assert_eq!(body["name"],"次の会話");assert!(body["message"]["content"].is_string());Json(json!({"id":"6","parent_id":"3","guild_id":"1","type":11}))}))
        .route("/interactions/{id}/test-token/callback",post(||async{Json(json!({}))}))
        .route("/webhooks/9/test-token/messages/@original",patch(move|Json(body):Json<Value>|{let r=r.clone();async move{r.lock().unwrap().push(body["content"].as_str().unwrap().to_owned());Json(json!({}))}}))
        .route("/v2/codex/workspaces",get(|Query(q):Query<HashMap<String,String>>|async move{
            assert_eq!(q.get("limit").map(String::as_str),Some("25"));assert_eq!(q.get("selectable").map(String::as_str),Some("true"));
            if let Some(cursor)=q.get("cursor"){assert_eq!(cursor,"opaque /+?=&");bound(json!({"data":[{"workspace_id":"ws_second","display_name":"二ページ目","state":"ready"}],"next_cursor":null}))}
            else{bound(json!({"data":[{"workspace_id":"ws_first","display_name":"一ページ目","state":"ready"}],"next_cursor":"opaque /+?=&"}))}
        }))
        .route("/v2/codex/workspaces/ws_second",get(||async{bound(json!({"workspace_id":"ws_second","state":"ready"}))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&tmp);
    cfg.proxy.base_url = endpoint.clone();
    let (store, _lock) = common::store(&cfg).await;
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("fake-credential".into(), endpoint.clone()).unwrap(),
        Proxy::new(endpoint, "fake-key".into()).unwrap(),
    )
    .unwrap();
    app.settings().await.proxy.check().await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let a = app.clone();
    let job = tokio::spawn(async move { a.control_loop(rx).await });
    let send = |id: u64, data: Value| {
        Incoming::Interaction(
            json!({"id":id.to_string(),"guild_id":"1","channel_id":"4","application_id":"9","token":"test-token","member":{"user":{"id":"2"}},"data":data}),
        )
    };
    tx.send(send(100, json!({"name":"workspace"})))
        .await
        .unwrap();
    wait(&replies, 1).await;
    assert!(replies.lock().unwrap()[0].contains("選択画面"));
    let next = menus.lock().unwrap()[0]["components"][1]["components"][0]["custom_id"]
        .as_str()
        .unwrap()
        .to_owned();
    tx.send(send(101, json!({"custom_id":next}))).await.unwrap();
    wait(&replies, 2).await;
    let choice = menus.lock().unwrap()[1]["components"][0]["components"][0]["custom_id"]
        .as_str()
        .unwrap()
        .to_owned();
    tx.send(send(102, json!({"custom_id":choice,"values":["0"]})))
        .await
        .unwrap();
    wait(&replies, 3).await;
    assert!(replies.lock().unwrap()[2].contains("共有ワークを選びました"));
    let selected: String = app
        .store
        .call(false, |c| {
            Ok(c.query_row(
                "SELECT request_json FROM proxy_conversations WHERE thread_id='4'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&selected).unwrap()["workspace"]["workspace_id"],
        "ws_second"
    );
    tx.send(send(103, json!({"custom_id":choice,"values":["0"]})))
        .await
        .unwrap();
    wait(&replies, 4).await;
    assert!(replies.lock().unwrap()[3].contains("すでに確定"));
    tx.send(send(
        104,
        json!({"name":"new","options":[{"name":"title","value":"次の会話"}]}),
    ))
    .await
    .unwrap();
    wait(&replies, 5).await;
    assert!(replies.lock().unwrap()[4].contains("<#6>"));
    assert!(app.store.conversation("6").await.is_ok());
    let queued_first = common::queued(&app.store, &app.settings().await.cfg, "200").await;
    let queued_latest = common::queued(&app.store, &app.settings().await.cfg, "201").await;
    tx.send(send(105, json!({"name":"cancel"}))).await.unwrap();
    wait(&replies, 6).await;
    assert!(replies.lock().unwrap()[5].contains("待機中の依頼を1件取り消しました"));
    assert!(replies.lock().unwrap()[5].contains("https://discord.com/channels/1/4/201"));
    assert_eq!(
        app.store.request(&queued_latest).await.unwrap().state,
        codex_hoshikage_gateway::domain::RequestState::Cancelled
    );
    tx.send(send(105, json!({"name":"cancel"}))).await.unwrap();
    wait(&replies, 7).await;
    assert_eq!(
        app.store.request(&queued_first).await.unwrap().state,
        codex_hoshikage_gateway::domain::RequestState::Queued
    );
    assert!(!app.store.conversation("4").await.unwrap().paused);
    app.cancel.cancel();
    job.await.unwrap().unwrap();
    server.abort();
}
async fn wait(replies: &Arc<Mutex<Vec<String>>>, n: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while replies.lock().unwrap().len() < n {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn uncertain_lease_extension_is_polled_without_second_post() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let posts = Arc::new(AtomicUsize::new(0));
    let resolved = Arc::new(AtomicBool::new(false));
    let p = posts.clone();
    let r = resolved.clone();
    let lookup = resolved.clone();
    let router=Router::new()
        .route("/readyz",get(||async{Json(json!({"status":"ready"}))}))
        .route("/v2/codex/capabilities",get(||async{let mut caps=common::caps_v2();caps["server_time"]=json!("2026-09-11T10:00:00Z");Json(caps)}))
        .route("/v2/codex/leases/lease_a",get(move||{let r=r.clone();async move{bound(json!({"lease_id":"lease_a","resource":{"type":"artifact","id":"art_a"},"state":"active","hold_until":if r.load(Ordering::SeqCst){"2026-09-11T10:02:00Z"}else{"2026-09-11T10:00:20Z"},"max_hold_until":"2026-09-12T10:00:00Z"}))}}))
        .route("/v2/codex/leases/lease_a/extend",post(move|Json(body):Json<Value>|{let p=p.clone();async move{
            assert_eq!(body["hold_until"],"2026-09-11T10:02:00Z");p.fetch_add(1,Ordering::SeqCst);
            (axum::http::StatusCode::SERVICE_UNAVAILABLE,bound(json!({"error":{"code":"temporarily_unavailable","retry":{"action":"poll_operation"}}})))
        }}))
        .route("/v2/codex/operations/by-key/{key}",get(move||{let r=lookup.clone();async move{r.store(true,Ordering::SeqCst);bound(json!({"state":"succeeded","operation_id":"op_extend","resource":{"type":"lease","id":"lease_a"}}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&tmp);
    cfg.proxy.base_url = endpoint.clone();
    cfg.limits.delivery_retention_secs = 120;
    let (store, _lock) = common::store(&cfg).await;
    store.call(true,|c|{c.execute("INSERT INTO resource_deliveries(id,thread_id,resource_type,resource_id,request_key,lease_id,hold_until,created_at) VALUES('d','4','artifact','art_a','lease-d','lease_a','2026-09-11T10:00:20Z',0)",[])?;Ok(())}).await.unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("fake-credential".into(), endpoint.clone()).unwrap(),
        Proxy::new(endpoint, "fake-key".into()).unwrap(),
    )
    .unwrap();
    app.settings().await.proxy.check().await.unwrap();
    let a = app.clone();
    let first = tokio::spawn(async move { a.retention_loop().await });
    tokio::time::timeout(Duration::from_secs(5),async{loop{let failed:bool=app.store.call(false,|c|Ok(c.query_row("SELECT error_code='retention_unconfirmed' FROM resource_deliveries WHERE id='d'",[],|r|r.get::<_,Option<bool>>(0))?.unwrap_or(false))).await.unwrap();if failed{break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    first.abort();
    let _ = first.await;
    // Restart just this worker to trigger the next sweep immediately.
    let a = app.clone();
    let second = tokio::spawn(async move { a.retention_loop().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let hold: String = app
                .store
                .call(false, |c| {
                    Ok(c.query_row(
                        "SELECT hold_until FROM resource_deliveries WHERE id='d'",
                        [],
                        |r| r.get(0),
                    )?)
                })
                .await
                .unwrap();
            if hold == "2026-09-11T10:02:00Z" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    assert!(resolved.load(Ordering::SeqCst));
    app.cancel.cancel();
    second.await.unwrap().unwrap();
    server.abort();
}

#[tokio::test]
async fn explicit_retry_keeps_resource_identity_and_duplicate_click_does_not_queue_twice() {
    let replies = Arc::new(Mutex::new(Vec::<String>::new()));
    let menus = Arc::new(Mutex::new(Vec::<Value>::new()));
    let r = replies.clone();
    let m = menus.clone();
    let router = Router::new()
        .route("/readyz", get(|| async { Json(json!({"status":"ready"})) }))
        .route(
            "/v2/codex/capabilities",
            get(|| async { Json(common::caps_v2()) }),
        )
        .route(
            "/channels/4",
            get(|| async { Json(json!({"id":"4","type":0,"guild_id":"1"})) }),
        )
        .route(
            "/channels/4/messages",
            post(move |Json(body): Json<Value>| {
                let m = m.clone();
                async move {
                    let mut messages = m.lock().unwrap();
                    let id = (500 + messages.len()).to_string();
                    messages.push(body);
                    Json(json!({"id":id,"channel_id":"4"}))
                }
            }),
        )
        .route(
            "/interactions/{id}/test-token/callback",
            post(|| async { Json(json!({})) }),
        )
        .route(
            "/webhooks/9/test-token/messages/@original",
            patch(move |Json(body): Json<Value>| {
                let r = r.clone();
                async move {
                    r.lock()
                        .unwrap()
                        .push(body["content"].as_str().unwrap().into());
                    Json(json!({}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&tmp);
    cfg.proxy.base_url = endpoint.clone();
    let (store, _) = common::store(&cfg).await;
    store.call(true,|c|{c.execute("INSERT INTO resource_deliveries(id,thread_id,resource_type,resource_id,request_key,state,created_at,sha256,size_bytes) VALUES('old','4','response_output','resp_fixed','lease-old','WAITING',0,'fixed-hash',42)",[])?;Ok(())}).await.unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("credential".into(), endpoint.clone()).unwrap(),
        Proxy::new(endpoint, "key".into()).unwrap(),
    )
    .unwrap();
    app.settings().await.proxy.check().await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let a = app.clone();
    let job = tokio::spawn(async move { a.control_loop(rx).await });
    let event = |id: u64, data: Value| {
        Incoming::Interaction(
            json!({"id":id.to_string(),"guild_id":"1","channel_id":"4","application_id":"9","token":"test-token","member":{"user":{"id":"2"}},"data":data}),
        )
    };
    tx.send(event(100, json!({"name":"retry"}))).await.unwrap();
    wait(&replies, 1).await;
    let choice = menus.lock().unwrap()[0]["components"][0]["components"][0]["custom_id"]
        .as_str()
        .unwrap()
        .to_owned();
    tx.send(event(101, json!({"custom_id":choice,"values":["0"]})))
        .await
        .unwrap();
    wait(&replies, 2).await;
    let confirm = menus.lock().unwrap()[1]["components"][0]["components"][0]["custom_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (id, n) in [(102, 3), (103, 4)] {
        tx.send(event(id, json!({"custom_id":confirm})))
            .await
            .unwrap();
        wait(&replies, n).await;
    }
    assert!(replies.lock().unwrap()[2].contains("再送を受け付けました"));
    assert!(replies.lock().unwrap()[3].contains("すでに別の再送"));
    let records: Vec<(String, String, String)> = app
        .store
        .call(false, |c| {
            let mut st =
                c.prepare("SELECT id,resource_id,state FROM resource_deliveries ORDER BY id")?;
            Ok(st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0],
        ("102".into(), "resp_fixed".into(), "WAITING".into())
    );
    assert_eq!(records[1].2, "SUPERSEDED");
    app.cancel.cancel();
    job.await.unwrap().unwrap();
    server.abort();
}
