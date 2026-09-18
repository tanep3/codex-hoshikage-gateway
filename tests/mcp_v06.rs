use codex_hoshikage_gateway::mcp_v06::*;
use serde_json::{Value, json};
fn examples() -> Value {
    serde_json::from_str(include_str!("fixtures/mcp_v06_examples.json")).unwrap()
}
#[test]
fn contract_examples_and_reply_roundtrip() {
    let v = examples();
    let c = Capabilities::parse(&v["capability"]["mcp_approval_v06"]).unwrap();
    assert!(c.supports(None));
    assert!(c.supports(Some(&Selection::guard())));
    for k in [
        "presentation_private_entry",
        "presentation_unreviewed",
        "presentation_turn_eligible",
        "presentation_policy_denied",
        "presentation_selected_unreviewed",
    ] {
        assert!(Presentation::parse(v[k].clone()).is_ok(), "{k}");
    }
    for (p, r, turn) in [
        ("presentation_unreviewed", "reply_once", false),
        ("presentation_turn_eligible", "reply_turn", true),
    ] {
        let p = Presentation::parse(v[p].clone()).unwrap();
        let tokens: Vec<String> =
            serde_json::from_value(v[r]["expected_page_tokens"].clone()).unwrap();
        assert_eq!(p.permit_body(turn, &tokens, 0).unwrap(), v[r]);
    }
    assert_eq!(decline_body(1).unwrap(), v["reply_decline"]);
}
#[test]
fn semantics_and_permission_are_independent() {
    let v = examples();
    let p = Presentation::parse(v["presentation_selected_unreviewed"].clone()).unwrap();
    let tokens = vec![p.value()["page"]["token"].as_str().unwrap().to_owned()];
    assert!(p.permit_body(false, &tokens, 0).is_ok());
    assert!(p.permit_body(true, &tokens, 0).is_err());
    let mut wrong = v["presentation_selected_unreviewed"].clone();
    wrong["actions"]["allow_turn_tool"] = json!(true);
    assert!(Presentation::parse(wrong).is_err());
    let mut wrong = v["presentation_selected_unreviewed"].clone();
    wrong["tool_policy"]["decision"] = json!("unavailable");
    assert!(Presentation::parse(wrong).is_err());
}
#[test]
fn incomplete_or_mismatched_display_never_permits() {
    for path in [
        vec!["scope", "execution_policy_binding_id"],
        vec!["scope", "policy_generation"],
        vec!["audience", "channel_id"],
        vec!["scope", "response_id"],
    ] {
        let mut v = examples()["presentation_turn_eligible"].clone();
        v[&path[0]][&path[1]] = json!("other");
        assert!(Presentation::parse(v).is_err());
    }
    let mut v = examples()["presentation_unreviewed"].clone();
    v["scope"]
        .as_object_mut()
        .unwrap()
        .remove("policy_generation");
    assert!(Presentation::parse(v).is_err());
    let mut v = examples()["presentation_unreviewed"].clone();
    v["display"]["omissions"] = json!(["missing"]);
    assert!(Presentation::parse(v).is_err());
    let mut v = examples()["presentation_policy_denied"].clone();
    v["actions"]["allow_once"] = json!(true);
    assert!(Presentation::parse(v).is_err());
}
#[test]
fn reply_requires_correct_pages_and_expiry() {
    let v = examples();
    let p = Presentation::parse(v["presentation_unreviewed"].clone()).unwrap();
    assert!(p.permit_body(false, &[], 0).is_err());
    assert!(p.permit_body(false, &["wrong".into()], 0).is_err());
    assert!(
        p.permit_body(false, &["opaque-page-none".into()], i64::MAX)
            .is_err()
    );
    assert!(decline_body(1).is_ok()); // no presentation or policy required
}
#[test]
fn capabilities_and_endpoint_limits_are_bounded() {
    let mut v = examples()["capability"]["mcp_approval_v06"].clone();
    v["response_limits"]["interaction_list"] = json!(u64::MAX);
    assert!(Capabilities::parse(&v).is_err());
    assert_eq!(
        response_limit(
            "/v2/codex/interactions/x/presentation?audience=requester&page=1",
            true
        ),
        65536
    );
    assert_eq!(
        response_limit("/v2/codex/interactions/x/operation", false),
        262144
    );
    assert_eq!(
        response_limit("/v2/codex/responses/x/interactions", true),
        67108864
    );
    assert_eq!(
        response_limit("/v2/codex/responses/x/mcp-grants", true),
        1048576
    );
}
#[test]
fn preparation_does_not_gain_a_new_deadline() {
    let mut v = examples()["response_policy_ready"]["approval_policy"].clone();
    assert!(ExecutionPolicy::parse(&v).is_ok());
    v["preparation"]["deadline_at"] = v["preparation"]["started_at"].clone();
    assert!(ExecutionPolicy::parse(&v).is_err());
    let mut v = examples()["response_policy_none"]["approval_policy"].clone();
    v["generation"] = json!("invented");
    assert!(ExecutionPolicy::parse(&v).is_err());
}
mod common;
#[tokio::test]
async fn persisted_selection_and_policy_reject_cross_run_or_stale_update() {
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (s, _lock) = common::store(&cfg).await;
    let request = common::queued(&s, &cfg, "700").await;
    assert!(s.v06_selection(&request).await.unwrap().is_none());
    s.fix_v06_selection(&request, Some(Selection::guard()))
        .await
        .unwrap();
    s.fix_v06_selection(&request, Some(Selection::guard()))
        .await
        .unwrap();
    assert!(s.fix_v06_selection(&request, None).await.is_err());
    let e = examples();
    let pending = ExecutionPolicy::parse(&e["response_policy_pending"]["approval_policy"]).unwrap();
    let ready = ExecutionPolicy::parse(&e["response_policy_ready"]["approval_policy"]).unwrap();
    s.save_v06_policy(&request, pending, 0).await.unwrap();
    assert!(s.save_v06_policy(&request, ready.clone(), 0).await.is_err());
    s.save_v06_policy(&request, ready.clone(), 1).await.unwrap();
    let mut wrong = ready.clone();
    wrong.binding_id = "different".into();
    assert!(s.save_v06_policy(&request, wrong, 2).await.is_err());
    let mut failed = ready.clone();
    failed.state = "failed".into();
    failed.reason = Some("policy_setup_unknown".into());
    s.save_v06_policy(&request, failed, 2).await.unwrap();
    assert!(s.save_v06_policy(&request, ready, 3).await.is_err());
    let count = s
        .call(false, |c| {
            Ok(c.query_row("SELECT count(*) FROM mcp_v06_runs", [], |r| {
                r.get::<_, i64>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[test]
fn schema_eight_upgrade_preserves_old_records() {
    use codex_hoshikage_gateway::storage;
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    storage::initialize(&cfg).unwrap();
    let c = rusqlite::Connection::open(storage::db_path(&cfg)).unwrap();
    c.execute_batch("DROP TABLE direct_generated_images;DROP TABLE direct_image_inventories;DELETE FROM schema_migrations WHERE version=13;DROP TABLE runtime_mode;DELETE FROM schema_migrations WHERE version=12;DROP TRIGGER direct_interaction_no_rewind;DROP TABLE direct_interactions;DELETE FROM schema_migrations WHERE version=11;DROP TRIGGER direct_dispatch_no_rewind;DROP TABLE direct_answers;DROP TABLE direct_dispatches;DROP TABLE direct_conversations;DELETE FROM schema_migrations WHERE version=10;DROP TABLE mcp_v06_decisions; DROP TABLE mcp_v06_parts; DROP TABLE mcp_v06_pages; DROP TABLE mcp_v06_views; DROP TABLE mcp_v06_runs; DELETE FROM schema_migrations WHERE version=9; UPDATE schema_meta SET schema_version=8;").unwrap();
    drop(c);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let (_s, _) = storage::Store::open(&cfg).unwrap();
    let c = rusqlite::Connection::open(storage::db_path(&cfg)).unwrap();
    assert_eq!(
        c.query_row("SELECT schema_version FROM schema_meta", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        storage::SCHEMA
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        storage::SCHEMA
    );
}
#[test]
fn pages_must_all_be_delivered_and_cannot_mix_audiences() {
    let mut v = examples()["presentation_unreviewed"].clone();
    v["page"]["count"] = json!(2);
    let p0 = Presentation::parse(v.clone()).unwrap();
    v["page"]["index"] = json!(1);
    v["page"]["token"] = json!("second-page");
    let p1 = Presentation::parse(v.clone()).unwrap();
    let mut receipts = PageReceipts::new(&p0).unwrap();
    assert!(receipts.confirm(&p1, &["102".into()]).is_err());
    assert!(receipts.confirm(&p0, &[]).is_err());
    receipts.confirm(&p0, &["101".into()]).unwrap();
    assert!(receipts.permit_body(&p0, false, 0).is_err());
    assert!(receipts.confirm(&p1, &["101".into()]).is_err());
    receipts.confirm(&p1, &["102".into()]).unwrap();
    assert_eq!(
        receipts.permit_body(&p1, false, 0).unwrap()["expected_page_tokens"],
        json!(["opaque-page-none", "second-page"])
    );
    v["presentation_fingerprint"] = json!("new-version");
    assert!(
        receipts
            .permit_body(&Presentation::parse(v).unwrap(), false, 0)
            .is_err()
    );
}
#[test]
fn full_long_display_survives_fragmentation_without_markdown_execution() {
    let mut v = examples()["presentation_unreviewed"].clone();
    v["display"]["fields"] = json!([
 {"label":"a","value":"😀".repeat(500)},
 {"label":"b","value":"`@<>\\".repeat(180)},
 {"label":"c","value":"z".repeat(1000)}]);
    let p = Presentation::parse(v).unwrap();
    let rendered = render_page(&p).unwrap();
    assert!(rendered.len() > 1 && rendered.iter().all(|x| x.encode_utf16().count() <= 1900));
    assert_eq!(rendered.join("").matches('😀').count(), 500);
    assert!(rendered.join("").contains("\\`\\@\\<\\>\\\\"));
    let mut proof = PageReceipts::new(&p).unwrap();
    assert!(proof.confirm(&p, &["1".into()]).is_err());
    let ids = (1..=rendered.len())
        .map(|n| n.to_string())
        .collect::<Vec<_>>();
    proof.confirm(&p, &ids).unwrap();
    assert!(proof.permit_body(&p, false, 0).is_ok());
}
#[tokio::test]
async fn page_transport_encodes_query_and_enforces_limit_before_parse() {
    use axum::{Router, extract::Query, response::IntoResponse, routing::get};
    use codex_hoshikage_gateway::proxy::Proxy;
    use std::collections::HashMap;
    let router = Router::new().route(
        "/v2/codex/interactions/int_123/presentation",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            assert_eq!(q.get("audience").map(String::as_str), Some("requester"));
            let mut v = examples()["presentation_unreviewed"].clone();
            let large = q.get("presentation_id").is_some_and(|s| s == "large");
            if !large {
                assert_eq!(
                    q.get("presentation_id").map(String::as_str),
                    Some("opaque +&?")
                );
                v["presentation_id"] = json!("opaque +&?");
            }
            let body = if large {
                " ".repeat(65537)
            } else {
                serde_json::to_string(&v).unwrap()
            };
            (
                [
                    ("X-Proxy-Instance-Id", "pxy_test"),
                    ("X-Proxy-Recovery-Generation", "gen_test"),
                ],
                body,
            )
                .into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let p = Proxy::new(format!("http://{addr}"), "test".into()).unwrap();
    p.bind_v2(&common::caps_v2()).await.unwrap();
    assert!(
        p.approval_v06_page("int_123", true, 0, Some("opaque +&?"))
            .await
            .is_ok()
    );
    assert!(
        p.approval_v06_page("int_123", true, 0, Some("large"))
            .await
            .is_err()
    );
    task.abort();
}

#[test]
fn preparation_examples_cannot_release_unknown_configuration() {
    let v = examples();
    for (key, x) in v.as_object().unwrap() {
        if let Some(resp) = x.get("response")
            && resp.get("approval_policy").is_some()
        {
            assert!(validate_response_policy(resp).is_ok(), "{key}");
        }
    }
    let p = v["response_policy_pending"]["approval_policy"].clone();
    let mut r = json!({"approval_policy":p,"phase":"rejected","execution_status":"not_started"});
    assert!(validate_response_policy(&r).is_err());
    r["approval_policy"]["state"] = json!("failed");
    r["approval_policy"]["reason"] = json!("policy_setup_unknown");
    r["approval_policy"]["preparation"]["configuration_isolation"] = json!("pending");
    assert!(validate_response_policy(&r).is_err());
    r["phase"] = json!("unknown");
    assert!(validate_response_policy(&r).is_ok());
    r["phase"] = json!("rejected");
    r["approval_policy"]["preparation"]["configuration_isolation"] = json!("confirmed");
    assert!(validate_response_policy(&r).is_ok());
}
#[test]
fn grants_preserve_policy_scope_and_refreshing_is_not_revocation() {
    let mut g = examples()["grants_selected"]["data"][0].clone();
    // The examples' list key is contract-owned, find it without assuming its display name.
    if g.is_null() {
        g = examples()
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
    }
    validate_grant(&g).unwrap();
    g["availability"] =
        json!({"state":"refreshing","reason":"catalog_loading","retry_after_ms":2000});
    validate_grant(&g).unwrap();
    g["grant_policy"]["execution_policy_binding_id"] = json!("other");
    assert!(validate_grant(&g).is_err());
}
#[tokio::test]
async fn preparation_unknown_can_be_isolated_without_starting_again() {
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (s, _) = common::store(&cfg).await;
    let request = common::queued(&s, &cfg, "702").await;
    s.fix_v06_selection(&request, Some(Selection::guard()))
        .await
        .unwrap();
    let mut p = examples()["response_policy_pending"]["approval_policy"].clone();
    s.observe_v06_policy(&request, &p).await.unwrap();
    p["state"] = json!("failed");
    p["reason"] = json!("policy_setup_unknown");
    p["preparation"]["configuration_isolation"] = json!("pending");
    s.observe_v06_policy(&request, &p).await.unwrap();
    assert!(
        s.v06_status(&request)
            .await
            .unwrap()
            .unwrap()
            .contains("AIはまだ開始していません")
    );
    p["preparation"]["configuration_isolation"] = json!("confirmed");
    s.observe_v06_policy(&request, &p).await.unwrap();
    p["state"] = json!("preparing");
    p["reason"] = Value::Null;
    assert!(s.observe_v06_policy(&request, &p).await.is_err());
}

#[test]
fn proxy_review_delay_responses_preserve_nonready_contract() {
    let examples: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/mcp_v06_review_delay.json")).unwrap();
    for (name, value) in examples.as_object().unwrap() {
        let p = codex_hoshikage_gateway::mcp_v06::Presentation::parse(value.clone())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(p.value()["actions"]["allow_once"], false);
        assert_eq!(p.value()["actions"]["allow_turn_tool"], false);
        if name.starts_with("catalog_") {
            assert_eq!(p.value()["reason"], *name);
            assert_eq!(p.value()["actions"]["retry"], true);
        } else {
            assert_eq!(p.value()["state"], "private_required");
            assert_eq!(p.value()["actions"]["open_private_details"], true);
            assert!(
                !codex_hoshikage_gateway::mcp_v06::render_page(&p)
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
