mod common;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use codex_hoshikage_gateway::{
    application::App, discord::Discord, domain::RequestState, mcp_form, proxy::Proxy,
};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Mock {
    items: Vec<Value>,
    posts: Vec<Value>,
    keys: Vec<String>,
    messages: Vec<Value>,
    callbacks: Vec<Value>,
    private: Vec<Value>,
    private_original: Value,
    deferred_private: bool,
    private_sequence: u64,
    lost: bool,
    targets: Vec<String>,
    deleted: Vec<String>,
    fail_delete: bool,
    operation: Value,
    turn_caps: bool,
    inline_caps: bool,
    v06_caps: bool,
    private_pages: Vec<Value>,
    response: Option<Value>,
    reply_reject: bool,
    reject_code: Option<&'static str>,
    loading_gets: usize,
    fail_grants: bool,
    lost_display: bool,
    presentation_gets: usize,
    presentation: Value,
    grants: Vec<Value>,
    revoke_keys: Vec<String>,
}
fn bound(v: Value) -> Response {
    (
        [
            ("X-Proxy-Instance-Id", "pxy_test"),
            ("X-Proxy-Recovery-Generation", "gen_test"),
        ],
        Json(v),
    )
        .into_response()
}
fn item(id: &str, schema: Value) -> Value {
    json!({"interaction_id":id,"response_id":"resp_a","conversation_id":"conv_a","workspace_id":"ws_a","revision":1,"kind":"mcp_form","state":"pending","reply_status":"not_sent","expires_at":"2099-01-01T00:00:00Z","request":{"serverName":"lightpanda","message":"Yahoo! JAPANを開いてよいですか？","_meta":{"codex_approval_kind":"mcp_tool_call"},"requestedSchema":schema}})
}
fn empty() -> Value {
    json!({"type":"object","properties":{}})
}
async fn setup() -> (
    App,
    Arc<Mutex<Mock>>,
    tempfile::TempDir,
    codex_hoshikage_gateway::storage::StateLock,
    tokio::task::JoinHandle<()>,
    String,
) {
    let mock = Arc::new(Mutex::new(Mock {
        items: vec![item("int_a", empty())],
        ..Default::default()
    }));
    let router = Router::new()
 .route("/v2/codex/responses/resp_a/mcp-grants",get(|State(m):State<Arc<Mutex<Mock>>>|async move{let m=m.lock().unwrap();if m.fail_grants{return StatusCode::SERVICE_UNAVAILABLE.into_response();}bound(json!({"response_id":"resp_a","data":m.grants}))}))
 .route("/v2/codex/mcp-grants/grant1/revoke",post(|State(m):State<Arc<Mutex<Mock>>>,h:HeaderMap|async move{
 let mut m=m.lock().unwrap();m.revoke_keys.push(h["Idempotency-Key"].to_str().unwrap().to_owned());m.grants[0]["state"]=json!("revoked");m.grants[0]["reason"]=json!("operator_revoked");
 StatusCode::SERVICE_UNAVAILABLE
 }))

 .route("/v2/codex/interactions/{id}/presentation",get(|State(m):State<Arc<Mutex<Mock>>>,axum::extract::Query(q):axum::extract::Query<std::collections::HashMap<String,String>>|async move{let mut m=m.lock().unwrap();m.presentation_gets+=1;if m.loading_gets>0 {m.loading_gets-=1;let mut p=if q.get("audience").is_some_and(|s|s=="requester"){m.private_pages[q.get("page").and_then(|s|s.parse::<usize>().ok()).unwrap_or(0)].clone()}else{m.presentation.clone()};p["state"]=json!("unavailable");p["reason"]=json!("catalog_loading");p["diagnostic"]=json!({"code":"catalog_loading","retryable":true,"retry_after_ms":2000});p["actions"]["allow_once"]=json!(false);p["actions"]["allow_turn_tool"]=json!(false);p["actions"]["retry"]=json!(true);for k in ["presentation_id","presentation_fingerprint","page","expires_at"] {p[k]=Value::Null;}codex_hoshikage_gateway::mcp_v06::Presentation::parse(p.clone()).expect("loading fixture");return bound(p);}
 if q.get("audience").is_some_and(|s|s=="requester"){let page=q.get("page").and_then(|s|s.parse::<usize>().ok()).unwrap_or(0);return bound(m.private_pages.get(page).cloned().unwrap_or(Value::Null));}bound(m.presentation.clone())}))
 .route("/v2/codex/interactions/{id}/operation",get(|State(m):State<Arc<Mutex<Mock>>>|async move{bound(m.lock().unwrap().operation.clone())}))
 .route("/users/@me",get(||async{Json(json!({"id":"9","bot":true}))}))
 .route("/readyz",get(||async{Json(json!({"status":"ready"}))}))
 .route("/v2/codex/capabilities",get(|State(m):State<Arc<Mutex<Mock>>>|async move{let mut c=mcp_caps(); if m.lock().unwrap().turn_caps {c["mcp_operation_details"]=json!({"enabled":true,"profile":"native-item-id-v1","max_argument_bytes":65536,"disclosure":"requester_only"});c["mcp_turn_approval"]=json!({"enabled":true,"profile":"native-item-id-v1","max_grants":16,"ttl_seconds":600,"max_records":256});}
 if m.lock().unwrap().inline_caps {c["mcp_inline_approval"]=inline_capability();}
 if m.lock().unwrap().v06_caps {c["mcp_approval_v06"]=v06_examples()["capability"]["mcp_approval_v06"].clone();}Json(c)}))
 .route("/v2/codex/conversations/conv_a/responses",post(|State(m):State<Arc<Mutex<Mock>>>,Json(v):Json<Value>|async move{m.lock().unwrap().posts.push(v);bound(json!({"response_id":"resp_a"}))}))
 .route("/v2/codex/conversations/conv_a",get(||async{bound(json!({"conversation_id":"conv_a","workspace_id":"ws_a","state":"ready"}))}))
 .route("/v2/codex/responses/resp_a",get(|State(m):State<Arc<Mutex<Mock>>>|async move{bound(m.lock().unwrap().response.clone().unwrap_or(json!({"response_id":"resp_a","conversation_id":"conv_a","workspace_id":"ws_a","execution_status":"interrupted","error":{"code":"unsupported_interaction"}})))}))
        .route(
            "/channels/4",
            get(|| async { Json(json!({"id":"4","guild_id":"1","type":0})) }),
        )
        .route(
            "/v2/codex/responses/resp_a/interactions",
            get(|State(m): State<Arc<Mutex<Mock>>>| async move {
                bound(json!({"response_id":"resp_a","data":m.lock().unwrap().items}))
            }),
        )
        .route(
            "/v2/codex/interactions/{id}",
            get(
                |State(m): State<Arc<Mutex<Mock>>>, Path(id): Path<String>| async move {
                    bound(
                        m.lock()
                            .unwrap()
                            .items
                            .iter()
                            .find(|v| v["interaction_id"] == id)
                            .unwrap()
                            .clone(),
                    )
                },
            ),
        )
        .route(
            "/v2/codex/interactions/{id}/reply",
            post(
                |State(m): State<Arc<Mutex<Mock>>>,
                 Path(id): Path<String>,
                 h: HeaderMap,
                 Json(v): Json<Value>| async move {
                    assert_eq!(h["X-Proxy-Instance-Id"], "pxy_test");
                    assert_eq!(h["X-Proxy-Recovery-Generation"], "gen_test");
                    assert_eq!(h["authorization"], "Bearer key");
                    let mut m = m.lock().unwrap();
                    if m.reply_reject {m.reply_reject=false;return (StatusCode::CONFLICT,[("X-Proxy-Instance-Id","pxy_test"),("X-Proxy-Recovery-Generation","gen_test")],Json(json!({"error":{"code":m.reject_code.unwrap_or("presentation_stale")}}))).into_response();}
                    m.keys.push(h["Idempotency-Key"].to_str().unwrap().into());
                    m.posts.push(v.clone());
                    m.targets.push(id.clone());
                    let i = m
                        .items
                        .iter_mut()
                        .find(|v| v["interaction_id"] == id)
                        .unwrap();
                    i["state"] = json!("submitted");
                    i["reply_status"] = json!("written");
                    i["revision"] = json!(2);
                    i["request"] = Value::Null;
                    if m.lost {
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    } else {
                        bound(
                            json!({"state":"succeeded","resource":{"type":"interaction","id":id}}),
                        )
                    }
                },
            ),
        )
        .route(
            "/v2/codex/operations/by-key/{key}",
            get(|State(m):State<Arc<Mutex<Mock>>>,Path(key):Path<String>| async move {
                let m=m.lock().unwrap();
                if m.revoke_keys.contains(&key) {return bound(json!({"kind":"mcp_grant.revoke","state":"succeeded","resource":{"type":"mcp_grant","id":"grant1"},"in_flight_or_unknown_count":2}));}
                let index=m.keys.iter().position(|k| k==&key).unwrap();
                bound(json!({"state":"succeeded","resource":{"type":"interaction","id":m.targets[index]}}))
            }),
        )
        .route(
            "/channels/4/messages",
            post(
                |State(m): State<Arc<Mutex<Mock>>>, Json(mut v): Json<Value>| async move {
                    let mut m = m.lock().unwrap();
                    v["id"] = json!((100 + m.messages.len()).to_string());
                    v["channel_id"] = json!("4");
                    v["author"] = json!({"id":"9","bot":true});
                    m.messages.push(v.clone());
                    if m.lost_display {m.lost_display=false;return StatusCode::SERVICE_UNAVAILABLE.into_response();}
                    Json(v).into_response()
                },
            ),
        )
        .route(
            "/channels/4/messages/{id}",
            patch(
                |State(m): State<Arc<Mutex<Mock>>>,
                 Path(id): Path<String>,
                 Json(mut v): Json<Value>| async move {
                    v["id"] = json!(id);
                    v["channel_id"] = json!("4");
                    v["author"] = json!({"id":"9","bot":true});
                    m.lock().unwrap().messages.push(v.clone());
                    Json(v)
                },
            ).get(|State(m):State<Arc<Mutex<Mock>>>,Path(id):Path<String>|async move {
                let m=m.lock().unwrap();
                if m.deleted.contains(&id) {return StatusCode::NOT_FOUND.into_response();}
                match m.messages.iter().rev().find(|v|v["id"]==id) {
                    Some(v)=>Json(v.clone()).into_response(),
                    None=>StatusCode::NOT_FOUND.into_response(),
                }
            }).delete(|State(m):State<Arc<Mutex<Mock>>>,Path(id):Path<String>|async move {
                let mut m=m.lock().unwrap();
                if m.fail_delete {return StatusCode::SERVICE_UNAVAILABLE;}
                m.deleted.push(id);
                StatusCode::NO_CONTENT
            }),
        )
        .route(
            "/interactions/{id}/token/callback",
            post(
                |State(m): State<Arc<Mutex<Mock>>>, Json(v): Json<Value>| async move {
                    let mut m=m.lock().unwrap();if v["type"]==5 {m.deferred_private=true;m.private_original=json!({"id":(999+m.private_sequence).to_string()});m.private_sequence+=1;}m.callbacks.push(v);
                    StatusCode::NO_CONTENT
                },
            ),
        )
        .route(
            "/webhooks/9/token/messages/@original",
            patch(
                |State(m): State<Arc<Mutex<Mock>>>, Json(v): Json<Value>| async move {
                    let mut m=m.lock().unwrap();if !m.private_original.is_object(){m.private_original=json!({"id":"999"});}for (k,val) in v.as_object().unwrap(){m.private_original[k]=val.clone();}m.deferred_private=false;let v=m.private_original.clone();m.private.push(v.clone());
                    Json(v)
                },
            ),
        )
        .route(
            "/webhooks/9/token",
            post(
                |State(m): State<Arc<Mutex<Mock>>>, Json(v): Json<Value>| async move {
                    assert_eq!(v["flags"], 64);
                    let mut m=m.lock().unwrap();let mut v=v;if m.deferred_private {v["id"]=m.private_original["id"].clone();m.private_original=v.clone();m.deferred_private=false;}else{v["id"]=json!((9000+m.private.len()).to_string());}m.private.push(v.clone());
                    Json(v)
                },
            ),
        )
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
    let tmp = tempfile::tempdir().unwrap();
    let cfg = common::config(&tmp);
    let (store, lock) = common::store(&cfg).await;
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
    store.call(true,|c|{c.execute("INSERT INTO proxy_conversations(thread_id,request_key,request_json,conversation_id,workspace_id,state) VALUES('4','cv-key','{}','conv_a','ws_a','READY')",[])?;Ok(())}).await.unwrap();
    let p = Proxy::new(endpoint.clone(), "key".into())
        .unwrap()
        .with_store(store.clone());
    p.bind_v2(&common::caps_v2()).await.unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("token".into(), endpoint).unwrap(),
        p,
    )
    .unwrap();
    app.discord.identify_bot().await.unwrap();
    (app, mock, tmp, lock, server, rid)
}
async fn local(app: &App, remote: &str) -> String {
    let r = remote.to_owned();
    app.store
        .call(false, move |c| {
            Ok(c.query_row(
                "SELECT id FROM mcp_interactions WHERE interaction_id=?1",
                [r],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}
fn interaction(id: &str, action: &str) -> Value {
    json!({"id":"200","type":3,"guild_id":"1","channel_id":"4","application_id":"9","token":"token","member":{"user":{"id":"2"}},"data":{"custom_id":format!("mcp:{id}:1:{action}")}})
}
#[test]
fn whole_flat_profile_and_limits() {
    let mut props = Map::new();
    for i in 0..32 {
        props.insert(format!("p{i}"), json!({"type":"string"}));
    }
    let s = json!({"type":"object","properties":props});
    mcp_form::validate_schema(&s).unwrap();
    let s = json!({"type":"object","required":["a","b","c","d"],"properties":{"a":{"type":"string","minLength":2,"maxLength":4},"b":{"type":"integer","minimum":-2,"maximum":2},"c":{"type":"number"},"d":{"type":"boolean"}}});
    let a = json!({"a":"😀あ","b":2,"c":0.5,"d":false});
    mcp_form::validate_answers(&s, a.as_object().unwrap()).unwrap();
    assert!(mcp_form::validate_answers(&s, json!({}).as_object().unwrap()).is_err());
    assert!(mcp_form::parse_value(&s["properties"]["b"], "2.5").is_err());
    assert!(mcp_form::parse_value(&s["properties"]["b"], "3").is_err());
    assert_eq!(
        mcp_form::parse_value(&s["properties"]["d"], "いいえ").unwrap(),
        false
    );
    assert!(mcp_form::parse_value(&s["properties"]["c"], "9007199254740992").is_err());
    let e = json!({"type":"integer","enum":(1..=64).collect::<Vec<_>>()});
    assert_eq!(mcp_form::parse_value(&e, "64").unwrap(), 64);
    assert!(mcp_form::parse_value(&json!({"type":"string"}), &"a".repeat(8192)).is_ok());
    assert!(mcp_form::parse_value(&json!({"type":"string"}), &"a".repeat(8193)).is_err());
    assert!(
        mcp_form::validate_schema(
            &json!({"type":"object","properties":{"x":{"type":"string","pattern":".*"}}})
        )
        .is_err()
    );
    assert!(
        mcp_form::validate_schema(&json!({"type":"object","properties":{"x":{"type":"array"}}}))
            .is_err()
    );
}
#[tokio::test]
async fn permission_then_form_no_automatic_answers() {
    let (app, m, tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    assert!(m.lock().unwrap().posts.is_empty());
    let mut other = interaction(&id, "accept");
    other["member"]["user"]["id"] = json!("99");
    assert!(app.handle_mcp(&other).await.is_err());
    app.handle_mcp(&interaction(&id, "accept")).await.unwrap();
    assert_eq!(
        m.lock().unwrap().posts[0]["response"],
        json!({"action":"accept","content":{}})
    );
    app.scan_mcp(&rid).await.unwrap();
    assert!(
        m.lock().unwrap().messages.last().unwrap()["components"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    app.handle_mcp(&interaction(&id, "accept")).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    m.lock().unwrap().items.push(item("int_b",json!({"type":"object","required":["name"],"properties":{"name":{"type":"string","default":"do-not-auto-send","minLength":1}}})));
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_b").await;
    app.handle_mcp(&interaction(&id, "form")).await.unwrap();
    app.handle_mcp(&interaction(&id, "submit:0")).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    app.handle_mcp(&interaction(&id, "edit:0")).await.unwrap();
    assert_eq!(m.lock().unwrap().callbacks.last().unwrap()["type"], 9);
    let mut save = interaction(&id, "save:0:0");
    save["type"] = json!(5);
    save["data"]["components"] =
        json!([{"components":[{"custom_id":"value0","value":"secret-input"}]}]);
    app.handle_mcp(&save).await.unwrap();
    app.handle_mcp(&interaction(&id, "submit:1")).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 2);
    assert_eq!(
        m.lock().unwrap().posts[1]["response"]["content"]["name"],
        "secret-input"
    );
    assert!(
        !serde_json::to_string(&m.lock().unwrap().messages)
            .unwrap()
            .contains("secret-input")
    );
    for name in ["gateway.sqlite3", "gateway.sqlite3-wal"] {
        if let Ok(b) = std::fs::read(tmp.path().join("state").join(name)) {
            assert!(!b.windows(12).any(|w| w == b"secret-input"));
        }
    }
    server.abort();
}
#[tokio::test]
async fn lost_reply_restarts_by_poll_only() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    m.lock().unwrap().lost = true;
    assert!(
        app.mcp_reply(&id, "4", 1, "accept", Map::new())
            .await
            .is_err()
    );
    let s = app.settings().await;
    let fresh = App::new(s.cfg, app.store.clone(), app.discord.clone(), s.proxy).unwrap();
    fresh.scan_mcp(&rid).await.unwrap();
    assert!(
        fresh
            .mcp_reply(&id, "4", 1, "accept", Map::new())
            .await
            .is_err()
    );
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    assert!(
        m.lock().unwrap().messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("回答をMCPへ送りました")
    );
    server.abort();
}
#[tokio::test]
async fn stale_expired_stop_and_wrong_conversation_rejected() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    assert!(
        app.mcp_reply(&id, "5", 1, "accept", Map::new())
            .await
            .is_err()
    );
    assert!(
        app.mcp_reply(&id, "4", 0, "accept", Map::new())
            .await
            .is_err()
    );
    m.lock().unwrap().items[0]["state"] = json!("expired");
    m.lock().unwrap().items[0]["revision"] = json!(2);
    assert!(
        app.mcp_reply(&id, "4", 1, "accept", Map::new())
            .await
            .is_err()
    );
    app.scan_mcp(&rid).await.unwrap();
    assert!(
        m.lock().unwrap().messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("期限")
    );
    assert!(m.lock().unwrap().posts.is_empty());
    m.lock().unwrap().items.push(item("int_b", empty()));
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_b").await;
    app.store.stop("stop-i".into(), "4".into()).await.unwrap();
    assert!(
        app.mcp_reply(&id, "4", 1, "decline", Map::new())
            .await
            .is_err()
    );
    assert!(m.lock().unwrap().posts.is_empty());
    server.abort();
}
#[tokio::test]
async fn concurrent_clicks_and_generation_change_are_fenced() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    let (a, b) = tokio::join!(
        app.mcp_reply(&id, "4", 1, "accept", Map::new()),
        app.mcp_reply(&id, "4", 1, "decline", Map::new())
    );
    assert!(a.is_ok() || b.is_ok());
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    m.lock().unwrap().items.push(item("int_b", empty()));
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_b").await;
    let mut caps = common::caps_v2();
    caps["recovery_generation"] = json!("changed");
    assert!(app.settings().await.proxy.bind_v2(&caps).await.is_err());
    assert!(
        app.mcp_reply(&id, "4", 1, "accept", Map::new())
            .await
            .is_err()
    );
    app.expire_mcp_ui().await.unwrap();
    assert!(
        m.lock().unwrap().messages.last().unwrap()["components"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    server.abort();
}
#[tokio::test]
async fn long_form_pages_and_restart_drop_only_unsent_answers() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    let mut props = Map::new();
    for n in 0..32 {
        props.insert(format!("field{n:02}"), json!({"type":"string"}));
    }
    m.lock().unwrap().items[0] = item("int_a", json!({"type":"object","properties":props}));
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    app.handle_mcp(&interaction(&id, "form")).await.unwrap();
    assert_eq!(
        m.lock().unwrap().private.last().unwrap()["components"][0]["components"][0]["options"]
            .as_array()
            .unwrap()
            .len(),
        25
    );
    app.handle_mcp(&interaction(&id, "page:1")).await.unwrap();
    assert_eq!(
        m.lock().unwrap().private.last().unwrap()["components"][0]["components"][0]["options"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    let s = app.settings().await;
    let fresh = App::new(s.cfg, app.store.clone(), app.discord.clone(), s.proxy).unwrap();
    fresh
        .handle_mcp(&interaction(&id, "submit:0"))
        .await
        .unwrap();
    assert!(m.lock().unwrap().posts.is_empty());
    fresh.handle_mcp(&interaction(&id, "form")).await.unwrap();
    let mut save = interaction(&id, "save:0:0");
    save["type"] = json!(5);
    save["data"]["components"] = json!([{"components":[{"custom_id":"value0","value":"a".repeat(4000)}]},{"components":[{"custom_id":"value1","value":"b".repeat(4000)}]},{"components":[{"custom_id":"value2","value":"c".repeat(192)}]}]);
    fresh.handle_mcp(&save).await.unwrap();
    fresh
        .handle_mcp(&interaction(&id, "submit:1"))
        .await
        .unwrap();
    assert_eq!(
        m.lock().unwrap().posts[0]["response"]["content"]["field00"]
            .as_str()
            .unwrap()
            .len(),
        8192
    );
    server.abort();
}
#[test]
fn interruption_reasons_do_not_invent_user_rejection() {
    use codex_hoshikage_gateway::application::interruption_with_evidence as explain;
    assert!(explain(Some("unsupported_interaction"), false, false).contains("未対応"));
    assert!(
        explain(Some("unsupported_interaction"), false, false)
            .contains("利用者による拒否ではありません")
    );
    assert!(explain(None, false, false).contains("理由を確定できません"));
    assert!(explain(None, true, false).contains("停止操作"));
    assert!(explain(None, false, true).contains("拒否を送信済み"));
    assert!(explain(Some("interaction_expired"), false, false).contains("期限"));
}
#[tokio::test]
async fn discord_control_loop_routes_mcp_button_without_duplicate_reply() {
    use codex_hoshikage_gateway::discord::Incoming;
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    let a = app.clone();
    let job = tokio::spawn(async move { a.control_loop(rx).await });
    tx.send(Incoming::Interaction(through_discord_library(interaction(
        &id, "accept",
    ))))
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if !m.lock().unwrap().posts.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(m.lock().unwrap().callbacks[0]["type"], 6);
    assert!(m.lock().unwrap().private.is_empty());
    app.cancel.cancel();
    job.await.unwrap().unwrap();
    server.abort();
}

fn mcp_caps() -> Value {
    let mut c = common::caps_v2();
    c["features"]["interaction_relay"] = json!(true);
    c["interaction_kinds"] = json!(["mcp_form", "mcp_url", "user_input", "permissions"]);
    c["interaction_limits"] = json!({"schema_profile":"flat-primitives-v1","max_count":16,"max_bytes":65536,"timeout_seconds":600});
    c
}
#[tokio::test]
async fn declares_only_supported_form_and_preserves_interruption_code() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    let p = app.settings().await.proxy;
    p.check().await.unwrap();
    p.start_v2(
        &app.store.request(&rid).await.unwrap(),
        "conv_a",
        json!("test"),
    )
    .await
    .unwrap();
    assert_eq!(
        m.lock().unwrap().posts[0]["interaction_capabilities"],
        json!(["mcp_form"])
    );
    assert_eq!(
        p.reconcile_v2(&app.store, &rid).await.unwrap(),
        RequestState::Cancelled
    );
    app.store
        .call(false, move |c| {
            let code: String =
                c.query_row("SELECT error_code FROM requests WHERE id=?1", [rid], |r| {
                    r.get(0)
                })?;
            assert_eq!(code, "unsupported_interaction");
            Ok(())
        })
        .await
        .unwrap();
    let mut c = mcp_caps();
    c["interaction_limits"]["schema_profile"] = json!("unknown");
    assert!(!mcp_form::supported(&c));
    server.abort();
}
#[tokio::test]
async fn schema_five_upgrade_preserves_requests_and_checks_new_migration() {
    use codex_hoshikage_gateway::storage::{self, Store};
    let tmp = tempfile::tempdir().unwrap();
    let cfg = common::config(&tmp);
    let (store, _lock) = common::store(&cfg).await;
    let id = common::queued(&store, &cfg, "10").await;
    store.call(true,|c|{c.execute_batch("DROP TABLE runtime_mode;DELETE FROM schema_migrations WHERE version=12;DROP TRIGGER direct_interaction_no_rewind;DROP TABLE direct_interactions;DELETE FROM schema_migrations WHERE version=11;DROP TRIGGER direct_dispatch_no_rewind;DROP TABLE direct_answers;DROP TABLE direct_dispatches;DROP TABLE direct_conversations;DELETE FROM schema_migrations WHERE version=10;DROP TABLE mcp_v06_decisions;DROP TABLE mcp_v06_parts;DROP TABLE mcp_v06_pages;DROP TABLE mcp_v06_views;DROP TABLE mcp_v06_runs;DELETE FROM schema_migrations WHERE version=9;DROP TABLE mcp_inline_views;DROP TABLE mcp_inline_runs;DELETE FROM schema_migrations WHERE version=8;DROP TABLE mcp_grant_revokes; DROP TABLE mcp_grant_records; DROP TABLE mcp_detail_views; DROP TABLE mcp_run_context; DELETE FROM schema_migrations WHERE version=7; ALTER TABLE requests DROP COLUMN interaction_scan_done; DROP TABLE mcp_interactions; DELETE FROM schema_migrations WHERE version=6; UPDATE schema_meta SET schema_version=5;")?;Ok(())}).await.unwrap();
    drop(store);
    let (store, _) = Store::open(&cfg).unwrap();
    assert_eq!(
        store.request(&id).await.unwrap().state,
        RequestState::Queued
    );
    assert_eq!(
        storage::validate_database(&store.path).unwrap().0,
        storage::SCHEMA
    );
    store
        .call(true, |c| {
            c.execute(
                "UPDATE schema_migrations SET checksum='bad' WHERE version=6",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(storage::validate_database(&store.path).is_err());
}
fn through_discord_library(mut v: Value) -> Value {
    v["version"] = json!(1);
    v["locale"] = json!("ja");
    v["entitlements"] = json!([]);
    v["attachment_size_limit"] = json!(10485760);
    v["member"]["user"] = json!({"id":"2","username":"test","discriminator":"0","avatar":null});
    v["member"]["roles"] = json!([]);
    v["member"]["flags"] = json!(0);
    v["member"]["joined_at"] = json!("2026-09-01T00:00:00Z");
    v["member"]["deaf"] = json!(false);
    v["member"]["mute"] = json!(false);
    if v["type"] == 3 {
        v["data"]["component_type"] = json!(2);
        v["message"] = json!({"id":"100","channel_id":"4","author":{"id":"9","username":"bot","discriminator":"0","avatar":null,"bot":true},"content":"confirm","timestamp":"2026-09-16T00:00:00Z","edited_timestamp":null,"tts":false,"mention_everyone":false,"mentions":[],"mention_roles":[],"attachments":[],"embeds":[],"pinned":false,"type":0});
    }
    let parsed: serenity::model::application::Interaction = serde_json::from_value(v).unwrap();
    assert!(serde_json::to_value(&parsed).unwrap().get("type").is_none());
    codex_hoshikage_gateway::discord::interaction_value(&parsed).unwrap()
}
#[test]
fn serenity_ingress_keeps_button_and_modal_discriminators() {
    let v = through_discord_library(interaction("local", "accept"));
    assert_eq!(v["type"], 3);
    let mut v = interaction("local", "save:0:0");
    v["type"] = json!(5);
    v["data"]["components"] =
        json!([{"type":1,"components":[{"type":4,"custom_id":"value0","value":"answer"}]}]);
    let v = through_discord_library(v);
    assert_eq!(v["type"], 5);
    assert_eq!(
        v["data"]["components"][0]["components"][0]["value"],
        "answer"
    );
}

#[test]
fn confirmation_preserves_prose_and_explains_missing_verified_details() {
    let mut form = item("int", empty())["request"].clone();
    form["message"] = json!("Allow browser_evaluate? <arbitrary text>".repeat(150));
    let text = codex_hoshikage_gateway::mcp_ui::confirmation_description(&form).unwrap();
    assert!(text.contains("MCPからの確認（原文）"));
    assert!(text.contains(form["message"].as_str().unwrap()));
    assert!(text.contains("実際の引数・実行コード"));
    assert!(text.contains("「拒否」"));
    assert!(text.contains("/stop"));
    form["_meta"] = Value::Null;
    let text = codex_hoshikage_gateway::mcp_ui::confirmation_description(&form).unwrap();
    assert!(text.contains("入力・確認"));
    assert!(!text.contains("ツールの実行許可が必要"));
}

#[tokio::test]
async fn five_confirmed_permissions_compact_without_automatic_accept_or_reposts() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    for index in 0..5 {
        let remote = if index == 0 {
            "int_a".to_owned()
        } else {
            format!("int_{index}")
        };
        if index > 0 {
            m.lock().unwrap().items.push(item(&remote, empty()));
        }
        app.scan_mcp(&rid).await.unwrap();
        assert_eq!(m.lock().unwrap().posts.len(), index);
        let id = local(&app, &remote).await;
        app.mcp_reply(&id, "4", 1, "accept", Map::new())
            .await
            .unwrap();
        {
            let mut m = m.lock().unwrap();
            let v = m
                .items
                .iter_mut()
                .find(|v| v["interaction_id"] == remote)
                .unwrap();
            v["state"] = json!("resolved");
            v["revision"] = json!(3);
        }
        app.scan_mcp(&rid).await.unwrap();
    }
    let snapshots = m.lock().unwrap().messages.len();
    app.scan_mcp(&rid).await.unwrap();
    assert_eq!(m.lock().unwrap().messages.len(), snapshots);
    assert_eq!(m.lock().unwrap().posts.len(), 5);
    app.store.call(false, |c| {
        assert_eq!(c.query_row("SELECT count(*) FROM mcp_interactions WHERE closed=1",[],|r|r.get::<_,i64>(0))?,5);
        assert_eq!(c.query_row("SELECT count(*) FROM deliveries WHERE kind='mcp_summary' AND state='CONFIRMED'",[],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(c.query_row("SELECT count(*) FROM deliveries WHERE kind IN ('mcp_action','mcp_description') AND state!='DELETED'",[],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    let m = m.lock().unwrap();
    assert!(m.messages.iter().rev().any(|v| {
        v["content"]
            .as_str()
            .unwrap_or("")
            .contains("5件を送信済み")
    }));
    assert_eq!(m.deleted.len(), 10);
    server.abort();
}

#[tokio::test]
async fn compact_cleanup_recovers_after_restart_without_new_permission() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    app.mcp_reply(&id, "4", 1, "accept", Map::new())
        .await
        .unwrap();
    {
        let mut m = m.lock().unwrap();
        m.items[0]["state"] = json!("resolved");
        m.fail_delete = true;
    }
    app.scan_mcp(&rid).await.unwrap();
    app.store
        .call(false, |c| {
            assert_eq!(
                c.query_row("SELECT closed FROM mcp_interactions", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    let s = app.settings().await;
    let fresh = App::new(s.cfg, app.store.clone(), app.discord.clone(), s.proxy).unwrap();
    m.lock().unwrap().fail_delete = false;
    fresh.scan_mcp(&rid).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    fresh
        .store
        .call(false, |c| {
            assert_eq!(
                c.query_row("SELECT closed FROM mcp_interactions", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn resolved_without_our_confirmed_accept_is_not_reported_as_allowed() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    m.lock().unwrap().items[0]["state"] = json!("resolved");
    app.scan_mcp(&rid).await.unwrap();
    assert!(m.lock().unwrap().posts.is_empty());
    assert!(m.lock().unwrap().deleted.is_empty());
    app.store
        .call(false, |c| {
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM deliveries WHERE kind='mcp_summary'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn unknown_confirmation_is_retained_until_later_verified_resolution() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    app.mcp_reply(&id, "4", 1, "accept", Map::new())
        .await
        .unwrap();
    m.lock().unwrap().items[0]["state"] = json!("unknown");
    app.scan_mcp(&rid).await.unwrap();
    assert!(m.lock().unwrap().deleted.is_empty());
    assert!(
        m.lock().unwrap().messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("不明")
    );
    m.lock().unwrap().items[0]["state"] = json!("resolved");
    app.scan_mcp(&rid).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    assert_eq!(m.lock().unwrap().deleted.len(), 2);
    server.abort();
}

#[tokio::test]
async fn terminal_run_keeps_incomplete_cleanup_recoverable() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    app.mcp_reply(&id, "4", 1, "accept", Map::new())
        .await
        .unwrap();
    m.lock().unwrap().items[0]["state"] = json!("unknown");
    app.scan_mcp(&rid).await.unwrap();
    app.store
        .observe(rid.clone(), RequestState::Completed, "test", true)
        .await
        .unwrap();
    {
        let mut m = m.lock().unwrap();
        m.items[0]["state"] = json!("resolved");
        m.fail_delete = true;
    }
    app.scan_mcp(&rid).await.unwrap();
    app.expire_mcp_ui().await.unwrap();
    app.store
        .call(false, |c| {
            assert_eq!(
                c.query_row("SELECT closed FROM mcp_interactions", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                c.query_row("SELECT interaction_scan_done FROM requests", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    let s = app.settings().await;
    let fresh = App::new(s.cfg, app.store.clone(), app.discord.clone(), s.proxy).unwrap();
    m.lock().unwrap().fail_delete = false;
    fresh.expire_mcp_ui().await.unwrap();
    fresh.scan_mcp(&rid).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    assert_eq!(m.lock().unwrap().deleted.len(), 2);
    fresh
        .store
        .call(false, |c| {
            assert_eq!(
                c.query_row("SELECT interaction_scan_done FROM requests", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn summary_counts_only_its_request_and_does_not_delete_other_cards() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let id = local(&app, "int_a").await;
    let s = app.settings().await;
    let other = common::queued(&app.store, &s.cfg, "20").await;
    app.store.call(true,move|c|{
        c.execute("INSERT INTO mcp_interactions(id,interaction_id,request_id,response_id,conversation_id,workspace_id,instance_id,generation,base_url,revision,request_digest,expires_at,state,closed,action,operation_state) SELECT 'other','int_other',?1,'resp_other','conv_other',workspace_id,instance_id,generation,base_url,revision,request_digest,expires_at,'resolved',1,'accept','succeeded' FROM mcp_interactions LIMIT 1",[other])?;Ok(())
    }).await.unwrap();
    assert!(
        app.delivery
            .text(
                "other",
                "4",
                "mcp_description",
                0,
                "別の作業の確認",
                json!([])
            )
            .await
            .unwrap()
    );
    app.mcp_reply(&id, "4", 1, "accept", Map::new())
        .await
        .unwrap();
    m.lock().unwrap().items[0]["state"] = json!("resolved");
    app.scan_mcp(&rid).await.unwrap();
    let m = m.lock().unwrap();
    assert!(m.messages.iter().any(|v| {
        v["content"]
            .as_str()
            .unwrap_or("")
            .contains("1件を送信済み")
    }));
    assert!(!m.messages.iter().any(|v| {
        v["content"]
            .as_str()
            .unwrap_or("")
            .contains("2件を送信済み")
    }));
    assert_eq!(m.deleted.len(), 2);
    server.abort();
}

async fn turn_setup() -> (
    App,
    Arc<Mutex<Mock>>,
    tempfile::TempDir,
    codex_hoshikage_gateway::storage::StateLock,
    tokio::task::JoinHandle<()>,
    String,
) {
    let (app, m, tmp, lock, server, rid) = setup().await;
    m.lock().unwrap().turn_caps = true;
    app.settings().await.proxy.check().await.unwrap();
    let id = rid.clone();
    app.store
        .call(true, move |c| {
            c.execute(
                "INSERT INTO mcp_run_context VALUES(?1,'principal','channel',?1)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let scope = json!({"instance_id":"pxy_test","recovery_generation":"gen_test","context":{"principal_id":"principal","channel_id":"channel","run_id":rid},"response_id":"resp_a","conversation_id":"conv_a","workspace_id":"ws_a","turn_id":"turn_a","input_generation":0,"config_generation":"cfg1","server":"lightpanda","tool":"markdown"});
    m.lock().unwrap().operation = json!({"interaction_id":"int_a","response_id":"resp_a","turn_id":"turn_a","call_id":"call1","server":"lightpanda","tool":"markdown","config_generation":"cfg1","scope_fingerprint":"fp1","input_generation":0,"binding_status":"verified","unavailable_reason":null,"scope":scope,"revision":1,"turn_grant_eligible":true,"ineligible_reason":null,"arguments":{"url":"private-operation-secret"},"redacted_paths":[],"disclosure":"requester_only"});
    app.scan_mcp(&rid).await.unwrap();
    (app, m, tmp, lock, server, rid)
}
fn turn_event(custom: String) -> Value {
    let mut e = interaction("unused", "accept");
    e["data"]["custom_id"] = json!(custom);
    e
}
#[tokio::test]
async fn private_details_explicit_scope_and_no_duplicate_reply() {
    for turn in [false, true] {
        let (app, m, _tmp, _lock, server, _rid) = turn_setup().await;
        let id = local(&app, "int_a").await;
        app.handle_mcp_turn(&turn_event(format!("mt:details:{id}:1")))
            .await
            .unwrap();
        assert!(
            !serde_json::to_string(&m.lock().unwrap().messages)
                .unwrap()
                .contains("private-operation-secret")
        );
        assert!(
            serde_json::to_string(&m.lock().unwrap().private)
                .unwrap()
                .contains("private-operation-secret")
        );
        let view: String = app
            .store
            .call(false, |c| {
                Ok(c.query_row("SELECT id FROM mcp_detail_views", [], |r| r.get(0))?)
            })
            .await
            .unwrap();
        let e = turn_event(format!("mt:{}:{view}", if turn { "turn" } else { "once" }));
        m.lock().unwrap().lost = turn;
        let (first, second) = tokio::join!(app.handle_mcp_turn(&e), app.handle_mcp_turn(&e));
        first.unwrap();
        second.unwrap();
        app.store
            .call(false, |c| {
                let mut q = c.prepare("SELECT fingerprint,scope_digest FROM mcp_detail_views")?;
                let records = q
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert!(!serde_json::to_string(&records)?.contains("private-operation-secret"));
                Ok(())
            })
            .await
            .unwrap();
        let m = m.lock().unwrap();
        assert_eq!(m.posts.len(), 1);
        assert_eq!(m.posts[0]["expected_scope_fingerprint"], "fp1");
        assert_eq!(
            m.posts[0]["grant_scope"],
            if turn {
                json!("turn_tool")
            } else {
                Value::Null
            }
        );
        server.abort();
    }
}
#[tokio::test]
async fn stale_private_view_and_foreign_scope_cannot_approve() {
    for change in [
        "scope_fingerprint",
        "scope",
        "tool",
        "instance_id",
        "recovery_generation",
        "response_id",
        "conversation_id",
        "workspace_id",
        "turn_id",
        "config_generation",
        "input_generation",
        "ineligible_reason",
    ] {
        let (app, m, _tmp, _lock, server, _rid) = turn_setup().await;
        let id = local(&app, "int_a").await;
        app.handle_mcp_turn(&turn_event(format!("mt:details:{id}:1")))
            .await
            .unwrap();
        let view: String = app
            .store
            .call(false, |c| {
                Ok(c.query_row("SELECT id FROM mcp_detail_views", [], |r| r.get(0))?)
            })
            .await
            .unwrap();
        {
            let mut m = m.lock().unwrap();
            match change {
                "scope" => m.operation["scope"]["context"]["run_id"] = json!("other-run"),
                "tool" => {
                    m.operation["tool"] = json!("browser_evaluate");
                    m.operation["scope"]["tool"] = json!("browser_evaluate");
                }
                "scope_fingerprint" => m.operation["scope_fingerprint"] = json!("changed"),
                "ineligible_reason" => m.operation["ineligible_reason"] = json!("unrecognized"),
                key => m.operation["scope"][key] = json!("changed"),
            }
        }
        app.handle_mcp_turn(&turn_event(format!("mt:turn:{view}")))
            .await
            .unwrap();
        assert!(m.lock().unwrap().posts.is_empty());
        server.abort();
    }
}

#[tokio::test]
async fn lost_revoke_is_reconciled_after_restart_without_reposting() {
    let (app, m, _tmp, _lock, server, rid) = turn_setup().await;
    {
        let mut m = m.lock().unwrap();
        let scope = m.operation["scope"].clone();
        m.grants = vec![
            json!({"grant_id":"grant1","scope":scope,"state":"active","created_at":"2026-09-16T00:00:00Z","expires_at":"2099-01-01T00:00:00Z","application_count":3,"initial_interaction_id":"int_a"}),
        ];
    }
    let menu = turn_event(format!("mt:grants:{rid}"));
    app.handle_mcp_turn(&menu).await.unwrap();
    let id: String = app
        .store
        .call(false, |c| {
            Ok(c.query_row("SELECT id FROM mcp_grant_records", [], |r| r.get(0))?)
        })
        .await
        .unwrap();
    app.handle_mcp_turn(&turn_event(format!("mt:revoke:{id}")))
        .await
        .unwrap();
    assert_eq!(m.lock().unwrap().revoke_keys.len(), 1);
    let s = app.settings().await;
    let fresh = App::new(s.cfg, app.store.clone(), app.discord.clone(), s.proxy).unwrap();
    fresh.handle_mcp_turn(&menu).await.unwrap();
    let result: (String, i64) = app
        .store
        .call(false, |c| {
            Ok(c.query_row(
                "SELECT state,in_flight_count FROM mcp_grant_revokes",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(result, ("succeeded".into(), 2));
    assert_eq!(m.lock().unwrap().revoke_keys.len(), 1);
    server.abort();
}
#[tokio::test]
async fn enabled_start_carries_persisted_context_and_unsafe_tool_has_no_turn_button() {
    let (app, m, _tmp, _lock, server, rid) = turn_setup().await;
    let p = app.settings().await.proxy;
    p.start_v2(
        &app.store.request(&rid).await.unwrap(),
        "conv_a",
        json!("test"),
    )
    .await
    .unwrap();
    assert_eq!(
        m.lock().unwrap().posts[0]["approval_context"],
        json!({"principal_id":"principal","channel_id":"channel","run_id":rid})
    );
    {
        let mut m = m.lock().unwrap();
        m.operation["tool"] = json!("browser_evaluate");
        m.operation["scope"]["tool"] = json!("browser_evaluate");
    }
    let id = local(&app, "int_a").await;
    app.handle_mcp_turn(&turn_event(format!("mt:details:{id}:1")))
        .await
        .unwrap();
    let data = serde_json::to_string(&m.lock().unwrap().private).unwrap();
    assert!(data.contains("mt:once:"));
    assert!(!data.contains("mt:turn:"));
    assert!(
        app.mcp_reply(&id, "4", 1, "accept", Map::new())
            .await
            .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn schema_six_upgrade_preserves_existing_interaction() {
    use codex_hoshikage_gateway::storage::{self, Store};
    let (app, _m, _tmp, _lock, server, rid) = setup().await;
    app.scan_mcp(&rid).await.unwrap();
    let cfg = app.settings().await.cfg;
    app.store.call(true,|c|{c.execute_batch("DROP TABLE runtime_mode;DELETE FROM schema_migrations WHERE version=12;DROP TRIGGER direct_interaction_no_rewind;DROP TABLE direct_interactions;DELETE FROM schema_migrations WHERE version=11;DROP TRIGGER direct_dispatch_no_rewind;DROP TABLE direct_answers;DROP TABLE direct_dispatches;DROP TABLE direct_conversations;DELETE FROM schema_migrations WHERE version=10;DROP TABLE mcp_v06_decisions;DROP TABLE mcp_v06_parts;DROP TABLE mcp_v06_pages;DROP TABLE mcp_v06_views;DROP TABLE mcp_v06_runs;DELETE FROM schema_migrations WHERE version=9;DROP TABLE mcp_inline_views;DROP TABLE mcp_inline_runs;DELETE FROM schema_migrations WHERE version=8;DROP TABLE mcp_grant_revokes;DROP TABLE mcp_grant_records;DROP TABLE mcp_detail_views;DROP TABLE mcp_run_context;ALTER TABLE mcp_interactions DROP COLUMN grant_scope;DELETE FROM schema_migrations WHERE version=7;UPDATE schema_meta SET schema_version=6;")?;Ok(())}).await.unwrap();
    drop(app);
    let (store, _) = Store::open(&cfg).unwrap();
    assert_eq!(
        storage::validate_database(&store.path).unwrap().0,
        storage::SCHEMA
    );
    store.call(false,|c|{let n:i64=c.query_row("SELECT count(*) FROM mcp_interactions WHERE interaction_id='int_a' AND grant_scope IS NULL",[],|r|r.get(0))?;assert_eq!(n,1);Ok(())}).await.unwrap();
    server.abort();
}
#[test]
fn capabilities_and_high_risk_are_fail_closed() {
    use codex_hoshikage_gateway::mcp_grants::{Capabilities, turn_eligible};
    assert!(!Capabilities::parse(&json!({})).details);
    let mut c = json!({"mcp_operation_details":{"enabled":true,"profile":"native-item-id-v1","max_argument_bytes":65536,"disclosure":"requester_only"},"mcp_turn_approval":{"enabled":true,"profile":"native-item-id-v1","max_grants":16,"ttl_seconds":600,"max_records":256}});
    assert!(Capabilities::parse(&c).turn);
    c["mcp_turn_approval"]["profile"] = json!("unknown");
    assert!(!Capabilities::parse(&c).turn);
    c["mcp_operation_details"]["enabled"] = json!("true");
    assert!(!Capabilities::parse(&c).details);
    let caps = Capabilities {
        inline: false,
        details: true,
        turn: true,
    };
    let mut operation = json!({"turn_grant_eligible":true,"binding_status":"verified","unavailable_reason":null,"ineligible_reason":null,"scope":{"context":{}},"redacted_paths":[],"tool":"browser_evaluate"});
    assert!(!turn_eligible(&operation, caps));
    operation["tool"] = json!("browser_run_code_unsafe");
    assert!(!turn_eligible(&operation, caps));
    operation["tool"] = json!("browser_snapshot");
    assert!(turn_eligible(&operation, caps));
    operation["redacted_paths"] = json!(["/secret"]);
    assert!(!turn_eligible(&operation, caps));
}

fn inline_capability() -> Value {
    json!({"enabled":true,"profile":"source-conversation-v1","max_response_bytes":32768,"max_display_text_utf16_units":1400,"max_display_fields":8,"max_presentations_per_interaction":4})
}
fn presentation() -> Value {
    json!({"interaction_id":"int_a","response_id":"resp_a","turn_id":"turn_a","revision":1,"scope_fingerprint":"fp1","presentation_id":"present1","presentation_fingerprint":"pt1","profile":"source-conversation-v1","renderer":"browser-find-v1","audience":{"kind":"source_conversation","channel_id":"channel"},"state":"inline","reason":null,"expires_at":(time::OffsetDateTime::now_utc()+time::Duration::minutes(5)).format(&time::format_description::well_known::Rfc3339).unwrap(),"display":{"disclosure":"source_conversation","provenance":"proxy_verified_call","title":"ページ内を検索","fields":[{"label":"検索語","value":"ノートパソコン"}],"limitations":["現在のページを検索します"],"omissions":[]},"actions":{"allow_once":true,"allow_turn_tool":true,"decline":true,"open_private_details":false}})
}
async fn enable_inline(app: &App, m: &Arc<Mutex<Mock>>, rid: &str) {
    {
        let mut m = m.lock().unwrap();
        m.inline_caps = true;
        m.presentation = presentation();
        m.operation["tool"] = json!("browser_find");
        m.operation["scope"]["tool"] = json!("browser_find");
    }
    app.settings().await.proxy.check().await.unwrap();
    let r = rid.to_owned();
    app.store
        .call(true, move |c| {
            c.execute("INSERT INTO mcp_inline_runs VALUES(?1)", [r])?;
            Ok(())
        })
        .await
        .unwrap();
    app.scan_mcp(rid).await.unwrap();
}
fn inline_event(m: &Arc<Mutex<Mock>>, action: &str) -> Value {
    let m = m.lock().unwrap();
    let msg = m
        .messages
        .iter()
        .rev()
        .find(|m| m["components"].to_string().contains("mi:"))
        .unwrap();
    let custom = msg["components"][0]["components"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|c| c["custom_id"].as_str().filter(|s| s.ends_with(action)))
        .unwrap();
    let mut e = turn_event(custom.into());
    e["message"] = json!({"id":msg["id"]});
    e
}
#[tokio::test]
async fn inline_first_card_explicit_scope_and_duplicate_click() {
    for turn in [false, true] {
        let (app, m, _tmp, _lock, server, rid) = turn_setup().await;
        enable_inline(&app, &m, &rid).await;
        let event = inline_event(&m, if turn { "turn" } else { "once" });
        {
            let m = m.lock().unwrap();
            let card = m
                .messages
                .iter()
                .rev()
                .find(|x| x["id"] == event["message"]["id"])
                .unwrap();
            assert!(card["content"].as_str().unwrap().contains("ノートパソコン"));
            assert_eq!(
                card["components"][0]["components"]
                    .as_array()
                    .unwrap()
                    .len(),
                3
            );
            assert!(!card.to_string().contains("private-operation-secret"));
            assert!(!card.to_string().contains("mt:details"));
            assert!(!card.to_string().contains("pt1"));
        }
        let (a, b) = tokio::join!(app.handle_inline_mcp(&event), app.handle_inline_mcp(&event));
        a.unwrap();
        b.unwrap();
        {
            let m = m.lock().unwrap();
            assert_eq!(m.posts.len(), 1);
            assert_eq!(m.posts[0]["approval_view"], "source_conversation");
            assert_eq!(m.posts[0]["expected_presentation_fingerprint"], "pt1");
            assert_eq!(
                m.posts[0]["grant_scope"],
                if turn {
                    json!("turn_tool")
                } else {
                    Value::Null
                }
            );
        }
        server.abort();
    }
}
#[tokio::test]
async fn inline_stale_message_scope_pending_delivery_and_foreign_user_fail_closed() {
    for case in 0..8 {
        let (app, m, _tmp, _lock, server, rid) = turn_setup().await;
        enable_inline(&app, &m, &rid).await;
        let mut e = inline_event(&m, "once");
        match case {
            0 => e["message"]["id"] = json!("99999"),
            1 => e["member"]["user"]["id"] = json!("other"),
            2 => m.lock().unwrap().presentation["presentation_fingerprint"] = json!("changed"),
            3 => m.lock().unwrap().presentation["display"]["title"] = json!("変わった内容"),
            4 => m.lock().unwrap().presentation["audience"]["channel_id"] = json!("another"),
            5 => {
                app.store
                    .call(true, |c| {
                        c.execute(
                            "UPDATE deliveries SET state='PATCH_PENDING' WHERE kind='mcp_action'",
                            [],
                        )?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            6 => m.lock().unwrap().presentation["expires_at"] = json!("2000-01-01T00:00:00Z"),
            _ => e["channel_id"] = json!("other"),
        }
        let _ = app.handle_inline_mcp(&e).await;
        assert!(m.lock().unwrap().posts.is_empty(), "case {case}");
        server.abort();
    }
}
#[tokio::test]
async fn inline_private_unknown_omissions_and_large_display_never_publish_arguments() {
    for case in 0..5 {
        let (app, m, _tmp, _lock, server, rid) = turn_setup().await;
        enable_inline(&app, &m, &rid).await;
        {
            let mut m = m.lock().unwrap();
            m.presentation["presentation_id"] = json!("present2");
            m.presentation["presentation_fingerprint"] = json!("pt2");
            match case {
                0 => {
                    let p = &mut m.presentation;
                    p["state"] = json!("private_required");
                    p["reason"] = json!("private_arguments");
                    p["renderer"] = Value::Null;
                    p["display"]["title"] = json!("補足確認が必要です");
                    p["display"]["fields"] = json!([]);
                    p["display"]["omissions"] = json!(["private_arguments"]);
                    p["actions"] = json!({"allow_once":false,"allow_turn_tool":false,"decline":true,"open_private_details":true});
                }
                1 => m.presentation["renderer"] = json!("unknown"),
                2 => m.presentation["display"]["omissions"] = json!(["private_arguments"]),
                3 => m.presentation["display"]["fields"][0]["value"] = json!("`".repeat(1100)),
                _ => m.presentation["actions"]["allow_once"] = json!("true"),
            }
        }
        app.scan_mcp(&rid).await.unwrap();
        {
            let m = m.lock().unwrap();
            let public = m
                .messages
                .iter()
                .rev()
                .find(|v| v["components"].to_string().contains(":decline"))
                .unwrap();
            assert!(!public["components"].to_string().contains("mi:"));
            assert!(
                !serde_json::to_string(&m.messages)
                    .unwrap()
                    .contains("private-operation-secret")
            );
        }
        let active: i64 = app
            .store
            .call(false, |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM mcp_inline_views WHERE active=1",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(active, 0);
        server.abort();
    }
}
#[tokio::test]
async fn inline_once_only_and_lost_reply_reopen_without_resend() {
    let (app, m, _tmp, _lock, server, rid) = turn_setup().await;
    enable_inline(&app, &m, &rid).await;
    m.lock().unwrap().presentation["actions"]["allow_turn_tool"] = json!(false);
    m.lock().unwrap().presentation["presentation_fingerprint"] = json!("pt2");
    m.lock().unwrap().presentation["presentation_id"] = json!("present2");
    app.scan_mcp(&rid).await.unwrap();
    let e = inline_event(&m, "once");
    {
        let m = m.lock().unwrap();
        let card = m
            .messages
            .iter()
            .find(|x| x["id"] == e["message"]["id"])
            .unwrap();
        assert_eq!(
            card["components"][0]["components"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
    m.lock().unwrap().lost = true;
    app.handle_inline_mcp(&e).await.unwrap();
    let settings = app.settings().await;
    let fresh = App::new(
        settings.cfg.clone(),
        app.store.clone(),
        app.discord.clone(),
        settings.proxy.clone(),
    )
    .unwrap();
    fresh.scan_mcp(&rid).await.unwrap();
    let _ = fresh.handle_inline_mcp(&e).await;
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    server.abort();
}
#[test]
fn inline_capability_and_payload_validation_are_strict() {
    use codex_hoshikage_gateway::{mcp_grants::Capabilities, mcp_inline::validate_presentation};
    let mut c = json!({"mcp_inline_approval":inline_capability()});
    assert!(Capabilities::parse(&c).inline);
    c["mcp_inline_approval"]["max_response_bytes"] = json!("32768");
    assert!(!Capabilities::parse(&c).inline);
    let p = presentation();
    validate_presentation(&p).unwrap();
    for key in [
        "display",
        "actions",
        "audience",
        "renderer",
        "scope_fingerprint",
    ] {
        let mut q = p.clone();
        q.as_object_mut().unwrap().remove(key);
        assert!(validate_presentation(&q).is_err());
    }
    let mut p = p;
    p["display"]["fields"][0]["value"] = json!("😀".repeat(701));
    assert!(validate_presentation(&p).is_err());
}

#[tokio::test]
async fn inline_start_declares_mode_and_refuses_capability_downgrade() {
    let (app, m, _tmp, _lock, server, rid) = turn_setup().await;
    enable_inline(&app, &m, &rid).await;
    let p = app.settings().await.proxy;
    let r = app.store.request(&rid).await.unwrap();
    p.start_v2(&r, "conv_a", json!("test")).await.unwrap();
    assert_eq!(
        m.lock().unwrap().posts[0]["approval_presentation"],
        json!({"mode":"source_conversation"})
    );
    m.lock().unwrap().inline_caps = false;
    p.check().await.unwrap();
    assert!(p.start_v2(&r, "conv_a", json!("test")).await.is_err());
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    server.abort();
}
#[tokio::test]
async fn inline_display_reopen_requires_same_confirmed_card() {
    let (app, m, _tmp, _lock, server, rid) = turn_setup().await;
    enable_inline(&app, &m, &rid).await;
    let e = inline_event(&m, "once");
    let s = app.settings().await;
    let fresh = App::new(
        s.cfg.clone(),
        app.store.clone(),
        app.discord.clone(),
        s.proxy.clone(),
    )
    .unwrap();
    fresh.handle_inline_mcp(&e).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    server.abort();
}
#[tokio::test]
async fn schema_seven_upgrade_keeps_approval_context_and_old_requests_private() {
    use codex_hoshikage_gateway::storage::{self, Store};
    let (app, _m, _tmp, _lock, server, rid) = turn_setup().await;
    let s = app.settings().await;
    app.store.call(true,|c|{c.execute_batch("DROP TABLE runtime_mode;DELETE FROM schema_migrations WHERE version=12;DROP TRIGGER direct_interaction_no_rewind;DROP TABLE direct_interactions;DELETE FROM schema_migrations WHERE version=11;DROP TRIGGER direct_dispatch_no_rewind;DROP TABLE direct_answers;DROP TABLE direct_dispatches;DROP TABLE direct_conversations;DELETE FROM schema_migrations WHERE version=10;DROP TABLE mcp_v06_decisions;DROP TABLE mcp_v06_parts;DROP TABLE mcp_v06_pages;DROP TABLE mcp_v06_views;DROP TABLE mcp_v06_runs;DELETE FROM schema_migrations WHERE version=9;DROP TABLE mcp_inline_views;DROP TABLE mcp_inline_runs;DELETE FROM schema_migrations WHERE version=8;UPDATE schema_meta SET schema_version=7;")?;Ok(())}).await.unwrap();
    drop(app);
    let (store, _) = Store::open(&s.cfg).unwrap();
    assert_eq!(
        storage::validate_database(&store.path).unwrap().0,
        storage::SCHEMA
    );
    store
        .call(false, move |c| {
            assert_eq!(
                c.query_row(
                    "SELECT run_id FROM mcp_run_context WHERE request_id=?1",
                    [&rid],
                    |r| r.get::<_, String>(0)
                )?,
                rid
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM mcp_inline_runs", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    server.abort();
}
#[tokio::test]
async fn inline_unknown_proxy_does_not_advertise_inline_on_existing_requests() {
    let (app, m, _tmp, _lock, server, rid) = turn_setup().await;
    m.lock().unwrap().inline_caps = true;
    app.settings().await.proxy.check().await.unwrap();
    app.scan_mcp(&rid).await.unwrap();
    assert!(
        !serde_json::to_string(&m.lock().unwrap().messages)
            .unwrap()
            .contains("mi:")
    );
    server.abort();
}

fn v06_examples() -> Value {
    serde_json::from_str(include_str!("fixtures/mcp_v06_examples.json")).unwrap()
}
fn v06_page(key: &str, rid: &str) -> Value {
    let mut p = v06_examples()[key].clone();
    p["interaction_id"] = json!("int_a");
    p["response_id"] = json!("resp_a");
    p["turn_id"] = json!("turn_a");
    p["expires_at"] = json!("2099-01-01T00:00:00Z");
    p["audience"]["channel_id"] = json!("channel");
    if p["audience"]["kind"] == "requester" {
        p["audience"]["principal_id"] = json!("principal");
    }
    if p["scope"].is_object() {
        p["scope"]["instance_id"] = json!("pxy_test");
        p["scope"]["recovery_generation"] = json!("gen_test");
        p["scope"]["context"] =
            json!({"principal_id":"principal","channel_id":"channel","run_id":rid});
        p["scope"]["response_id"] = json!("resp_a");
        p["scope"]["turn_id"] = json!("turn_a");
        p["scope"]["conversation_id"] = json!("conv_a");
        p["scope"]["workspace_id"] = json!("ws_a");
    }
    p
}
async fn enable_v06(app: &App, m: &Arc<Mutex<Mock>>, rid: &str, key: &str) {
    let mut p = v06_page(key, rid);
    if key == "presentation_unreviewed" {
        p["audience"]["kind"] = json!("source_conversation");
        p["audience"]["principal_id"] = Value::Null;
        p["display"]["disclosure"] = json!("source_conversation");
    }
    let selection = serde_json::to_string(&p["execution_policy"]["selection"]).unwrap();
    {
        let mut m = m.lock().unwrap();
        m.v06_caps = true;
        m.turn_caps = false;
        m.inline_caps = false;
        m.presentation = p;
    }
    app.settings().await.proxy.check().await.unwrap();
    let rid = rid.to_owned();
    app.store.call(true,move|c|{c.execute("INSERT INTO mcp_v06_runs(request_id,profile,selection_json) VALUES(?1,'source-conversation-v3',?2)",rusqlite::params![rid,selection])?;c.execute("INSERT OR IGNORE INTO mcp_run_context VALUES(?1,'principal','channel',?1)",[rid])?;Ok(())}).await.unwrap();
    app.scan_mcp(&app.store.active("4").await.unwrap().unwrap().id)
        .await
        .unwrap();
}
fn v06_event(m: &Arc<Mutex<Mock>>, action: &str, private: bool) -> Value {
    let m = m.lock().unwrap();
    let messages = if private { &m.private } else { &m.messages };
    let (msg, custom) = messages
        .iter()
        .rev()
        .find_map(|msg| {
            msg["components"]
                .as_array()?
                .iter()
                .flat_map(|r| r["components"].as_array().into_iter().flatten())
                .find_map(|c| {
                    c["custom_id"]
                        .as_str()
                        .filter(|id| id.starts_with(&format!("ma6:{action}:")))
                        .map(|id| (msg, id.to_owned()))
                })
        })
        .unwrap_or_else(|| {
            panic!(
                "v06 button {action}: {}",
                serde_json::to_string(messages).unwrap()
            )
        });
    let mut e = turn_event(custom);
    e["message"] = json!({"id":msg["id"]});
    e
}
#[tokio::test]
async fn v06_public_once_turn_and_duplicate_click_are_bound() {
    for turn in [false, true] {
        let (app, m, _tmp, _lock, server, rid) = setup().await;
        enable_v06(
            &app,
            &m,
            &rid,
            if turn {
                "presentation_turn_eligible"
            } else {
                "presentation_unreviewed"
            },
        )
        .await;
        let event = v06_event(&m, if turn { "turn" } else { "once" }, false);
        {
            let m = m.lock().unwrap();
            assert!(m.messages.iter().any(|v| {
                v["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("action") || s.contains("操作"))
            }));
            assert!(
                !m.messages
                    .iter()
                    .any(|v| v["components"].to_string().contains("ma6:private:"))
            );
        }
        let (a, b) = tokio::join!(app.handle_v06_mcp(&event), app.handle_v06_mcp(&event));
        a.unwrap();
        b.unwrap();
        let m = m.lock().unwrap();
        assert_eq!(m.posts.len(), 1);
        assert_eq!(m.posts[0]["approval_view"], "source_conversation");
        assert_eq!(
            m.posts[0]["expected_policy_binding_id"],
            m.presentation["execution_policy"]["binding_id"]
        );
        assert_eq!(
            m.posts[0]["expected_page_tokens"],
            json!([m.presentation["page"]["token"]])
        );
        assert_eq!(
            m.posts[0]["grant_scope"],
            if turn {
                json!("turn_tool")
            } else {
                Value::Null
            }
        );
        server.abort();
    }
}
#[tokio::test]
async fn v06_stale_card_display_audience_and_stop_cannot_permit() {
    for case in 0..7 {
        let (app, m, _tmp, _lock, server, rid) = setup().await;
        enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
        let mut event = v06_event(&m, "once", false);
        match case {
            0 => event["message"]["id"] = json!("99999"),
            1 => event["member"]["user"]["id"] = json!("3"),
            2 => m.lock().unwrap().presentation["presentation_fingerprint"] = json!("other"),
            3 => m.lock().unwrap().presentation["display"]["title"] = json!("別の内容"),
            4 => m.lock().unwrap().presentation["audience"]["channel_id"] = json!("other"),
            5 => {
                let id = rid.clone();
                app.store
                    .call(true, move |c| {
                        c.execute("UPDATE requests SET stop_requested=1 WHERE id=?1", [id])?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            _ => {
                m.lock().unwrap().presentation["scope"]["execution_policy_binding_id"] =
                    json!("other")
            }
        }
        let _ = app.handle_v06_mcp(&event).await;
        assert!(m.lock().unwrap().posts.is_empty(), "case {case}");
        server.abort();
    }
}
#[tokio::test]
async fn v06_decline_does_not_need_presentation_or_schema() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let event = v06_event(&m, "decline", false);
    {
        let mut m = m.lock().unwrap();
        m.presentation = Value::Null;
        m.items[0]["request"]["requestedSchema"] = Value::Null;
    }
    app.handle_v06_mcp(&event).await.unwrap();
    assert_eq!(
        m.lock().unwrap().posts,
        vec![json!({"expected_revision":1,"response":{"action":"decline"}})]
    );
    server.abort();
}
#[tokio::test]
async fn v06_public_restart_recovers_receipts_and_unknown_reply_is_not_resent() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let s = app.settings().await;
    let fresh = App::new(s.cfg, app.store.clone(), app.discord.clone(), s.proxy).unwrap();
    let before = m.lock().unwrap().messages.len();
    fresh.scan_mcp(&rid).await.unwrap();
    assert_eq!(m.lock().unwrap().messages.len(), before);
    let event = v06_event(&m, "once", false);
    m.lock().unwrap().lost = true;
    fresh.handle_v06_mcp(&event).await.unwrap();
    fresh.handle_v06_mcp(&event).await.unwrap();
    fresh.scan_mcp(&rid).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    server.abort();
}
#[tokio::test]
async fn v06_private_pages_are_explicit_and_require_all_receipts() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_private_entry").await;
    let mut p = v06_page("presentation_unreviewed", &rid);
    p["audience"]["kind"] = json!("requester");
    p["audience"]["principal_id"] = json!("principal");
    p["display"]["disclosure"] = json!("requester_only");
    p["page"]["count"] = json!(2);
    p["display"]["fields"][0]["value"] = json!("SECRET_TEXT_ONLY_PRIVATE");
    let mut second = p.clone();
    second["page"]["index"] = json!(1);
    second["page"]["token"] = json!("second-page");
    m.lock().unwrap().private_pages = vec![p, second];
    app.handle_v06_mcp(&v06_event(&m, "private", false))
        .await
        .unwrap();
    {
        let m = m.lock().unwrap();
        assert!(
            !serde_json::to_string(&m.messages)
                .unwrap()
                .contains("SECRET_TEXT_ONLY_PRIVATE")
        );
        assert!(
            !m.private
                .iter()
                .any(|v| v["components"].to_string().contains("ma6:once:"))
        );
    }
    assert!(
        m.lock().unwrap().private_original["content"]
            .as_str()
            .unwrap()
            .contains("1/2")
    );
    let page = v06_event(&m, "page1", true);
    app.handle_v06_mcp(&page).await.unwrap();
    assert!(
        m.lock().unwrap().private_original["content"]
            .as_str()
            .unwrap()
            .contains("2/2")
    );
    let event = v06_event(&m, "once", true);
    app.handle_v06_mcp(&event).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    assert_eq!(
        m.lock().unwrap().posts[0]["expected_page_tokens"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let bytes = std::fs::read(&app.store.path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("SECRET_TEXT_ONLY_PRIVATE"));
    server.abort();
}

#[tokio::test]
async fn v06_start_and_grant_menu_work_without_legacy_switches() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let proxy = app.settings().await.proxy;
    proxy
        .start_v2(
            &app.store.request(&rid).await.unwrap(),
            "conv_a",
            json!("test"),
        )
        .await
        .unwrap();
    {
        let m = m.lock().unwrap();
        assert_eq!(
            m.posts[0]["approval_presentation"]["profile"],
            "source-conversation-v3"
        );
        assert_eq!(
            m.posts[0]["approval_policy"]["id"],
            "evaluated-turn-notion-guard"
        );
    }
    let mut grant = v06_examples()
        .as_object()
        .unwrap()
        .values()
        .find_map(|v| {
            v["data"]
                .as_array()
                .and_then(|a| a.iter().find(|g| g.get("grant_policy").is_some()))
                .cloned()
        })
        .unwrap();
    grant["grant_id"] = json!("grant1");
    grant["scope"] = m.lock().unwrap().presentation["scope"].clone();
    grant["availability"] =
        json!({"state":"refreshing","reason":"catalog_loading","retry_after_ms":2000});
    m.lock().unwrap().grants = vec![grant];
    app.handle_mcp_turn(&turn_event(format!("mt:grants:{rid}")))
        .await
        .unwrap();
    let rendered = serde_json::to_string(&m.lock().unwrap().private).unwrap();
    assert!(rendered.contains("定義を更新中"), "{rendered}");
    m.lock().unwrap().v06_caps = false;
    proxy.check().await.unwrap();
    assert!(
        proxy
            .start_v2(
                &app.store.request(&rid).await.unwrap(),
                "conv_a",
                json!("test")
            )
            .await
            .is_err()
    );
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    server.abort();
}
#[tokio::test]
async fn v06_private_old_buttons_after_restart_require_redisplay() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_private_entry").await;
    m.lock().unwrap().private_pages = vec![v06_page("presentation_unreviewed", &rid)];
    app.handle_v06_mcp(&v06_event(&m, "private", false))
        .await
        .unwrap();
    let event = v06_event(&m, "once", true);
    let s = app.settings().await;
    let fresh = App::new(s.cfg, app.store.clone(), app.discord.clone(), s.proxy).unwrap();
    fresh.handle_v06_mcp(&event).await.unwrap();
    assert!(m.lock().unwrap().posts.is_empty());
    fresh
        .handle_v06_mcp(&v06_event(&m, "private", false))
        .await
        .unwrap();
    fresh
        .handle_v06_mcp(&v06_event(&m, "once", true))
        .await
        .unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    server.abort();
}

#[tokio::test]
async fn v06_definitively_rejected_permission_keeps_decline_available() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let approve = v06_event(&m, "once", false);
    let mut decline = v06_event(&m, "decline", false);
    decline["id"] = json!("989898");
    m.lock().unwrap().reply_reject = true;
    app.handle_v06_mcp(&approve).await.unwrap();
    assert!(m.lock().unwrap().posts.is_empty());
    app.handle_v06_mcp(&decline).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    assert_eq!(m.lock().unwrap().posts[0]["response"]["action"], "decline");
    server.abort();
}
#[tokio::test]
async fn v06_reconciliation_holds_configuration_unknown_until_isolated() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let id = rid.clone();
    app.store
        .call(true, move |c| {
            c.execute(
                "UPDATE requests SET state='SENDING',turn_id=NULL WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut p = v06_examples()["response_policy_pending"]["approval_policy"].clone();
    let mut response = json!({"response_id":"resp_a","conversation_id":"conv_a","workspace_id":"ws_a","turn_id":null,"phase":"dispatching","execution_status":"not_started","approval_policy":p});
    m.lock().unwrap().response = Some(response.clone());
    let proxy = app.settings().await.proxy;
    assert_eq!(
        proxy.reconcile_v2(&app.store, &rid).await.unwrap(),
        RequestState::Sending
    );
    assert!(
        app.store
            .v06_status(&rid)
            .await
            .unwrap()
            .unwrap()
            .contains("準備中")
    );
    p["state"] = json!("failed");
    p["reason"] = json!("policy_setup_unknown");
    p["preparation"]["configuration_isolation"] = json!("pending");
    response["approval_policy"] = p.clone();
    response["phase"] = json!("unknown");
    m.lock().unwrap().response = Some(response.clone());
    assert_eq!(
        proxy.reconcile_v2(&app.store, &rid).await.unwrap(),
        RequestState::Unknown
    );
    assert!(app.store.active("4").await.unwrap().is_some());
    assert!(
        app.store
            .v06_status(&rid)
            .await
            .unwrap()
            .unwrap()
            .contains("AIはまだ開始していません")
    );
    p["preparation"]["configuration_isolation"] = json!("confirmed");
    response["approval_policy"] = p;
    response["phase"] = json!("rejected");
    m.lock().unwrap().response = Some(response);
    assert_eq!(
        proxy.reconcile_v2(&app.store, &rid).await.unwrap(),
        RequestState::Failed
    );
    assert!(app.store.active("4").await.unwrap().is_none());
    assert!(m.lock().unwrap().posts.is_empty());
    server.abort();
}

#[tokio::test]
async fn v06_display_timeout_is_finite_and_retry_only_reads() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    m.lock().unwrap().presentation = Value::Null;
    let s = app.settings().await;
    let fresh = App::new(
        s.cfg.clone(),
        app.store.clone(),
        app.discord.clone(),
        s.proxy.clone(),
    )
    .unwrap();
    fresh.scan_mcp(&rid).await.unwrap();
    app.store
        .call(true, |c| {
            c.execute(
                "UPDATE mcp_v06_views SET poll_deadline_ms=0 WHERE active=1",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let fresh = App::new(s.cfg, app.store.clone(), app.discord.clone(), s.proxy).unwrap();
    let count = m.lock().unwrap().presentation_gets;
    fresh.scan_mcp(&rid).await.unwrap();
    assert_eq!(m.lock().unwrap().presentation_gets, count);
    fresh
        .handle_v06_mcp(&v06_event(&m, "retry", false))
        .await
        .unwrap();
    assert_eq!(m.lock().unwrap().presentation_gets, count + 1);
    assert!(m.lock().unwrap().posts.is_empty());
    server.abort();
}
#[tokio::test]
async fn v06_uncertain_display_post_is_not_duplicated_after_restart() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    m.lock().unwrap().lost_display = true;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let count = m.lock().unwrap().messages.len();
    let s = app.settings().await;
    let fresh = App::new(s.cfg, app.store.clone(), app.discord.clone(), s.proxy).unwrap();
    fresh.expire_mcp_ui().await.unwrap();
    fresh.scan_mcp(&rid).await.unwrap();
    assert_eq!(m.lock().unwrap().messages.len(), count);
    assert!(m.lock().unwrap().posts.is_empty());
    assert!(
        !m.lock().unwrap().messages.last().unwrap()["components"]
            .to_string()
            .contains("ma6:once:")
    );
    server.abort();
}
#[tokio::test]
async fn v06_saved_grant_cancel_survives_list_outage() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let mut grant = v06_examples()
        .as_object()
        .unwrap()
        .values()
        .find_map(|v| {
            v["data"]
                .as_array()
                .and_then(|a| a.iter().find(|g| g.get("grant_policy").is_some()))
                .cloned()
        })
        .unwrap();
    grant["grant_id"] = json!("grant1");
    grant["scope"] = m.lock().unwrap().presentation["scope"].clone();
    m.lock().unwrap().grants = vec![grant];
    app.handle_mcp_turn(&turn_event(format!("mt:grants:{rid}")))
        .await
        .unwrap();
    m.lock().unwrap().fail_grants = true;
    app.handle_mcp_turn(&turn_event(format!("mt:grants:{rid}")))
        .await
        .unwrap();
    let control = m
        .lock()
        .unwrap()
        .private
        .iter()
        .rev()
        .find_map(|v| {
            v["components"][0]["components"][0]["options"][0]["value"]
                .as_str()
                .map(str::to_owned)
        })
        .unwrap();
    assert!(m.lock().unwrap().private.iter().any(|v| {
        v["content"]
            .as_str()
            .is_some_and(|s| s.contains("現在の許可一覧を取得できません"))
    }));
    let event = turn_event(format!("mt:revoke:{control}"));
    app.handle_mcp_turn(&event).await.unwrap();
    app.handle_mcp_turn(&event).await.unwrap();
    assert_eq!(m.lock().unwrap().revoke_keys.len(), 1);
    server.abort();
}

#[tokio::test]
async fn v06_failed_private_open_finishes_deferred_message() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_private_entry").await;
    app.handle_v06_mcp(&v06_event(&m, "private", false))
        .await
        .unwrap();
    let m = m.lock().unwrap();
    assert_eq!(m.callbacks.last().unwrap()["type"], 5);
    assert_eq!(m.private.last().unwrap()["id"], "999");
    assert!(
        m.private.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("もう一度押してください")
    );
    assert!(m.posts.is_empty());
    server.abort();
}

#[tokio::test]
async fn v06_decline_survives_execution_monitor_uncertainty() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let event = v06_event(&m, "decline", false);
    app.store
        .observe(rid, RequestState::Unknown, "transport_unknown", false)
        .await
        .unwrap();
    app.handle_v06_mcp(&event).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    assert_eq!(m.lock().unwrap().posts[0]["response"]["action"], "decline");
    server.abort();
}

#[tokio::test]
async fn v06_catalog_refresh_preserves_explicit_click_without_reposting() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let click = v06_event(&m, "turn", false);
    m.lock().unwrap().loading_gets = 1;
    app.handle_v06_mcp(&click).await.unwrap();
    assert_eq!(m.lock().unwrap().posts.len(), 1);
    assert_eq!(m.lock().unwrap().posts[0]["grant_scope"], "turn_tool");
    server.abort();
}

#[tokio::test]
async fn v06_catalog_wait_is_finite_and_does_not_send_permission() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
    let click = v06_event(&m, "turn", false);
    let before = m.lock().unwrap().presentation_gets;
    m.lock().unwrap().loading_gets = 10;
    app.handle_v06_mcp(&click).await.unwrap();
    let m = m.lock().unwrap();
    assert!(m.posts.is_empty());
    assert_eq!(m.presentation_gets - before, 3);
    assert!(m.private.iter().any(|v| {
        v["content"]
            .as_str()
            .unwrap_or("")
            .contains("許可は送っていません")
    }));
    server.abort();
}

#[tokio::test]
async fn v06_catalog_post_rejections_do_not_block_decline() {
    for code in ["catalog_loading", "catalog_failed"] {
        let (app, m, _tmp, _lock, server, rid) = setup().await;
        enable_v06(&app, &m, &rid, "presentation_turn_eligible").await;
        let click = v06_event(&m, "turn", false);
        let mut decline = v06_event(&m, "decline", false);
        decline["id"] = json!("989898");
        {
            let mut m = m.lock().unwrap();
            m.reply_reject = true;
            m.reject_code = Some(code);
        }
        app.handle_v06_mcp(&click).await.unwrap();
        assert!(m.lock().unwrap().posts.is_empty());
        let rejected: String = app
            .store
            .call(true, |c| {
                Ok(
                    c.query_row("SELECT state FROM mcp_v06_decisions LIMIT 1", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(rejected, "REJECTED");
        app.handle_v06_mcp(&decline).await.unwrap();
        assert_eq!(m.lock().unwrap().posts.len(), 1);
        assert_eq!(m.lock().unwrap().posts[0]["response"]["action"], "decline");
        server.abort();
    }
}

#[tokio::test]
async fn v06_private_catalog_refresh_recovers_without_sending_permission() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_private_entry").await;
    let mut p = v06_page("presentation_unreviewed", &rid);
    p["audience"]["kind"] = json!("requester");
    p["audience"]["principal_id"] = json!("principal");
    p["display"]["disclosure"] = json!("requester_only");
    p["display"]["fields"][0]["value"] = json!("PRIVATEWAITTEST");
    {
        let mut m = m.lock().unwrap();
        m.private_pages = vec![p];
        m.loading_gets = 1;
    }
    app.handle_v06_mcp(&v06_event(&m, "private", false))
        .await
        .unwrap();
    let m = m.lock().unwrap();
    assert!(m.posts.is_empty());
    assert!(
        serde_json::to_string(&m.private)
            .unwrap()
            .contains("PRIVATEWAITTEST")
    );
    assert!(
        m.private_original["content"]
            .as_str()
            .unwrap()
            .contains("PRIVATEWAITTEST")
    );
    assert!(
        m.private_original["components"]
            .to_string()
            .contains("ma6:once:")
    );
    assert!(
        !serde_json::to_string(&m.messages)
            .unwrap()
            .contains("PRIVATEWAITTEST")
    );
    server.abort();
}

#[tokio::test]
async fn v06_private_catalog_wait_explains_no_permission_was_sent() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_private_entry").await;
    let mut p = v06_page("presentation_unreviewed", &rid);
    p["audience"]["kind"] = json!("requester");
    p["audience"]["principal_id"] = json!("principal");
    p["display"]["disclosure"] = json!("requester_only");
    {
        let mut m = m.lock().unwrap();
        m.private_pages = vec![p];
        m.loading_gets = 10;
    }
    let before = m.lock().unwrap().presentation_gets;
    app.handle_v06_mcp(&v06_event(&m, "private", false))
        .await
        .unwrap();
    let m = m.lock().unwrap();
    assert!(m.posts.is_empty());
    assert_eq!(m.presentation_gets - before, 3);
    assert!(m.private.iter().any(|v| {
        v["content"]
            .as_str()
            .unwrap_or("")
            .contains("このボタン操作では許可は送っていません")
    }));
    server.abort();
}

#[tokio::test]
async fn v06_catalog_refresh_keeps_private_entry_stable() {
    let (app, m, _tmp, _lock, server, rid) = setup().await;
    enable_v06(&app, &m, &rid, "presentation_private_entry").await;
    let before = m.lock().unwrap().messages.clone();
    m.lock().unwrap().loading_gets = 1;
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    app.scan_mcp(&rid).await.unwrap();
    assert_eq!(m.lock().unwrap().messages, before);
    assert!(m.lock().unwrap().posts.is_empty());
    server.abort();
}
