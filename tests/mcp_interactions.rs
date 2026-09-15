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
 .route("/readyz",get(||async{Json(json!({"status":"ready"}))}))
 .route("/v2/codex/capabilities",get(||async{Json(mcp_caps())}))
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
            get(|| async {
                bound(json!({"state":"succeeded","resource":{"type":"interaction","id":"int_a"}}))
            }),
        )
        .route(
            "/channels/4/messages",
            post(
                |State(m): State<Arc<Mutex<Mock>>>, Json(mut v): Json<Value>| async move {
                    let mut m = m.lock().unwrap();
                    v["id"] = json!((100 + m.messages.len()).to_string());
                    v["channel_id"] = json!("4");
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
                    m.lock().unwrap().messages.push(v.clone());
                    Json(v)
                },
            ),
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
    store.call(true,|c|{c.execute_batch("ALTER TABLE requests DROP COLUMN interaction_scan_done; DROP TABLE mcp_interactions; DELETE FROM schema_migrations WHERE version=6; UPDATE schema_meta SET schema_version=5;")?;Ok(())}).await.unwrap();
    drop(store);
    let (store, _) = Store::open(&cfg).unwrap();
    assert_eq!(
        store.request(&id).await.unwrap().state,
        RequestState::Queued
    );
    assert_eq!(storage::validate_database(&store.path).unwrap().0, 6);
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
