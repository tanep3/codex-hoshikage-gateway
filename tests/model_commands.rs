mod common;
use axum::{
    Json, Router,
    routing::{get, patch, post},
};
use codex_hoshikage_gateway::{
    application::App,
    discord::{Discord, Incoming},
    proxy::Proxy,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
#[tokio::test]
async fn model_commands_list_inspect_and_change_in_channel() {
    let replies = Arc::new(Mutex::new(Vec::<String>::new()));
    let out = replies.clone();
    let router=Router::new()
      .route("/channels/4",get(||async{Json(json!({"id":"4","guild_id":"1","type":0}))}))
      .route("/v1/models",get(||async{Json(json!({"data":[{"id":"chatgpt/test","owned_by":"chatgpt"},{"id":"chatgpt/next","owned_by":"chatgpt"}]}))}))
      .route("/interactions/{id}/test-token/callback",post(||async{Json(json!({}))}))
      .route("/webhooks/9/test-token/messages/@original",patch(move|Json(v):Json<Value>|{let out=out.clone();async move {out.lock().unwrap().push(v["content"].as_str().unwrap().into());Json(json!({}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let t = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&t);
    cfg.projects[0].channel_id = "4".into();
    let (store, _lock) = common::store(&cfg).await;
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("fake-long-credential-for-fixture".into(), endpoint.clone())
            .unwrap(),
        Proxy::new(endpoint, "fake-long-credential-for-fixture".into()).unwrap(),
    )
    .unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let a = app.clone();
    let job = tokio::spawn(async move { a.control_loop(rx).await });
    for (i, data) in [
        json!({"name":"models"}),
        json!({"name":"model"}),
        json!({"name":"model","options":[{"name":"id","type":3,"value":"chatgpt/next"}]}),
        json!({"name":"model","options":[{"name":"id","type":3,"value":"missing"}]}),
    ]
    .into_iter()
    .enumerate()
    {
        tx.send(Incoming::Interaction(json!({"id":(100+i).to_string(),"guild_id":"1","channel_id":"4","application_id":"9","token":"test-token","member":{"user":{"id":"2"}},"data":data}))).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while replies.lock().unwrap().len() <= i {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    let r = replies.lock().unwrap().clone();
    assert!(r[0].contains("chatgpt/next"));
    assert!(r[1].contains("選択中のモデル: chatgpt/test"));
    assert!(r[2].contains("chatgpt/next にしました"));
    assert!(r[3].contains("そのモデルIDは利用できません"));
    assert_eq!(
        app.store.conversation("4").await.unwrap().selected_model,
        "chatgpt/next"
    );

    // Plain model commands use control handling even without a mention or ready generation gate.
    app.settings.write().await.cfg.discord.response_mode =
        codex_hoshikage_gateway::config::ResponseMode::Mention;
    let (messages, rx) = tokio::sync::mpsc::channel(8);
    let a = app.clone();
    let admission = tokio::spawn(async move { a.admit_loop(rx).await });
    for (id, content, user) in [
        ("200", "/model chatgpt/test", "2"),
        ("200", "/model chatgpt/test", "2"),
        ("201", "/model", "2"),
        ("202", "/model id:", "2"),
        ("203", "/stop", "2"),
        ("204", "/model chatgpt/next", "999"),
    ] {
        messages.send(Incoming::Message(json!({"id":id,"channel_id":"4","guild_id":"1","author":{"id":user,"bot":false},"content":content,"attachments":[]}))).await.unwrap();
        if id != "204" {
            let target = id.to_owned();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let target = target.clone();
                    let seen = app
                        .store
                        .call(true, move |c| {
                            Ok(c.query_row(
                                "SELECT EXISTS(SELECT 1 FROM notices WHERE id=?1)",
                                [target],
                                |r| r.get::<_, bool>(0),
                            )?)
                        })
                        .await
                        .unwrap();
                    if seen {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
    }
    assert_eq!(
        app.store.conversation("4").await.unwrap().selected_model,
        "chatgpt/test"
    );
    app.store
        .call(true, |c| {
            assert_eq!(
                c.query_row("SELECT COUNT(*) FROM requests", [], |r| r.get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                c.query_row(
                    "SELECT COUNT(*) FROM operations WHERE interaction_id='200'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                c.query_row(
                    "SELECT COUNT(*) FROM operations WHERE interaction_id='204'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            let selected: String =
                c.query_row("SELECT code FROM notices WHERE id='201'", [], |r| r.get(0))?;
            assert!(selected.contains("chatgpt/test"));
            Ok(())
        })
        .await
        .unwrap();
    app.cancel.cancel();
    job.await.unwrap().unwrap();
    admission.await.unwrap().unwrap();
    server.abort();
}
