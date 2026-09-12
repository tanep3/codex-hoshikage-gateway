mod common;
use axum::{
    Json, Router,
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
};
use codex_hoshikage_gateway::{
    application::App,
    discord::Discord,
    domain::{self, RequestState},
    generated_images::{Snapshot, validate_png},
    proxy::Proxy,
    proxy_v2::Binding,
    storage::Store,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
fn snapshot(state: &str, revision: u64, items: Value) -> Snapshot {
    serde_json::from_value(json!({"response_id":"resp_image","conversation_id":"conv_image","workspace_id":"ws_image","revision":revision,"state":state,"items":items,"error":null,"expires_at":null})).unwrap()
}
fn item(id: &str, ordinal: u64, state: &str) -> Value {
    json!({"image_id":id,"ordinal":ordinal,"state":state,"artifact_id":if state=="ready"{Some("art_image")}else{None},"error":null})
}
fn png() -> Vec<u8> {
    let mut out = std::io::Cursor::new(vec![]);
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}
#[test]
fn inventory_validation_preserves_ids_and_allows_late_repairs() {
    let pending = snapshot("pending", 1, json!([]));
    pending.validate(None).unwrap();
    let failed = snapshot("complete", 2, json!([item("img_a", 0, "failed")]));
    failed.validate(Some(&pending)).unwrap();
    let ready = snapshot("complete", 3, json!([item("img_a", 0, "ready")]));
    ready.validate(Some(&failed)).unwrap();
    let mut changed = ready.clone();
    changed.items[0].artifact_id = Some("other".into());
    assert!(changed.validate(Some(&ready)).is_err());
    changed.revision += 1;
    assert!(changed.validate(Some(&ready)).is_err());
    let duplicate = snapshot(
        "complete",
        4,
        json!([item("img_a", 0, "ready"), item("img_b", 0, "ready")]),
    );
    assert!(duplicate.validate(None).is_err());
    let creating = snapshot("complete", 4, json!([item("img_a", 0, "creating")]));
    assert!(creating.validate(None).is_err());
    let unknown = snapshot("unknown", 3, json!([]));
    unknown.validate(None).unwrap();
    assert_ne!(unknown.state, "complete");
    assert!(
        snapshot("complete", 4, json!([]))
            .validate(Some(&ready))
            .is_err()
    );
    let bytes = png();
    validate_png(&bytes, 4).unwrap();
    assert!(validate_png(&bytes, 3).is_err());
    assert!(validate_png(&bytes[..33], 4).is_err());
}
fn reply(v: Value) -> impl IntoResponse {
    (
        [
            ("X-Proxy-Instance-Id", "pxy_test"),
            ("X-Proxy-Recovery-Generation", "gen_test"),
        ],
        Json(v),
    )
}
async fn setup(endpoint: String) -> (tempfile::TempDir, App, String) {
    let t = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&t);
    cfg.proxy.base_url = endpoint.clone();
    let (store, _lock) = common::store(&cfg).await;
    let id = common::queued(&store, &cfg, "123").await;
    store.begin_send(id.clone()).await.unwrap();
    store
        .observe(id.clone(), RequestState::Completed, "test", true)
        .await
        .unwrap();
    let i = id.clone();
    store.call(true,move|c|{c.execute("UPDATE requests SET response_id='resp_image' WHERE id=?1",[i])?;c.execute("INSERT INTO proxy_conversations(thread_id,request_key,request_json,conversation_id,workspace_id,state) VALUES('4','cv-key','{}','conv_image','ws_image','READY')",[])?;Ok(())}).await.unwrap();
    let app = App::new(
        cfg,
        store,
        Discord::with_endpoint("test".into(), endpoint.clone()).unwrap(),
        Proxy::new(endpoint, "test".into()).unwrap(),
    )
    .unwrap();
    app.settings().await.proxy.check().await.unwrap();
    app.discord.register("99", "1").await.unwrap();
    app.connected.store(true, Ordering::SeqCst);
    (t, app, id)
}
#[tokio::test]
async fn image_only_late_inventory_survives_reopen_and_lost_discord_receipt() {
    exercise_delivery(false).await;
}
#[tokio::test]
async fn failed_execution_and_partial_registration_still_deliver_the_ready_image() {
    exercise_delivery(true).await;
}
async fn exercise_delivery(partial: bool) {
    let gets = Arc::new(AtomicUsize::new(0));
    let posts = Arc::new(AtomicUsize::new(0));
    let message = Arc::new(Mutex::new(json!([])));
    let bytes = png();
    let hash = domain::digest(&bytes);
    let size = bytes.len();
    let g = gets.clone();
    let p = posts.clone();
    let m = message.clone();
    let list = message.clone();
    let router=Router::new()
        .route("/readyz",get(||async{Json(json!({"status":"ready"}))}))
        .route("/v2/codex/capabilities",get(||async{let mut v=common::caps_v2();v["features"]["generated_image_artifacts"]=json!(true);v["features"]["response_generated_images"]=json!(true);v["limits"]["generated_images_settle_seconds"]=json!(600);Json(v)}))
        .route("/users/@me",get(||async{Json(json!({"id":"99","bot":true}))}))
        .route("/applications/99/guilds/1/commands",put(||async{Json(json!([]))}))
        .route("/channels/4",get(||async{Json(json!({"id":"4","guild_id":"1","type":0,"permission_overwrites":[]}))}))
        .route("/v2/codex/responses/resp_image/generated-images",get(move||{let g=g.clone();async move {let n=g.fetch_add(1,Ordering::SeqCst);reply(serde_json::to_value(if n==0 {snapshot("pending",1,json!([]))}else{delivery_snapshot(partial)}).unwrap())}}))
        .route("/v2/codex/responses/resp_image",get(||async{reply(json!({"response_id":"resp_image","conversation_id":"conv_image","workspace_id":"ws_image","execution_status":"completed"}))}))
        .route("/guilds/1/roles",get(||async{Json(json!([{"id":"1","permissions":"101376"}]))}))
        .route("/guilds/1/members/99",get(||async{Json(json!({"user":{"id":"99"},"roles":[]}))}))
        .route("/v2/codex/artifacts/art_image",get(move||{let hash=hash.clone();async move{reply(json!({"artifact_id":"art_image","response_id":"resp_image","conversation_id":"conv_image","workspace_id":"ws_image","media_type":"image/png","state":"ready","size_bytes":size,"sha256":hash,"display_name":"generated-image-1.png","expires_at":"2030-01-01T00:00:00Z"}))}}))
        .route("/v2/codex/artifacts/art_image/content",get(move||{let b=bytes.clone();async move{([("X-Proxy-Instance-Id","pxy_test"),("X-Proxy-Recovery-Generation","gen_test")],b)}}))
        .route("/v2/codex/leases",post(||async{reply(json!({"lease_id":"lease_image","operation_id":"op_lease","state":"active"}))}))
        .route("/v2/codex/leases/lease_image",get(||async{reply(json!({"lease_id":"lease_image","state":"active","resource":{"type":"artifact","id":"art_image"},"hold_until":"2030-01-01T00:00:00Z"}))}))
        .route("/v2/codex/leases/lease_image/release",post(||async{reply(json!({"operation_id":"op_release","state":"succeeded"}))}))
        .route("/channels/4/messages",post(move|h:HeaderMap,body:Bytes|{let p=p.clone();let m=m.clone();async move{
            assert!(h["content-type"].to_str().unwrap().starts_with("multipart/"),"no empty text message");
            assert!(body.windows(9).any(|w|w==b"image/png"));
            let text=String::from_utf8_lossy(&body);let start=text.find("{\"allowed_mentions\"").unwrap();let rest=&text[start..];let end=rest.find("\r\n--").unwrap();let payload:Value=serde_json::from_str(&rest[..end]).unwrap();
            assert_eq!(payload["message_reference"]["message_id"],"123");assert_eq!(payload["content"],if partial {"作業途中の画像2"}else{"画像1"});
            *m.lock().unwrap()=json!([{"id":"999","channel_id":"4","author":{"id":"99"},"nonce":payload["nonce"],"attachments":[{"filename":"generated-image-1.png","size":size}]}]);
            p.fetch_add(1,Ordering::SeqCst);StatusCode::BAD_GATEWAY
        }}).get(move||{let m=list.clone();async move{Json(m.lock().unwrap().clone())}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (_tmp, app, id) = setup(endpoint).await;
    if partial {
        let i = id.clone();
        app.store
            .call(true, move |c| {
                c.execute("UPDATE requests SET state='FAILED' WHERE id=?1", [i])?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let a = app.clone();
    let discovery = tokio::spawn(async move { a.generated_images_loop().await });
    // No resource worker until after the discovery transaction and a DB reopen.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let n: i64 = app
                .store
                .call(false, |c| {
                    Ok(c.query_row(
                        "SELECT count(*) FROM generated_image_items WHERE delivery_id IS NOT NULL",
                        [],
                        |r| r.get(0),
                    )?)
                })
                .await
                .unwrap();
            if n == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    discovery.abort();
    discovery.await.ok();
    let settings = app.settings().await;
    let (reopened, _worker) = Store::open(&settings.cfg).unwrap();
    let restored = App::new(
        settings.cfg,
        reopened,
        app.discord.clone(),
        settings.proxy.clone(),
    )
    .unwrap();
    restored.connected.store(true, Ordering::SeqCst);
    let b = Binding {
        instance_id: "pxy_test".into(),
        generation: "gen_test".into(),
        base_url: "http://fixture".into(),
    };
    restored
        .apply_image_snapshot(&id, &b, delivery_snapshot(partial))
        .await
        .unwrap();
    let a = restored.clone();
    let worker = tokio::spawn(async move { a.resource_loop().await });
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let n: i64 = restored
                .store
                .call(false, |c| {
                    Ok(c.query_row(
                        "SELECT count(*) FROM resource_deliveries WHERE message_id='999'",
                        [],
                        |r| r.get(0),
                    )?)
                })
                .await
                .unwrap();
            if n == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    restored.image_progress(&id).await.unwrap();
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    let i = id.clone();
    assert_eq!(
        restored
            .store
            .call(false, move |c| Ok(c.query_row(
                "SELECT state FROM generated_image_watches WHERE request_id=?1",
                [i],
                |r| r.get::<_, String>(0)
            )?))
            .await
            .unwrap(),
        if partial { "WATCHING" } else { "DONE" }
    );
    if partial {
        restored
            .store
            .call(false, |c| {
                let text: String = c.query_row(
                    "SELECT code FROM notices WHERE id LIKE 'image-progress-%'",
                    [],
                    |r| r.get(0),
                )?;
                assert!(text.contains("画像2件中1件"));
                Ok(())
            })
            .await
            .unwrap();
    }
    restored.cancel.cancel();
    worker.await.unwrap().unwrap();
    server.abort();
}

#[tokio::test]
async fn snapshots_reuse_manual_delivery_and_roll_back_cross_response_or_revision_conflicts() {
    let router = Router::new()
        .route("/readyz", get(|| async { Json(json!({"status":"ready"})) }))
        .route(
            "/v2/codex/capabilities",
            get(|| async { Json(common::caps_v2()) }),
        )
        .route(
            "/users/@me",
            get(|| async { Json(json!({"id":"99","bot":true})) }),
        )
        .route(
            "/applications/99/guilds/1/commands",
            put(|| async { Json(json!([])) }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", l.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    let (_t, app, id) = setup(endpoint).await;
    let i = id.clone();
    app.store.call(true,move|c|{
        c.execute("INSERT INTO generated_image_watches(request_id,thread_id,instance_id,generation,response_id,conversation_id,workspace_id) VALUES(?1,'4','pxy_test','gen_test','resp_image','conv_image','ws_image')",[i])?;
        c.execute("INSERT INTO resource_deliveries(id,thread_id,resource_type,resource_id,request_key,state,message_id,created_at) VALUES('manual','4','artifact','art_image','manual-key','DELIVERED','321',0)",[])?;Ok(())
    }).await.unwrap();
    let b = Binding {
        instance_id: "pxy_test".into(),
        generation: "gen_test".into(),
        base_url: "http://test".into(),
    };
    let failed = snapshot("complete", 1, json!([item("img_one", 0, "failed")]));
    app.apply_image_snapshot(&id, &b, failed).await.unwrap();
    let ready = snapshot("complete", 2, json!([item("img_one", 0, "ready")]));
    app.apply_image_snapshot(&id, &b, ready.clone())
        .await
        .unwrap();
    app.apply_image_snapshot(&id, &b, ready.clone())
        .await
        .unwrap();
    let mut wrong = ready.clone();
    wrong.response_id = "resp_other".into();
    assert!(app.apply_image_snapshot(&id, &b, wrong).await.is_err());
    let mut wrong = ready.clone();
    wrong.items[0].artifact_id = Some("art_other".into());
    assert!(app.apply_image_snapshot(&id, &b, wrong).await.is_err());
    let mut wrong = b.clone();
    wrong.generation = "restored".into();
    assert!(app.apply_image_snapshot(&id, &wrong, ready).await.is_err());
    let i = id.clone();
    app.store
        .call(false, move |c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM resource_deliveries", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                c.query_row(
                    "SELECT delivery_id FROM generated_image_items WHERE request_id=?1",
                    [i],
                    |r| r.get::<_, String>(0)
                )?,
                "manual"
            );
            Ok(())
        })
        .await
        .unwrap();
    app.image_progress(&id).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn monitoring_deadline_does_not_mean_no_images_and_manual_reconciliation_is_explicit() {
    let router = Router::new()
        .route("/readyz", get(|| async { Json(json!({"status":"ready"})) }))
        .route(
            "/v2/codex/capabilities",
            get(|| async { Json(common::caps_v2()) }),
        )
        .route(
            "/users/@me",
            get(|| async { Json(json!({"id":"99","bot":true})) }),
        )
        .route(
            "/channels/4",
            get(|| async {
                Json(json!({"id":"4","guild_id":"1","type":0,"permission_overwrites":[]}))
            }),
        )
        .route(
            "/applications/99/guilds/1/commands",
            put(|| async { Json(json!([])) }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", l.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    let (_t, app, id) = setup(endpoint).await;
    let i = id.clone();
    app.store.call(true,move|c|{c.execute("INSERT INTO generated_image_watches(request_id,thread_id,instance_id,generation,response_id,conversation_id,workspace_id,deadline_ms,inventory_state) VALUES(?1,'4','pxy_test','gen_test','resp_image','conv_image','ws_image',0,'unknown')",[i])?;Ok(())}).await.unwrap();
    let b = Binding {
        instance_id: "pxy_test".into(),
        generation: "gen_test".into(),
        base_url: "http://test".into(),
    };
    app.poll_generated_images(&id, &b, 600).await.unwrap();
    app.image_progress(&id).await.unwrap();
    let i = id.clone();
    app.store
        .call(false, move |c| {
            assert_eq!(
                c.query_row(
                    "SELECT state FROM generated_image_watches WHERE request_id=?1",
                    [i],
                    |r| r.get::<_, String>(0)
                )?,
                "TIMED_OUT"
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM resource_deliveries", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    app.reconcile_images(&id).await.unwrap();
    let i = id.clone();
    app.store
        .call(false, move |c| {
            let (state, deadline): (String, Option<i64>) = c.query_row(
                "SELECT state,deadline_ms FROM generated_image_watches WHERE request_id=?1",
                [i],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(state, "WATCHING");
            assert!(deadline.is_none());
            assert_eq!(
                c.query_row("SELECT count(*) FROM requests", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    server.abort();
}

fn delivery_snapshot(partial: bool) -> Snapshot {
    snapshot(
        "complete",
        2,
        if partial {
            json!([item("img_failed", 0, "failed"), item("img_one", 1, "ready")])
        } else {
            json!([item("img_one", 0, "ready")])
        },
    )
}

#[test]
fn discord_thread_permissions_use_parent_overrides_and_thread_send_bit() {
    use codex_hoshikage_gateway::discord_permissions::attachment_allowed;
    let base = (1u64 << 10) | (1 << 11) | (1 << 15) | (1 << 16);
    let roles = json!([{"id":"guild","permissions":base.to_string()},{"id":"writer","permissions":(1u64<<38).to_string()}]);
    let member = json!({"user":{"id":"bot"},"roles":["writer"]});
    assert!(attachment_allowed("guild", "bot", &roles, &member, &json!([]), true).unwrap());
    let deny = json!([{"id":"writer","type":0,"allow":"0","deny":(1u64<<15).to_string()}]);
    assert!(!attachment_allowed("guild", "bot", &roles, &member, &deny, true).unwrap());
    let mut overrides = deny;
    overrides
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"bot","type":1,"allow":(1u64<<15).to_string(),"deny":"0"}));
    assert!(attachment_allowed("guild", "bot", &roles, &member, &overrides, true).unwrap());
    assert!(
        !attachment_allowed(
            "guild",
            "bot",
            &roles,
            &json!({"user":{"id":"bot"},"roles":[]}),
            &json!([]),
            true
        )
        .unwrap()
    );
}

#[tokio::test]
async fn pending_image_notice_is_updated_once_when_no_images_are_confirmed() {
    let router = Router::new()
        .route("/readyz", get(|| async { Json(json!({"status":"ready"})) }))
        .route(
            "/v2/codex/capabilities",
            get(|| async { Json(common::caps_v2()) }),
        )
        .route(
            "/users/@me",
            get(|| async { Json(json!({"id":"99","bot":true})) }),
        )
        .route(
            "/applications/99/guilds/1/commands",
            put(|| async { Json(json!([])) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (_t, app, id) = setup(endpoint).await;
    let i = id.clone();
    app.store.call(true,move|c|{c.execute("INSERT INTO generated_image_watches(request_id,thread_id,instance_id,generation,response_id,conversation_id,workspace_id,inventory_state,terminal_observed_ms) VALUES(?1,'4','pxy_test','gen_test','resp_image','conv_image','ws_image','pending',0)",[i])?;Ok(())}).await.unwrap();
    app.image_progress(&id).await.unwrap();
    app.image_progress(&id).await.unwrap();
    app.store
        .call(false, |c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM notices", [], |r| r.get::<_, i64>(0))?,
                1
            );
            let text: String = c.query_row("SELECT code FROM notices", [], |r| r.get(0))?;
            assert!(text.contains("確認しています"));
            Ok(())
        })
        .await
        .unwrap();
    let i = id.clone();
    app.store
        .call(true, move |c| {
            c.execute(
                "UPDATE generated_image_watches SET inventory_state='complete' WHERE request_id=?1",
                [i],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    app.image_progress(&id).await.unwrap();
    app.store
        .call(false, |c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM notices", [], |r| r.get::<_, i64>(0))?,
                1
            );
            let text: String = c.query_row("SELECT code FROM notices", [], |r| r.get(0))?;
            assert!(text.contains("生成画像はありません"));
            Ok(())
        })
        .await
        .unwrap();
    server.abort();
}
