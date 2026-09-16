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
    lost: bool,
    targets: Vec<String>,
    deleted: Vec<String>,
    fail_delete: bool,
    operation: Value,
    turn_caps: bool,
    inline_caps: bool,
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
 .route("/v2/codex/responses/resp_a/mcp-grants",get(|State(m):State<Arc<Mutex<Mock>>>|async move{bound(json!({"response_id":"resp_a","data":m.lock().unwrap().grants}))}))
 .route("/v2/codex/mcp-grants/grant1/revoke",post(|State(m):State<Arc<Mutex<Mock>>>,h:HeaderMap|async move{
 let mut m=m.lock().unwrap();m.revoke_keys.push(h["Idempotency-Key"].to_str().unwrap().to_owned());m.grants[0]["state"]=json!("revoked");m.grants[0]["reason"]=json!("operator_revoked");
 StatusCode::SERVICE_UNAVAILABLE
 }))

 .route("/v2/codex/interactions/{id}/presentation",get(|State(m):State<Arc<Mutex<Mock>>>|async move{bound(m.lock().unwrap().presentation.clone())}))
 .route("/v2/codex/interactions/{id}/operation",get(|State(m):State<Arc<Mutex<Mock>>>|async move{bound(m.lock().unwrap().operation.clone())}))
 .route("/users/@me",get(||async{Json(json!({"id":"9","bot":true}))}))
 .route("/readyz",get(||async{Json(json!({"status":"ready"}))}))
 .route("/v2/codex/capabilities",get(|State(m):State<Arc<Mutex<Mock>>>|async move{let mut c=mcp_caps(); if m.lock().unwrap().turn_caps {c["mcp_operation_details"]=json!({"enabled":true,"profile":"native-item-id-v1","max_argument_bytes":65536,"disclosure":"requester_only"});c["mcp_turn_approval"]=json!({"enabled":true,"profile":"native-item-id-v1","max_grants":16,"ttl_seconds":600,"max_records":256});}
 if m.lock().unwrap().inline_caps {c["mcp_inline_approval"]=inline_capability();}Json(c)}))
 .route("/v2/codex/conversations/conv_a/responses",post(|State(m):State<Arc<Mutex<Mock>>>,Json(v):Json<Value>|async move{m.lock().unwrap().posts.push(v);bound(json!({"response_id":"resp_a"}))}))
 .route("/v2/codex/conversations/conv_a",get(||async{bound(json!({"conversation_id":"conv_a","workspace_id":"ws_a","state":"ready"}))}))
 .route("/v2/codex/responses/resp_a",get(||async{bound(json!({"response_id":"resp_a","conversation_id":"conv_a","workspace_id":"ws_a","execution_status":"interrupted","error":{"code":"unsupported_interaction"}}))}))
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
                    Json(v)
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
                    m.lock().unwrap().callbacks.push(v);
                    StatusCode::NO_CONTENT
                },
            ),
        )
        .route(
            "/webhooks/9/token/messages/@original",
            patch(
                |State(m): State<Arc<Mutex<Mock>>>, Json(v): Json<Value>| async move {
                    m.lock().unwrap().private.push(v);
                    Json(json!({}))
                },
            ),
        )
        .route(
            "/webhooks/9/token",
            post(
                |State(m): State<Arc<Mutex<Mock>>>, Json(v): Json<Value>| async move {
                    assert_eq!(v["flags"], 64);
                    m.lock().unwrap().private.push(v);
                    Json(json!({}))
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
    store.call(true,|c|{c.execute_batch("DROP TABLE mcp_inline_views;DROP TABLE mcp_inline_runs;DELETE FROM schema_migrations WHERE version=8;DROP TABLE mcp_grant_revokes; DROP TABLE mcp_grant_records; DROP TABLE mcp_detail_views; DROP TABLE mcp_run_context; DELETE FROM schema_migrations WHERE version=7; ALTER TABLE requests DROP COLUMN interaction_scan_done; DROP TABLE mcp_interactions; DELETE FROM schema_migrations WHERE version=6; UPDATE schema_meta SET schema_version=5;")?;Ok(())}).await.unwrap();
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
    app.store.call(true,|c|{c.execute_batch("DROP TABLE mcp_inline_views;DROP TABLE mcp_inline_runs;DELETE FROM schema_migrations WHERE version=8;DROP TABLE mcp_grant_revokes;DROP TABLE mcp_grant_records;DROP TABLE mcp_detail_views;DROP TABLE mcp_run_context;ALTER TABLE mcp_interactions DROP COLUMN grant_scope;DELETE FROM schema_migrations WHERE version=7;UPDATE schema_meta SET schema_version=6;")?;Ok(())}).await.unwrap();
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
    app.store.call(true,|c|{c.execute_batch("DROP TABLE mcp_inline_views;DROP TABLE mcp_inline_runs;DELETE FROM schema_migrations WHERE version=8;UPDATE schema_meta SET schema_version=7;")?;Ok(())}).await.unwrap();
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
