mod common;
use codex_hoshikage_gateway::{
    domain,
    selections::{load_menu, save_menu},
};
use serde_json::json;

#[tokio::test]
async fn selections_survive_restart_but_reject_wrong_scope_generation_and_expiry() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = common::config(&tmp);
    let (store, _lock) = common::store(&cfg).await;
    let token = save_menu(
        &store,
        "4",
        "artifact",
        "conv_a",
        "binding-a".into(),
        vec![json!({"artifact_id":"art_a"})],
        Some("opaque /?&+ cursor".into()),
    )
    .await
    .unwrap();
    let path = store.path.clone();
    drop(store);
    let (store, _) = codex_hoshikage_gateway::storage::Store::open(&cfg).unwrap();
    let menu = load_menu(&store, &token, "4", "binding-a").await.unwrap();
    assert_eq!(menu.1, "conv_a");
    assert_eq!(menu.2[0]["artifact_id"], "art_a");
    assert_eq!(menu.3.as_deref(), Some("opaque /?&+ cursor"));
    assert!(load_menu(&store, &token, "5", "binding-a").await.is_err());
    assert!(load_menu(&store, &token, "4", "binding-b").await.is_err());
    let id = token.clone();
    store
        .call(true, move |c| {
            c.execute(
                "UPDATE selection_menus SET expires_at=?1 WHERE id=?2",
                rusqlite::params![domain::now_ms() - 1, id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(load_menu(&store, &token, "4", "binding-a").await.is_err());
    assert_eq!(
        codex_hoshikage_gateway::storage::validate_database(&path)
            .unwrap()
            .0,
        codex_hoshikage_gateway::storage::SCHEMA
    );
}

#[tokio::test]
async fn schema_two_upgrade_preserves_waiting_deliveries_and_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = common::config(&tmp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "10").await;
    store.call(true,|c|{c.execute_batch("DROP TRIGGER direct_dispatch_no_rewind;DROP TABLE direct_answers;DROP TABLE direct_dispatches;DROP TABLE direct_conversations;DELETE FROM schema_migrations WHERE version=10;DROP TABLE mcp_v06_decisions;DROP TABLE mcp_v06_parts;DROP TABLE mcp_v06_pages;DROP TABLE mcp_v06_views;DROP TABLE mcp_v06_runs; DROP TABLE mcp_inline_views; DROP TABLE mcp_inline_runs; DROP TABLE mcp_grant_revokes; DROP TABLE mcp_grant_records; DROP TABLE mcp_detail_views; DROP TABLE mcp_run_context; ALTER TABLE requests DROP COLUMN interaction_scan_done; DROP TABLE mcp_interactions; DROP TABLE artifact_delivery_claims; ALTER TABLE resource_deliveries DROP COLUMN image_request_id; ALTER TABLE resource_deliveries DROP COLUMN image_ordinal; DROP TABLE generated_image_items; DROP TABLE generated_image_watches; DROP TABLE recovery_reviews; DROP TABLE selection_menus; ALTER TABLE resource_deliveries DROP COLUMN retry_of; ALTER TABLE resource_deliveries DROP COLUMN shared_workspace; ALTER TABLE resource_deliveries DROP COLUMN next_attempt_at; ALTER TABLE resource_deliveries DROP COLUMN attempts; DELETE FROM schema_migrations WHERE version>=3; UPDATE schema_meta SET schema_version=2;")?;Ok(())}).await.unwrap();
    drop(store);
    let (store, _) = codex_hoshikage_gateway::storage::Store::open(&cfg).unwrap();
    assert!(store.request(&request).await.unwrap().dispatch_eligible);
    assert!(!store.conversation("4").await.unwrap().paused);
    assert_eq!(
        codex_hoshikage_gateway::storage::validate_database(&store.path)
            .unwrap()
            .0,
        codex_hoshikage_gateway::storage::SCHEMA
    );
}

#[test]
fn forum_creation_includes_first_message_and_disables_mentions() {
    use codex_hoshikage_gateway::commands::new_thread_body;
    let forum = new_thread_body(&json!({"type":15}), "新しい会話").unwrap();
    assert!(
        forum["message"]["content"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert_eq!(forum["message"]["allowed_mentions"]["parse"], json!([]));
    assert!(forum.get("type").is_none());
    assert!(
        new_thread_body(&json!({"type":0}), "会話")
            .unwrap()
            .get("message")
            .is_none()
    );
    assert!(new_thread_body(&json!({"type":2}), "会話").is_err());
}

#[test]
fn retention_uses_finite_server_deadlines_and_never_revives_expired_leases() {
    use codex_hoshikage_gateway::retention::renewal_deadline;
    let now = "2026-09-11T10:00:00Z";
    assert_eq!(
        renewal_deadline(now, "2026-09-11T10:00:20Z", "2026-09-11T10:05:00Z", 120)
            .unwrap()
            .as_deref(),
        Some("2026-09-11T10:02:00Z")
    );
    assert_eq!(
        renewal_deadline(now, "2026-09-11T10:00:20Z", "2026-09-11T10:00:30Z", 120)
            .unwrap()
            .as_deref(),
        Some("2026-09-11T10:00:30Z")
    );
    assert!(
        renewal_deadline(now, "2026-09-11T10:05:00Z", "2026-09-11T11:00:00Z", 120)
            .unwrap()
            .is_none()
    );
    assert!(renewal_deadline(now, now, "2026-09-11T11:00:00Z", 120).is_err());
    assert!(
        renewal_deadline(now, "2026-09-11T10:00:20Z", "2026-09-11T10:00:20Z", 120)
            .unwrap()
            .is_none()
    );
}
