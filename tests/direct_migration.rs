mod common;
use codex_hoshikage_gateway::{
    backup,
    direct_config::{Codex, DirectConfig},
    direct_migration,
    storage::{self, Store},
};
use rusqlite::Connection;
use std::{fs, os::unix::fs::PermissionsExt};

fn direct(temp: &tempfile::TempDir) -> DirectConfig {
    let old = common::config(temp);
    let home = temp.path().join("codex-home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    DirectConfig {
        discord: old.discord,
        codex: Codex {
            command: std::env::current_exe().unwrap(),
            home,
            workspace_root: None,
            model_provider: "openai".into(),
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            network_access: false,
        },
        storage: old.storage,
        limits: old.limits,
        default_model: "gpt-5.6-luna".into(),
    }
}

#[tokio::test]
async fn cutover_preserves_discord_history_and_starts_fresh_codex_context() {
    let temp = tempfile::tempdir().unwrap();
    let old = common::config(&temp);
    let (store, lock) = common::store(&old).await;
    store.call(true, |db| {
        db.execute("UPDATE conversations SET proxy_thread_id='old-thread',last_response_id='old-response',continuation='READY' WHERE thread_id='4'", [])?;
        Ok(())
    }).await.unwrap();
    drop(store);
    drop(lock);
    let next = direct(&temp);
    let bundle = temp.path().join("before-direct");
    let id = direct_migration::cutover(&next, &bundle).unwrap();
    assert_eq!(backup::verify(&bundle).unwrap().backup_id, id);
    let db = Connection::open(next.storage.state_dir.join("gateway.sqlite3")).unwrap();
    let context: (String, Option<String>, Option<String>, String) = db.query_row(
        "SELECT continuation,proxy_thread_id,last_response_id,selected_model FROM conversations WHERE thread_id='4'",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).unwrap();
    assert_eq!(
        context,
        ("NEW".into(), None, None, next.default_model.clone())
    );
    assert_eq!(
        db.query_row("SELECT mode FROM runtime_mode", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "direct"
    );
    assert!(direct_migration::cutover(&next, &temp.path().join("second-backup")).is_err());
    assert!(Store::open(&old).is_err());
    let (store, _) = Store::open_direct(&next).unwrap();
    let request = store
        .reserve(
            "999".into(),
            "4".into(),
            "metadata".into(),
            next.limits.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    store
        .finalize(request.clone(), "metadata".into(), "input".into(), vec![])
        .await
        .unwrap();
    let workspace = next.storage.state_dir.join("workspaces/4");
    fs::create_dir_all(&workspace).unwrap();
    assert!(
        store
            .prepare_direct(request, "4".into(), workspace, "openai".into())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn cutover_rejects_queued_requests_without_replaying_them() {
    let temp = tempfile::tempdir().unwrap();
    let old = common::config(&temp);
    let (store, lock) = common::store(&old).await;
    let request = common::queued(&store, &old, "123").await;
    drop(store);
    drop(lock);
    let next = direct(&temp);
    let bundle = temp.path().join("before-direct");
    assert!(direct_migration::cutover(&next, &bundle).is_err());
    assert!(!bundle.exists());
    let db = Connection::open(next.storage.state_dir.join("gateway.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT mode FROM runtime_mode", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "proxy"
    );
    assert_eq!(
        db.query_row("SELECT state FROM requests WHERE id=?1", [request], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "QUEUED"
    );
}

#[tokio::test]
async fn archive_init_keeps_unfinished_legacy_request_without_replay() {
    let temp = tempfile::tempdir().unwrap();
    let old = common::config(&temp);
    let (store, lock) = common::store(&old).await;
    let request = common::queued(&store, &old, "123").await;
    drop(store);
    drop(lock);
    let mut next = direct(&temp);
    next.storage.state_dir = temp.path().join("new-direct-state");
    let bundle = temp.path().join("legacy-archive");
    let backup_id = direct_migration::archive_init(&next, &old.storage.state_dir, &bundle).unwrap();
    assert_eq!(backup::verify(&bundle).unwrap().backup_id, backup_id);
    assert!(direct_migration::direct_database(&next).is_ok());
    let archived = Connection::open(bundle.join("gateway.sqlite3")).unwrap();
    let state: String = archived
        .query_row("SELECT state FROM requests WHERE id=?1", [request], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(state, "QUEUED");
    let new_db = Connection::open(next.storage.state_dir.join("gateway.sqlite3")).unwrap();
    let count: i64 = new_db
        .query_row("SELECT count(*) FROM requests", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let audit: String = new_db
        .query_row(
            "SELECT reason FROM admin_audit WHERE kind='direct_codex_archived_start'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(audit.contains("\"unresolved_requests\":1"));
    let pending = next.storage.state_dir.join("archive-init-pending.json");
    fs::write(&pending, b"pending").unwrap();
    assert!(direct_migration::direct_database(&next).is_err());
    fs::remove_file(pending).unwrap();
    assert!(
        direct_migration::archive_init(&next, &old.storage.state_dir, &temp.path().join("again"))
            .is_err()
    );
}

#[test]
fn archive_init_rejects_nested_state_directories() {
    let temp = tempfile::tempdir().unwrap();
    let old = common::config(&temp);
    storage::initialize(&old).unwrap();
    let mut next = direct(&temp);
    next.storage.state_dir = old.storage.state_dir.join("nested");
    assert!(
        direct_migration::archive_init(&next, &old.storage.state_dir, &temp.path().join("backup"))
            .is_err()
    );
    assert!(!temp.path().join("backup").exists());
}

#[tokio::test]
async fn fresh_direct_database_never_requires_proxy_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = direct(&temp);
    storage::initialize_direct(&cfg).unwrap();
    assert!(Store::open_direct(&cfg).is_ok());
    assert!(Store::open(&common::config(&temp)).is_err());
}

#[test]
fn schema_nine_cutover_preserves_an_offline_verifiable_backup() {
    let temp = tempfile::tempdir().unwrap();
    let old = common::config(&temp);
    storage::initialize(&old).unwrap();
    let db_path = old.storage.state_dir.join("gateway.sqlite3");
    let db = Connection::open(&db_path).unwrap();
    db.execute_batch("DROP TABLE direct_artifacts;DELETE FROM schema_migrations WHERE version=14;DROP TABLE direct_generated_images;DROP TABLE direct_image_inventories;DELETE FROM schema_migrations WHERE version=13;DROP TABLE runtime_mode;DELETE FROM schema_migrations WHERE version=12;DROP TRIGGER direct_interaction_no_rewind;DROP TABLE direct_interactions;DELETE FROM schema_migrations WHERE version=11;DROP TRIGGER direct_dispatch_no_rewind;DROP TABLE direct_answers;DROP TABLE direct_dispatches;DROP TABLE direct_conversations;DELETE FROM schema_migrations WHERE version=10;UPDATE schema_meta SET schema_version=9;").unwrap();
    drop(db);
    assert_eq!(storage::validate_database(&db_path).unwrap().0, 9);
    let next = direct(&temp);
    let bundle = temp.path().join("legacy-backup");
    direct_migration::cutover(&next, &bundle).unwrap();
    assert_eq!(backup::verify(&bundle).unwrap().schema_version, 9);
    assert_eq!(
        storage::validate_database(&db_path).unwrap().0,
        storage::SCHEMA
    );
    assert!(direct_migration::direct_database(&next).is_ok());
}
