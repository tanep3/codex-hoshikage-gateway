//! Real Gateway App -> deployed Proxy -> Codex -> MCP. Discord HTTP is simulated.
//! Never counts as verification of a real Discord user's button click.
mod common;
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, patch, post, put},
};
use codex_hoshikage_gateway::{
    application::App,
    config::{Config, secret},
    discord::{Discord, Incoming},
    proxy::Proxy,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
#[derive(Default)]
struct Wire {
    messages: HashMap<String, Value>,
    private: Vec<Value>,
    sequence: u64,
}
type Shared = Arc<Mutex<Wire>>;
fn input(id: &str) -> Value {
    if std::env::var("HOSHIKAGE_MCP_CASE").as_deref() == Ok("transport") {
        return json!({"id":id,"channel_id":"4","guild_id":"1","author":{"id":"2","bot":false},"attachments":[],"content":"Playwright MCP通信の診断です。順番に browser_tabs(action=list)、browser_navigate(url=https://www.showroom-live.com/)、browser_snapshot(depth=3)、browser_tabs(action=list) を1回ずつ実行してください。functions.execを使う場合は4操作を同じexecにまとめず、1操作ごとに別々のexec呼出しで実行してください。それ以外の操作、ログイン、クリック、ファイル操作は禁止です。ツール発見は必要なら行ってください。画面内容・個人情報は回答に記載しないでください。どれかが失敗した場合は再試行せずエラーの種類だけ報告して終了。4呼出しとも成功した場合だけ mcp transport done と回答してください。"});
    }
    if std::env::var("HOSHIKAGE_MCP_TOOL").as_deref() == Ok("tabs") {
        return json!({"id":id,"channel_id":"4","guild_id":"1","author":{"id":"2","bot":false},"attachments":[],"content":"Gateway API 0.6 結合試験です。playwright MCPの browser_tabs を action=list で必ず別々に5回、順番に呼んでください。他の引数は指定しないでください。タブの作成・選択・閉じる操作、移動、検索、ページ内容の取得、他のMCP、ファイル操作は禁止です。ツールの発見は必要なら行ってください。一覧の内容は回答に含めず、5回完了したら mcp acceptance done とだけ答えてください。"});
    }
    json!({"id":id,"channel_id":"4","guild_id":"1","author":{"id":"2","bot":false},"attachments":[],"content":"Gateway MCP結合試験です。playwright MCPのみ使用してください。最初に browser_navigate で https://example.com/ を開いてください。その後 browser_find で Example Domain を検索する呼出しを必ず別々に5回、順番に行ってください。検索条件は5回とも同じです。browser_findの引数はtextだけを指定し、その値をExample Domainにしてください。regexや他の引数を追加しないでください。browser_evaluate、コード実行、ファイル操作、他のサイトへのアクセスは使わないでください。MCPツールの発見は必要なら行ってください。5回終わったら mcp acceptance done とだけ答えてください。"})
}
fn event(custom: String) -> Value {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(10000);
    json!({"id":NEXT.fetch_add(1, Ordering::SeqCst).to_string(),"type":3,"guild_id":"1","channel_id":"4","application_id":"9","token":"live-token","member":{"user":{"id":"2"}},"data":{"custom_id":custom}})
}
fn buttons(v: &Value, prefix: &str) -> Vec<String> {
    v["components"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|row| row["components"].as_array().into_iter().flatten())
        .filter_map(|b| b["custom_id"].as_str())
        .filter(|s| s.starts_with(prefix))
        .map(str::to_owned)
        .collect()
}
#[tokio::test]
#[ignore = "real Codex/MCP executions; explicit HOSHIKAGE_LIVE_CONFIG opt-in required"]
async fn gateway_real_mcp_single_and_turn_grants() -> Result<()> {
    let live = Config::read(std::path::Path::new(&std::env::var(
        "HOSHIKAGE_LIVE_CONFIG",
    )?))?;
    let tmp = tempfile::tempdir()?;
    let mut cfg = common::config(&tmp);
    cfg.proxy = live.proxy;
    cfg.limits = live.limits;
    cfg.projects.clear();
    cfg.default_model = Some("chatgpt/gpt-5.6-luna".into());
    let wire = Shared::default();
    let router = Router::new()
        .route(
            "/users/@me",
            get(|| async { Json(json!({"id":"9","bot":true})) }),
        )
        .route(
            "/applications/9/guilds/1/commands",
            put(|| async { Json(json!([])) }),
        )
        .route(
            "/channels/4",
            get(|| async { Json(json!({"id":"4","guild_id":"1","type":0})) }),
        )
        .route(
            "/channels/4/messages",
            post(
                |State(w): State<Shared>, Json(mut v): Json<Value>| async move {
                    let mut w = w.lock().unwrap();
                    w.sequence += 1;
                    let id = (500 + w.sequence).to_string();
                    v["id"] = json!(id);
                    v["channel_id"] = json!("4");
                    v["author"] = json!({"id":"9","bot":true});
                    w.messages.insert(id, v.clone());
                    Json(v)
                },
            ),
        )
        .route(
            "/channels/4/messages/{id}",
            get(
                |State(w): State<Shared>, Path(id): Path<String>| async move {
                    Json(if id == "10" || id == "11" || id == "12" || id == "13" {
                        input(&id)
                    } else {
                        w.lock()
                            .unwrap()
                            .messages
                            .get(&id)
                            .cloned()
                            .unwrap_or(json!({}))
                    })
                },
            )
            .patch(
                |State(w): State<Shared>, Path(id): Path<String>, Json(v): Json<Value>| async move {
                    let mut w = w.lock().unwrap();
                    let m = w.messages.get_mut(&id).unwrap();
                    for (k, v) in v.as_object().unwrap() {
                        m[k] = v.clone();
                    }
                    Json(m.clone())
                },
            )
            .delete(
                |State(w): State<Shared>, Path(id): Path<String>| async move {
                    w.lock().unwrap().messages.remove(&id);
                    axum::http::StatusCode::NO_CONTENT
                },
            ),
        )
        .route(
            "/interactions/{id}/live-token/callback",
            post(|| async { Json(json!({})) }),
        )
        .route(
            "/webhooks/9/live-token",
            post(|State(w): State<Shared>, Json(v): Json<Value>| async move {
                w.lock().unwrap().private.push(v.clone());
                Json(v)
            }),
        )
        .route(
            "/webhooks/9/live-token/messages/@original",
            patch(|State(w): State<Shared>, Json(v): Json<Value>| async move {
                w.lock().unwrap().private.push(v.clone());
                Json(v)
            }),
        )
        .with_state(wire.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    codex_hoshikage_gateway::storage::initialize(&cfg)?;
    let (store, _) = codex_hoshikage_gateway::storage::Store::open(&cfg)?;
    let proxy = Proxy::new(cfg.proxy.base_url.clone(), secret(&cfg.proxy.api_key_file)?)?;
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("fixture".into(), endpoint)?,
        proxy,
    )?;
    app.settings().await.proxy.check().await?;
    app.discord.register("9", "1").await?;
    app.connected.store(true, Ordering::SeqCst);
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let mut jobs = Vec::new();
    let a = app.clone();
    jobs.push(tokio::spawn(async move { a.admit_loop(rx).await }));
    macro_rules! run {
        ($method:ident) => {
            let a = app.clone();
            jobs.push(tokio::spawn(async move { a.$method().await }));
        };
    }
    let (control_tx, control_rx) = tokio::sync::mpsc::channel(8);
    let controller = app.clone();
    jobs.push(tokio::spawn(async move {
        controller.control_loop(control_rx).await
    }));
    run!(scheduler_loop);
    run!(monitor_loop);
    run!(resource_loop);
    run!(delivery_loop);
    run!(capability_loop);
    run!(mcp_interaction_loop);
    let result=tokio::time::timeout(Duration::from_secs(540),async{
        // Turn grant first, then next Run must ask again for every call.
        let mut handled=HashSet::new();
        let cases=match std::env::var("HOSHIKAGE_MCP_CASE").as_deref(){Ok("revoke")=>vec![("12",true)],Ok("cancel")=>vec![("13",false)],Ok("transport")=>vec![("10",false),("11",false)],_=>vec![("10",true),("11",false)]};
        for (message,turn) in cases {
            tx.send(Incoming::Message(input(message))).await?;
            let mut find_count=0;let mut nav_count=0;let mut revoked=false;
            loop {
                let detail_buttons:Vec<String>=wire.lock().unwrap().messages.values().flat_map(|m| {
                    let mut controls=buttons(m,"mt:details:");
                    let desired=if turn && !revoked {":turn"}else{":once"};
                    let inline=buttons(m,"mi:");
                    if let Some(button)=inline.iter().find(|s|s.ends_with(desired)).or_else(||inline.iter().find(|s|s.ends_with(":once"))){controls.push(button.clone());}
                    let modern=buttons(m,"ma6:");
                    let action=if turn && !revoked {"ma6:turn:"} else {"ma6:once:"};
                    if let Some(button)=modern.iter().find(|s|s.starts_with(action)).or_else(||modern.iter().find(|s|s.starts_with("ma6:once:"))){controls.push(button.clone());}
                    controls
                }).collect();
                for custom in detail_buttons {
                    let parts:Vec<_>=custom.split(':').collect();
                    let inline=parts[0]=="mi";
                    let modern=parts[0]=="ma6";
                    let local=if inline{let view=parts[1].to_owned();app.store.call(false,move|c|Ok(c.query_row("SELECT interaction_local_id FROM mcp_inline_views WHERE id=?1",[view],|r|r.get::<_,String>(0))?)).await?}else{parts[2].to_owned()};
                    // Once/turn are two buttons for the same approval request.
                    // After revocation, do not select its other button while the
                    // resolved card is waiting for the monitor to remove it.
                    if handled.contains(&local){continue;}
                    let handled_id=local.clone();
                    // A mock POST exposes the card before Delivery has committed
                    // its receipt. Do not simulate a click on that unconfirmed UI.
                    if inline {
                        let view=parts[1].to_owned();
                        let confirmed:bool=app.store.call(false,move|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM deliveries d JOIN mcp_inline_views v ON d.target_id=v.interaction_local_id WHERE v.id=?1 AND v.active=1 AND d.kind='mcp_action' AND d.state='CONFIRMED' AND d.message_id IS NOT NULL)",[view],|r|r.get(0))?)).await?;
                        if !confirmed {continue;}
                    }
                    if modern {
                        let view=parts[4].to_owned();
                        let confirmed:bool=app.store.call(false,move|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM mcp_v06_views WHERE id=?1 AND active=1 AND state='READY')",[view],|r|r.get(0))?)).await?;
                        if !confirmed {continue;}
                    }
                    let interaction_local=local.clone();
                    let remote:String=app.store.call(false,move|c|Ok(c.query_row("SELECT interaction_id FROM mcp_interactions WHERE id=?1",[local],|r|r.get(0))?)).await?;
                    let op=app.settings().await.proxy.v2_json(reqwest::Method::GET,&format!("/v2/codex/interactions/{remote}/operation"),None,None).await?;
                    ensure!(op["binding_status"]=="verified","unverified operation");
                    ensure!(op["server"]=="playwright","unexpected MCP server");
                    let tool=op["tool"].as_str().context("tool missing")?;
                    let args=&op["arguments"];
                    let turn_this=match tool {
                        "browser_navigate"=>{let expected=if std::env::var("HOSHIKAGE_MCP_CASE").as_deref()==Ok("transport"){"https://www.showroom-live.com/"}else{"https://example.com/"};ensure!(args==&json!({"url":expected}),"unexpected navigation");nav_count+=1;false},
                        "browser_snapshot" if std::env::var("HOSHIKAGE_MCP_CASE").as_deref()==Ok("transport") => {ensure!(args==&json!({"depth":3}),"unexpected snapshot");false},
                        "browser_find"=>{ensure!(args["text"]=="Example Domain" && args.as_object().is_some_and(|o| o.keys().all(|k| k=="text")),"unexpected search (text_matches={}, keys={:?})",args["text"]=="Example Domain",args.as_object().map(|o|o.keys().collect::<Vec<_>>()));find_count+=1;turn && !revoked},
                        "browser_tabs" if std::env::var("HOSHIKAGE_MCP_TOOL").as_deref()==Ok("tabs") || std::env::var("HOSHIKAGE_MCP_CASE").as_deref()==Ok("transport") => {
                            ensure!(args==&json!({"action":"list"}),"unexpected tabs operation");
                            find_count+=1;turn && !revoked
                        },
                        _=>anyhow::bail!("unexpected tool: {tool}"),
                    };
                    eprintln!("message={message} operation={tool} eligible={} action={}",op["turn_grant_eligible"],if turn_this{"turn"}else{"once"});
                    if find_count == 1 && let Ok(delay)=std::env::var("HOSHIKAGE_MCP_REVIEW_DELAY_SECS") {
                        let delay: u64=delay.parse()?;
                        ensure!(delay<=120,"review delay must be at most 120 seconds");
                        eprintln!("waiting {delay}s before explicit user decision");
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                    }
                    if message == "13" {
                        let mut e=event(String::new());e["type"]=json!(2);e["data"]=json!({"name":"cancel"});
                        control_tx.send(Incoming::Interaction(e)).await?;
                        handled.insert(handled_id);
                        continue;
                    }
                    if modern {
                        ensure!(custom.starts_with(if turn_this{"ma6:turn:"}else{"ma6:once:"}),"v06 permission selection mismatch");
                        let msg=wire.lock().unwrap().messages.values().find(|m|buttons(m,"ma6:").contains(&custom)).cloned().context("v06 card missing")?;
                        let labels:Vec<String>=msg["components"].as_array().into_iter().flatten().flat_map(|r|r["components"].as_array().into_iter().flatten()).filter_map(|b|b["label"].as_str().map(str::to_owned)).collect();
                        ensure!(labels.contains(&"今回だけ許可".into())&&labels.contains(&"拒否".into()),"v06 choices missing");
                        if turn_this {ensure!(labels.contains(&"この依頼中、このツールを許可".into()),"v06 turn choice missing");}
                        ensure!(msg["content"].as_str().unwrap_or("").contains(&tool.replace('_', "\\_")),"tool missing from initial card");
                        let mut e=event(custom.clone());e["message"]=json!({"id":msg["id"]});
                        app.handle_v06_mcp(&e).await?;
                        let accepted:bool=app.store.call(false,move|c|Ok(c.query_row("SELECT operation_key IS NOT NULL AND action='accept' FROM mcp_interactions WHERE id=?1",[interaction_local],|r|r.get(0))?)).await?;
                        ensure!(accepted,"v06 approval did not record acceptance");
                        eprintln!("message={message} v06 initial card and explicit permission confirmed");
                    } else if inline {
                        ensure!(custom.ends_with(if turn_this{":turn"}else{":once"}),"inline permission selection mismatch");
                        let msg=wire.lock().unwrap().messages.values().find(|m|buttons(m,"mi:").contains(&custom)).cloned().context("inline card missing")?;
                        ensure!(msg["content"].as_str().unwrap_or("").contains(if tool=="browser_find"{"Example Domain"}else{"https://example.com/"}),"inline target missing");
                        let mut e=event(custom.clone());e["message"]=json!({"id":msg["id"]});
                        app.handle_inline_mcp(&e).await?;
                        let accepted:bool=app.store.call(false,move|c|Ok(c.query_row("SELECT operation_key IS NOT NULL AND action='accept' FROM mcp_interactions WHERE id=?1",[interaction_local],|r|r.get(0))?)).await?;
                        ensure!(accepted,"inline approval did not record acceptance; private responses: {:?}",wire.lock().unwrap().private);
                        eprintln!("message={message} inline card confirmed and selected");
                    } else {
                    app.handle_mcp_turn(&event(custom.clone())).await?;
                    let view=wire.lock().unwrap().private.last().cloned().context("private details missing")?;
                    ensure!(view["content"].as_str().unwrap_or("").contains(tool),"details missing tool: {}",view["content"]);
                    let prefix=if turn_this{"mt:turn:"}else{"mt:once:"};
                    let button=buttons(&view,prefix).into_iter().next().context("expected permission button missing")?;
                    app.handle_mcp_turn(&event(button)).await?;
                    let ack=wire.lock().unwrap().private.last().cloned().unwrap();
                    ensure!(ack["content"].as_str().unwrap_or("").contains("許可の回答を送りました"),"permission rejected: {}",ack["content"]);
                    }
                    if message=="12" && turn_this {
                        let request=op["scope"]["context"]["run_id"].as_str().context("run identity")?;
                        app.handle_mcp_turn(&event(format!("mt:grants:{request}"))).await?;
                        let req=request.to_owned();
                        let local:String=app.store.call(false,move|c|Ok(c.query_row("SELECT id FROM mcp_grant_records WHERE request_id=?1",[req],|r|r.get(0))?)).await?;
                        app.handle_mcp_turn(&event(format!("mt:revoke:{local}"))).await?;
                        let ack=wire.lock().unwrap().private.last().cloned().unwrap();
                        ensure!(ack["content"].as_str().unwrap_or("").contains("新たな適用を停止しました"),"revoke not confirmed");
                        revoked=true;
                        eprintln!("message={message} explicit grant revoke confirmed through Gateway");
                    }
                    handled.insert(handled_id);
                }
                let msg=message.to_owned();
                let rows:Vec<(String,String,Option<String>)>=app.store.call(false,move|c|{let mut q=c.prepare("SELECT id,state,response_id FROM requests WHERE message_id=?1")?;Ok(q.query_map([msg],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?)}).await?;
                if let Some((id,state,Some(response)))=rows.first()
                    && matches!(state.as_str(),"COMPLETED"|"FAILED"|"CANCELLED") {
                        eprintln!("message={message} request={id} response={response} terminal={state} navigate_prompts={nav_count} find_prompts={find_count}");
                    if message == "13" {
                            ensure!(state=="CANCELLED","cancel did not confirm interruption");
                            ensure!(!app.store.conversation("4").await?.paused,"cancel paused the queue");
                            ensure!(app.store.active("4").await?.is_none(),"cancel retained execution hold after confirmation");
                            ensure!(find_count==1,"unexpected operation count before cancel");
                            eprintln!("message={message} slash cancel confirmed by real Proxy; queue not paused; hold released");
                            break;
                        }
                        ensure!(state=="COMPLETED","execution did not complete");
                        if std::env::var("HOSHIKAGE_MCP_CASE").as_deref()==Ok("transport") {
                            eprintln!("transport diagnostic completed; inspect exact tool outcomes in Proxy rollout (COMPLETED alone is not tool success)");
                            break;
                        }
                        ensure!(nav_count<=1 && (if message=="12" {find_count>=2} else {find_count==if turn{1}else{5}}),"unexpected prompt count");
                        app.handle_mcp_turn(&event(format!("mt:grants:{id}"))).await?;
                        let list=app.settings().await.proxy.v2_json(reqwest::Method::GET,&format!("/v2/codex/responses/{response}/mcp-grants"),None,None).await?;
                        let grants=list["data"].as_array().context("grant list")?;
                        if revoked {ensure!(grants.len()==1 && grants[0]["state"]=="revoked","grant not revoked");ensure!(grants[0]["reason"]=="operator_revoked","revoke reason");ensure!(grants[0]["application_count"].as_u64().context("count")? + find_count - 1 == 5,"call counts after revoke");}else if turn {ensure!(grants.len()==1,"grant count");ensure!(matches!(grants[0]["state"].as_str(),Some("expired"|"revoked")) && grants[0]["reason"]=="scope_ended","grant not invalidated at Run end");ensure!(grants[0]["application_count"]==5,"expected five uses");}else{ensure!(grants.is_empty(),"grant leaked into next Run");}
                        eprintln!("message={message} grants checked; no cross-Run permission reuse");
                        break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        Ok::<(),anyhow::Error>(())
    }).await;
    for r in app.store.pending().await? {
        if r.response_id.is_some() {
            let _ = app.settings().await.proxy.stop_v2(&app.store, &r).await;
        }
    }
    app.cancel.cancel();
    for j in jobs {
        j.abort();
        let _ = j.await;
    }
    server.abort();
    result.context("real integration deadline exceeded")??;
    Ok(())
}
