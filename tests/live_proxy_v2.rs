//! Explicitly opted-in integration: real local Proxy/Codex, isolated Gateway DB, simulated Discord.
mod common;
use axum::{
    Json, Router,
    body::Bytes,
    http::HeaderMap,
    routing::{get, patch, post, put},
};
use codex_hoshikage_gateway::{
    application::App,
    config::{Config, secret},
    discord::{Discord, Incoming},
    domain::RequestState,
    proxy::Proxy,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
const CONTENT: &str = "hoshikage-gateway-v2-ok";
fn input() -> Value {
    json!({"id":"10","channel_id":"4","guild_id":"1","author":{"id":"2","bot":false},"content":"Gatewayの結合試験です。この会話専用ワークに gateway-acceptance.txt を作り、内容を正確に hoshikage-gateway-v2-ok としてください（改行なし）。hoshikage_publish_artifact でそのファイルを登録し、最後に gateway acceptance done とだけ答えてください。他のファイルやネットワークは操作しないでください。","attachments":[]})
}
#[tokio::test]
#[ignore = "creates a dedicated conversation and runs a real Codex Turn; explicitly opt in"]
async fn gateway_real_proxy_artifact_and_saved_answer() {
    let config = std::env::var("HOSHIKAGE_LIVE_CONFIG").expect("HOSHIKAGE_LIVE_CONFIG required");
    let live = Config::read(std::path::Path::new(&config)).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&tmp);
    cfg.proxy = live.proxy;
    cfg.proxy.contract_version = "2.0".into();
    cfg.default_model = Some("chatgpt/gpt-5.6-luna".into());
    cfg.projects.clear();
    cfg.limits = live.limits;
    let messages = Arc::new(Mutex::new(Vec::<Value>::new()));
    let m = messages.clone();
    let uploads = Arc::new(AtomicUsize::new(0));
    let u = uploads.clone();
    let sequence = Arc::new(AtomicUsize::new(500));
    let seq = sequence.clone();
    let router=Router::new()
        .route("/users/@me",get(||async{Json(json!({"id":"9","bot":true}))}))
        .route("/applications/9/guilds/1/commands",put(||async{Json(json!([]))}))
        .route("/channels/4",get(||async{Json(json!({"id":"4","guild_id":"1","type":0}))}))
        .route("/channels/4/messages/10",get(||async{Json(input())}))
        .route("/interactions/{id}/live-token/callback",post(||async{Json(json!({}))}))
        .route("/webhooks/9/live-token/messages/@original",patch(||async{Json(json!({}))}))
        .route("/channels/4/messages",post(move|h:HeaderMap,body:Bytes|{let m=m.clone();let u=u.clone();let seq=seq.clone();async move{
            let id=seq.fetch_add(1,Ordering::SeqCst).to_string();
            if h["content-type"].to_str().unwrap().starts_with("multipart/"){
                assert!(body.windows(CONTENT.len()).any(|w|w==CONTENT.as_bytes()));u.fetch_add(1,Ordering::SeqCst);
                Json(json!({"id":id,"channel_id":"4","attachments":[{"filename":"gateway-acceptance.txt","size":CONTENT.len()}]}))
            }else{let mut v:Value=serde_json::from_slice(&body).unwrap();v["id"]=json!(id);v["channel_id"]=json!("4");m.lock().unwrap().push(v.clone());Json(v)}
        }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    codex_hoshikage_gateway::storage::initialize(&cfg).unwrap();
    let (store, _) = codex_hoshikage_gateway::storage::Store::open(&cfg).unwrap();
    let p = Proxy::new(
        cfg.proxy.base_url.clone(),
        secret(&cfg.proxy.api_key_file).unwrap(),
    )
    .unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("live-fixture".into(), endpoint).unwrap(),
        p,
    )
    .unwrap();
    app.settings().await.proxy.check().await.unwrap();
    app.discord.register("9", "1").await.unwrap();
    app.connected.store(true, Ordering::SeqCst);
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let (control, crx) = tokio::sync::mpsc::channel(8);
    let mut jobs = Vec::new();
    macro_rules! run {
        ($method:ident) => {
            let a = app.clone();
            jobs.push(tokio::spawn(async move { a.$method().await }));
        };
    }
    let a = app.clone();
    jobs.push(tokio::spawn(async move { a.admit_loop(rx).await }));
    let a = app.clone();
    jobs.push(tokio::spawn(async move { a.control_loop(crx).await }));
    run!(scheduler_loop);
    run!(monitor_loop);
    run!(resource_loop);
    run!(delivery_loop);
    run!(capability_loop);
    run!(retention_loop);
    tx.send(Incoming::Message(input())).await.unwrap();
    let checked=tokio::time::timeout(Duration::from_secs(660),async{
        loop {
            let done:bool=app.store.call(false,|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM output_state WHERE state='DELIVERED')",[],|r|r.get(0))?)).await.unwrap();
            if done{break;}
            let rows=app.store.pending().await.unwrap();if rows.iter().any(|r|r.state==RequestState::Unknown){eprintln!("live execution currently UNKNOWN; continuing identity lookup");}
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        eprintln!("real Proxy saved answer delivered through Gateway");
        control.send(Incoming::Interaction(json!({"id":"100","guild_id":"1","channel_id":"4","application_id":"9","token":"live-token","member":{"user":{"id":"2"}},"data":{"name":"get"}}))).await.unwrap();
        let custom=loop{let found=messages.lock().unwrap().iter().find_map(|m|m["components"][0]["components"][0]["custom_id"].as_str().filter(|s|s.starts_with("pick:")).map(str::to_owned));if let Some(v)=found{break v;}tokio::time::sleep(Duration::from_millis(100)).await;};
        control.send(Incoming::Interaction(json!({"id":"101","guild_id":"1","channel_id":"4","application_id":"9","token":"live-token","member":{"user":{"id":"2"}},"data":{"custom_id":custom,"values":["0"]}}))).await.unwrap();
        while uploads.load(Ordering::SeqCst)==0{tokio::time::sleep(Duration::from_millis(100)).await;}
        let record:Value=app.store.call(false,|c|Ok(c.query_row("SELECT conversation_id,workspace_id FROM proxy_conversations WHERE thread_id='4'",[],|r|Ok(json!({"conversation_id":r.get::<_,String>(0)?,"workspace_id":r.get::<_,String>(1)?})))?)).await.unwrap();
        eprintln!("live isolated resources: {record}");
        assert_eq!(uploads.load(Ordering::SeqCst),1);
    }).await;
    app.cancel.cancel();
    for job in jobs {
        job.abort();
        let _ = job.await;
    }
    server.abort();
    checked.expect("live Gateway/Proxy integration deadline exceeded");
}

#[tokio::test]
#[ignore = "reads an existing real Proxy conversation; creates retention leases but never executes AI"]
async fn gateway_database_reopen_recovers_same_saved_answer_without_execution() {
    let config = std::env::var("HOSHIKAGE_LIVE_CONFIG").unwrap();
    let conversation = std::env::var("HOSHIKAGE_LIVE_CONVERSATION").unwrap();
    let live = Config::read(std::path::Path::new(&config)).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&tmp);
    cfg.proxy = live.proxy;
    cfg.proxy.contract_version = "2.0".into();
    cfg.default_model = Some("chatgpt/gpt-5.6-luna".into());
    cfg.projects.clear();
    cfg.limits = live.limits;
    let key = secret(&cfg.proxy.api_key_file).unwrap();
    let p = Proxy::new(cfg.proxy.base_url.clone(), key.clone()).unwrap();
    p.check().await.unwrap();
    let cv = p
        .v2_json(
            reqwest::Method::GET,
            &format!("/v2/codex/conversations/{conversation}"),
            None,
            None,
        )
        .await
        .unwrap();
    let response = cv["last_response_id"].as_str().unwrap().to_owned();
    let workspace = cv["workspace_id"].as_str().unwrap().to_owned();
    let sent = Arc::new(Mutex::new(Vec::<String>::new()));
    let out = sent.clone();
    let router = Router::new()
        .route(
            "/channels/4",
            get(|| async { Json(json!({"id":"4","guild_id":"1","type":0})) }),
        )
        .route(
            "/channels/4/messages",
            post(move |Json(v): Json<Value>| {
                let out = out.clone();
                async move {
                    out.lock()
                        .unwrap()
                        .push(v["content"].as_str().unwrap().into());
                    Json(json!({"id":"500","channel_id":"4"}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    codex_hoshikage_gateway::storage::initialize(&cfg).unwrap();
    let (store, done) = codex_hoshikage_gateway::storage::Store::open(&cfg).unwrap();
    store
        .add_conversation(
            "4".into(),
            codex_hoshikage_gateway::storage::PROXY_SCOPE.into(),
        )
        .await
        .unwrap();
    let delivery = format!("reopen-{}", uuid::Uuid::new_v4());
    let d = delivery.clone();
    store.call(true,move|c|{c.execute("INSERT INTO proxy_conversations(thread_id,request_key,request_json,conversation_id,workspace_id,state) VALUES('4','reopen-conversation','{}',?1,?2,'READY')",rusqlite::params![conversation,workspace])?;c.execute("INSERT INTO resource_deliveries(id,thread_id,resource_type,resource_id,request_key,created_at) VALUES(?1,'4','response_output',?2,?3,0)",rusqlite::params![d,response,format!("lease-{d}")])?;Ok(())}).await.unwrap();
    let app = App::new(
        cfg.clone(),
        store,
        Discord::with_endpoint("fixture".into(), endpoint.clone()).unwrap(),
        p,
    )
    .unwrap();
    app.settings().await.proxy.check().await.unwrap();
    app.connected.store(true, Ordering::SeqCst);
    let a = app.clone();
    let task = tokio::spawn(async move { a.resource_loop().await });
    let text = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(text) = app
                .output
                .lock()
                .await
                .get(&delivery)
                .map(|o| o.text.clone())
            {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(!text.is_empty());
    app.cancel.cancel();
    task.abort();
    let _ = task.await;
    drop(app);
    let _ = done.await;
    let (store, _) = codex_hoshikage_gateway::storage::Store::open(&cfg).unwrap();
    store.startup_recover().await.unwrap();
    let app = App::new(
        cfg.clone(),
        store,
        Discord::with_endpoint("fixture".into(), endpoint).unwrap(),
        Proxy::new(cfg.proxy.base_url, key).unwrap(),
    )
    .unwrap();
    app.settings().await.proxy.check().await.unwrap();
    app.connected.store(true, Ordering::SeqCst);
    let a = app.clone();
    let resource = tokio::spawn(async move { a.resource_loop().await });
    let recovered = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(text) = app
                .output
                .lock()
                .await
                .get(&delivery)
                .map(|o| o.text.clone())
            {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(recovered, text);
    let a = app.clone();
    let deliver = tokio::spawn(async move { a.delivery_loop().await });
    tokio::time::timeout(Duration::from_secs(10), async {
        while sent.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(sent.lock().unwrap().as_slice(), [text]);
    let executions: i64 = app
        .store
        .call(false, |c| {
            Ok(c.query_row("SELECT count(*) FROM requests", [], |r| r.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(executions, 0);
    app.cancel.cancel();
    resource.abort();
    deliver.abort();
    let _ = resource.await;
    let _ = deliver.await;
    server.abort();
}

#[tokio::test]
#[ignore = "posts a message and one saved artifact to the explicitly approved Discord channel"]
async fn gateway_real_discord_delivery() {
    let config = std::env::var("HOSHIKAGE_LIVE_CONFIG").unwrap();
    let conversation = std::env::var("HOSHIKAGE_LIVE_CONVERSATION").unwrap();
    let channel = std::env::var("HOSHIKAGE_LIVE_CHANNEL").unwrap();
    let guild = std::env::var("HOSHIKAGE_LIVE_GUILD").unwrap();
    let mut cfg = Config::read(std::path::Path::new(&config)).unwrap();
    assert_eq!(cfg.discord.guild_id, guild);
    let tmp = tempfile::tempdir().unwrap();
    cfg.storage.state_dir = tmp.path().join("state");
    cfg.storage.temp_dir = tmp.path().join("cache");
    cfg.storage.socket_path = tmp.path().join("admin.sock");
    cfg.proxy.contract_version = "2.0".into();
    cfg.projects.clear();
    cfg.default_model = Some("chatgpt/gpt-5.6-luna".into());
    let p = Proxy::new(
        cfg.proxy.base_url.clone(),
        secret(&cfg.proxy.api_key_file).unwrap(),
    )
    .unwrap();
    p.check().await.unwrap();
    let cv = p
        .v2_json(
            reqwest::Method::GET,
            &format!("/v2/codex/conversations/{conversation}"),
            None,
            None,
        )
        .await
        .unwrap();
    let workspace = cv["workspace_id"].as_str().unwrap().to_owned();
    let artifacts = p
        .v2_json(
            reqwest::Method::GET,
            &format!("/v2/codex/conversations/{conversation}/artifacts"),
            None,
            None,
        )
        .await
        .unwrap();
    let artifact = artifacts["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["display_name"] == "gateway-acceptance.txt" && a["size_bytes"] == CONTENT.len())
        .unwrap();
    let artifact_id = artifact["artifact_id"].as_str().unwrap().to_owned();
    let discord = Discord::new(secret(&cfg.discord.token_file).unwrap()).unwrap();
    let ch = discord.get(&format!("/channels/{channel}")).await.unwrap();
    assert_eq!(ch["guild_id"], guild);
    assert_eq!(ch["id"], channel);
    codex_hoshikage_gateway::storage::initialize(&cfg).unwrap();
    let (store, _) = codex_hoshikage_gateway::storage::Store::open(&cfg).unwrap();
    store
        .add_conversation(
            channel.clone(),
            codex_hoshikage_gateway::storage::PROXY_SCOPE.into(),
        )
        .await
        .unwrap();
    let delivery = format!("discord-acceptance-{}", uuid::Uuid::new_v4());
    let (d, t) = (delivery.clone(), channel.clone());
    store.call(true,move|c|{c.execute("INSERT INTO proxy_conversations(thread_id,request_key,request_json,conversation_id,workspace_id,state) VALUES(?1,?2,'{}',?3,?4,'READY')",rusqlite::params![t,format!("conversation-{d}"),conversation,workspace])?;c.execute("INSERT INTO resource_deliveries(id,thread_id,resource_type,resource_id,request_key,created_at) VALUES(?1,?2,'artifact',?3,?4,0)",rusqlite::params![d,t,artifact_id,format!("lease-{d}")])?;Ok(())}).await.unwrap();
    let app = App::new(cfg, store, discord, p).unwrap();
    app.settings().await.proxy.check().await.unwrap();
    app.connected.store(true, Ordering::SeqCst);
    assert!(
        app.delivery
            .text(
                &format!("notice-{delivery}"),
                &channel,
                "acceptance",
                0,
                "Gateway v2 配信テスト",
                json!([])
            )
            .await
            .unwrap()
    );
    let a = app.clone();
    let job = tokio::spawn(async move { a.resource_loop().await });
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let d = delivery.clone();
            let row: (String, Option<String>, Option<String>) = app
                .store
                .call(false, move |c| {
                    Ok(c.query_row(
                        "SELECT state,message_id,error_code FROM resource_deliveries WHERE id=?1",
                        [d],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )?)
                })
                .await
                .unwrap();
            if matches!(row.0.as_str(), "RELEASE_PENDING" | "DELIVERED") {
                break row.1.unwrap();
            }
            if row.2.is_some() {
                eprintln!("Discord delivery state: {} / {:?}", row.0, row.2);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    app.cancel.cancel();
    job.abort();
    let _ = job.await;
    let message = result.expect("Discord delivery unconfirmed; do not automatically repeat test");
    eprintln!(
        "verified Discord artifact: https://discord.com/channels/{guild}/{channel}/{message}"
    );
    let receipt = app
        .discord
        .get(&format!("/channels/{channel}/messages/{message}"))
        .await
        .unwrap();
    assert_eq!(receipt["attachments"][0]["size"], CONTENT.len());
}
