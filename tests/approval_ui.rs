mod common;
use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use codex_hoshikage_gateway::{
    application::App,
    approval_ui::approval_description,
    discord::{Discord, Incoming},
    domain::RequestState,
    proxy::Proxy,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
#[test]
fn approval_card_is_readable_and_bounded_without_internal_json() {
    let v = json!({"details":{"kind":"command","reason":"共通スキルへ反映してよいですか？","command":"cp -a stage/. skills/","threadId":"private-thread","availableDecisions":["accept"]}});
    let text = approval_description(&v);
    assert!(text.contains("目的：共通スキル"));
    assert!(text.contains("cp -a stage/. skills/"));
    assert!(!text.contains("threadId"));
    assert!(!text.contains("private-thread"));
    assert!(!text.contains("availableDecisions"));
    let text = approval_description(
        &json!({"details":{"command":"😀".repeat(3000),"reason":"😀".repeat(1000)}}),
    );
    assert!(text.encode_utf16().count() <= 2000);
    assert!(text.contains("省略"));
}
#[tokio::test]
async fn approval_updates_in_place_and_typing_pauses_until_decision() {
    let approved = Arc::new(AtomicUsize::new(0));
    let typing = Arc::new(AtomicUsize::new(0));
    let cards = Arc::new(Mutex::new(Vec::<Value>::new()));
    let replies = Arc::new(AtomicUsize::new(0));
    let extra_messages = Arc::new(AtomicUsize::new(0));
    let extra = extra_messages.clone();
    let errors = Arc::new(AtomicUsize::new(0));
    let err = errors.clone();
    let (a, b, t, c, p, reply) = (
        approved.clone(),
        approved.clone(),
        typing.clone(),
        cards.clone(),
        cards.clone(),
        replies.clone(),
    );
    let router=Router::new()
      .route("/channels/4",get(||async{Json(json!({"id":"4","guild_id":"1","type":0}))}))
      .route("/channels/4/typing",post(move||{let t=t.clone();async move{t.fetch_add(1,Ordering::SeqCst);StatusCode::NO_CONTENT}}))
      .route("/v1/codex/approvals/approval_a",get(move||{let a=a.clone();async move{Json(json!({"id":"approval_a","state":if a.load(Ordering::SeqCst)>0{"approved"}else{"pending"},"reply_status":if a.load(Ordering::SeqCst)>0{"written"}else{"not_sent"},"available_decisions":["accept","cancel"],"details":{"kind":"command","reason":"スキルを更新します","command":"cp stage skills","threadId":"thread_a","turnId":"turn_a"}}))}}).post(move|Json(v):Json<Value>|{let b=b.clone();async move{assert_eq!(v["expected_thread_id"],"thread_a");assert_eq!(v["expected_turn_id"],"turn_a");assert_eq!(v["decision"],"accept");b.fetch_add(1,Ordering::SeqCst);Json(json!({"reply_status":"written"}))}}))
      .route("/channels/4/messages",post(move|Json(mut v):Json<Value>|{let c=c.clone();async move{v["id"]=json!("100");v["channel_id"]=json!("4");c.lock().unwrap().push(v.clone());Json(v)}}))
      .route("/channels/4/messages/100",axum::routing::patch(move|Json(mut v):Json<Value>|{let p=p.clone();async move{v["id"]=json!("100");v["channel_id"]=json!("4");p.lock().unwrap().push(v.clone());Json(v)}}))
      .route("/interactions/{id}/token/callback",post(move|Json(v):Json<Value>|{let reply=reply.clone();async move{assert_eq!(v,json!({"type":6}));reply.fetch_add(1,Ordering::SeqCst);StatusCode::NO_CONTENT}}))
      .route("/webhooks/9/token",post(move|Json(v):Json<Value>|{let err=err.clone();async move{assert_eq!(v["flags"],64);assert!(v["content"].as_str().unwrap().contains("完了確認できません"));err.fetch_add(1,Ordering::SeqCst);Json(json!({}))}}))
      .route("/webhooks/9/token/messages/@original",axum::routing::patch(move||{let extra=extra.clone();async move{extra.fetch_add(1,Ordering::SeqCst);Json(json!({}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let tmp = tempfile::tempdir().unwrap();
    let cfg = common::config(&tmp);
    let (store, _lock) = common::store(&cfg).await;
    let rid = common::queued(&store, &cfg, "10").await;
    store.begin_send(rid.clone()).await.unwrap();
    store
        .identify(
            rid.clone(),
            "resp_a".into(),
            "thread_a".into(),
            "turn_a".into(),
        )
        .await
        .unwrap();
    store
        .observe(rid.clone(), RequestState::Running, "test", true)
        .await
        .unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("token".into(), endpoint.clone()).unwrap(),
        Proxy::new(endpoint, "key".into()).unwrap(),
    )
    .unwrap();
    app.typing_tick().await.unwrap();
    assert_eq!(typing.load(Ordering::SeqCst), 1);
    let id = rid.clone();
    app.store.call(true,move|c|{c.execute("INSERT INTO approvals(id,request_id,thread_id,turn_id,state) VALUES('approval_a',?1,'thread_a','turn_a','PENDING')",[id])?;Ok(())}).await.unwrap();
    app.refresh_approval("approval_a").await.unwrap();
    app.typing_tick().await.unwrap();
    assert_eq!(typing.load(Ordering::SeqCst), 1);
    assert_eq!(
        cards.lock().unwrap()[0]["components"][0]["components"][0]["label"],
        "今回のみ承認"
    );
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let a = app.clone();
    let job = tokio::spawn(async move { a.control_loop(rx).await });
    tx.send(Incoming::Interaction(json!({"type":3,"id":"200","guild_id":"1","channel_id":"4","application_id":"9","token":"token","member":{"user":{"id":"2"}},"data":{"custom_id":"approval:approval_a:accept"}}))).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while approved.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(approved.load(Ordering::SeqCst), 1);
    app.refresh_approval("approval_a").await.unwrap();
    assert_eq!(
        cards.lock().unwrap().last().unwrap()["components"],
        json!([])
    );
    assert!(
        cards.lock().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Codexへ送りました")
    );
    tx.send(Incoming::Interaction(json!({"type":3,"id":"201","guild_id":"1","channel_id":"4","application_id":"9","token":"token","member":{"user":{"id":"2"}},"data":{"custom_id":"approval:approval_a:accept"}}))).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while replies.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(approved.load(Ordering::SeqCst), 1);
    app.typing_tick().await.unwrap();
    assert_eq!(typing.load(Ordering::SeqCst), 2);
    app.store
        .observe(rid, RequestState::Completed, "test", true)
        .await
        .unwrap();
    app.typing_tick().await.unwrap();
    assert_eq!(typing.load(Ordering::SeqCst), 2);
    app.store
        .call(false, |c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM conversation_creations", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(errors.load(Ordering::SeqCst), 0);
    tx.send(Incoming::Interaction(json!({"type":3,"id":"202","guild_id":"1","channel_id":"4","application_id":"9","token":"token","member":{"user":{"id":"2"}},"data":{"custom_id":"approval:missing:accept"}}))).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while errors.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(approved.load(Ordering::SeqCst), 1);
    app.cancel.cancel();
    job.await.unwrap().unwrap();
    assert_eq!(extra_messages.load(Ordering::SeqCst), 0);
    server.abort();
}
