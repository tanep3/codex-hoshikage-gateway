//! Offline, one-way handover from the Proxy-backed service. Historical
//! identities remain for audit; only future Discord messages use local Codex.
use crate::{backup, direct_config::DirectConfig, domain, storage};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use std::time::Duration;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn cutover(cfg: &DirectConfig, backup_dir: &Path) -> Result<String> {
    cfg.validate()?;
    let _lock = storage::StateLock::acquire(&cfg.storage.state_dir)?;
    let db = cfg.storage.state_dir.join("gateway.sqlite3");
    let (schema, instance) = storage::validate_database(&db)?;
    ensure!(schema <= storage::SCHEMA, "unsupported database schema");
    let source = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    if schema >= 12 {
        let mode: String =
            source.query_row("SELECT mode FROM runtime_mode", [], |row| row.get(0))?;
        ensure!(
            mode == "proxy",
            "database was already converted to direct Codex"
        );
    }
    assert_quiescent(&source)?;
    drop(source);
    let backup = backup::create(&db, backup_dir)?;
    let checked = backup::verify(backup_dir)?;
    ensure!(
        backup.backup_id == checked.backup_id && checked.instance_uuid == instance,
        "cutover backup verification failed"
    );
    // Prove the exact saved DB can reach the target schema before touching
    // the service database. A failed migration leaves the old daemon usable.
    let backup_connection = Connection::open_with_flags(
        backup_dir.join("gateway.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut trial = Connection::open_in_memory()?;
    {
        let copy = rusqlite::backup::Backup::new(&backup_connection, &mut trial)?;
        copy.run_to_completion(128, Duration::from_millis(10), None)?;
    }
    trial.execute_batch("PRAGMA foreign_keys=ON")?;
    storage::migrate_v2(&mut trial, false)?;
    assert_quiescent(&trial)?;
    let integrity: String = trial.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(integrity == "ok", "trial migration failed integrity check");
    ensure!(
        !trial.prepare("PRAGMA foreign_key_check")?.exists([])?,
        "trial migration failed foreign key check"
    );
    let mut connection = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    storage::configure(&connection)?;
    storage::migrate_v2(&mut connection, false)?;
    let transaction = connection.transaction()?;
    let mode: String = transaction.query_row(
        "SELECT mode FROM runtime_mode WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        mode == "proxy",
        "database was already converted to direct Codex"
    );
    assert_quiescent(&transaction)?;
    let now = domain::now_ms();
    // The Discord conversation IDs and historical rows remain intact. Only
    // continuation pointers are cleared, so the next message starts fresh.
    transaction.execute(
        "UPDATE conversations SET continuation='NEW',last_response_id=NULL,proxy_thread_id=NULL,effective_model=NULL,effective_reasoning_effort=NULL,selected_model=?1,selected_reasoning_effort=?2,selection_revision=selection_revision+1,effort_revision=effort_revision+1",
        params![cfg.default_model, cfg.default_reasoning_effort],
    )?;
    transaction.execute(
        "UPDATE projects SET default_model=?1,default_reasoning_effort=?2 WHERE id=?3",
        params![
            cfg.default_model,
            cfg.default_reasoning_effort,
            storage::PROXY_SCOPE
        ],
    )?;
    transaction.execute(
        "UPDATE schema_meta SET fixed_digest=?1 WHERE singleton=1",
        [cfg.fixed_digest()],
    )?;
    transaction.execute(
        "UPDATE runtime_mode SET mode='direct',changed_at=?1,backup_id=?2 WHERE singleton=1 AND mode='proxy'",
        params![now, backup.backup_id],
    )?;
    transaction.execute(
        "INSERT INTO admin_audit(id,uid,kind,target_id,reason,risk_accepted,created_at) VALUES(?1,?2,'direct_codex_cutover',?3,?4,0,?5)",
        params![domain::id(),unsafe { libc::geteuid() },instance,format!("backup={}; fresh Codex context; historical requests retained",backup.backup_id),now],
    )?;
    transaction.commit()?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    Ok(backup.backup_id)
}

/// Start a separate direct instance while preserving the complete legacy
/// database and content as an offline, verifiable archive. Unresolved legacy
/// deliveries stay unresolved in that archive; no AI or Discord send is
/// inferred or retried.
pub fn archive_init(cfg: &DirectConfig, legacy_state: &Path, backup_dir: &Path) -> Result<String> {
    cfg.validate()?;
    let _legacy_lock = storage::StateLock::acquire(legacy_state)?;
    storage::private_dir(&cfg.storage.state_dir)?;
    let legacy_canonical = legacy_state.canonicalize()?;
    let direct_canonical = cfg.storage.state_dir.canonicalize()?;
    ensure!(
        !legacy_canonical.starts_with(&direct_canonical)
            && !direct_canonical.starts_with(&legacy_canonical),
        "legacy and direct state directories must be separate"
    );
    let old_db = legacy_state.join("gateway.sqlite3");
    storage::validate_database(&old_db)?;
    let source = Connection::open_with_flags(&old_db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    if source
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='runtime_mode'")?
        .exists([])?
    {
        let mode: String = source.query_row(
            "SELECT mode FROM runtime_mode WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        ensure!(mode == "proxy", "legacy database is not Proxy-backed");
    }
    ensure!(
        !cfg.storage.state_dir.join("gateway.sqlite3").exists(),
        "direct database already exists"
    );
    let unresolved_requests: i64 = source.query_row(
        "SELECT count(*) FROM requests WHERE state NOT IN ('COMPLETED','FAILED','CANCELLED')",
        [],
        |row| row.get(0),
    )?;
    let unresolved_resources: i64 = source.query_row(
        "SELECT count(*) FROM resource_deliveries WHERE state NOT IN ('DELIVERED','SUPERSEDED')",
        [],
        |row| row.get(0),
    )?;
    drop(source);
    let backup = backup::create(&old_db, backup_dir)?;
    let checked = backup::verify(backup_dir)?;
    ensure!(
        backup.backup_id == checked.backup_id,
        "archive verification changed"
    );
    let pending = cfg.storage.state_dir.join("archive-init-pending.json");
    crate::backup::atomic_new(
        &pending,
        &serde_json::to_vec(&serde_json::json!({
            "backup_id": backup.backup_id,
            "archive": backup_dir,
            "legacy_state": legacy_state,
        }))?,
    )?;
    let instance = storage::initialize_direct(cfg)?;
    let direct_db = cfg.storage.state_dir.join("gateway.sqlite3");
    let connection = Connection::open(&direct_db)?;
    connection.execute(
        "INSERT INTO admin_audit(id,uid,kind,target_id,reason,risk_accepted,created_at) VALUES(?1,?2,'direct_codex_archived_start',?3,?4,0,?5)",
        params![
            domain::id(),
            unsafe { libc::geteuid() },
            instance,
            serde_json::json!({
                "backup_id": backup.backup_id,
                "archive": backup_dir,
                "unresolved_requests": unresolved_requests,
                "unresolved_resources": unresolved_resources,
                "legacy_replay": false
            })
            .to_string(),
            domain::now_ms()
        ],
    )?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    fs::remove_file(&pending)?;
    fs::File::open(&cfg.storage.state_dir)?.sync_all()?;
    Ok(backup.backup_id)
}

fn assert_quiescent(connection: &Connection) -> Result<()> {
    for (table, condition, label) in [
        (
            "requests",
            "state NOT IN ('COMPLETED','FAILED','CANCELLED')",
            "unfinished requests",
        ),
        ("admissions", "status='VALIDATING'", "unfinished admissions"),
        (
            "resource_deliveries",
            "state IN ('WAITING','CACHED','POST_PENDING','RELEASE_PENDING')",
            "unfinished resource deliveries",
        ),
        ("holds", "released=0", "unreleased execution holds"),
    ] {
        let count: i64 = connection.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {condition}"),
            [],
            |row| row.get(0),
        )?;
        ensure!(count == 0, "cutover blocked by {label}");
    }
    let active_ops: i64 = connection.query_row(
        "SELECT count(*) FROM operations WHERE send_state IN ('SENDING','SENT') AND state NOT IN ('COMPLETED','FAILED','CANCELLED','SUPERSEDED')",
        [],
        |row| row.get(0),
    )?;
    ensure!(active_ops == 0, "cutover blocked by unfinished controls");
    Ok(())
}

pub fn direct_database(cfg: &DirectConfig) -> Result<PathBuf> {
    cfg.validate()?;
    ensure!(
        !cfg.storage
            .state_dir
            .join("archive-init-pending.json")
            .exists(),
        "archived direct initialization is incomplete"
    );
    let path = cfg.storage.state_dir.join("gateway.sqlite3");
    storage::validate_database(&path)?;
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (mode, digest): (String, String) = connection.query_row(
        "SELECT runtime_mode.mode,schema_meta.fixed_digest FROM runtime_mode JOIN schema_meta ON schema_meta.singleton=runtime_mode.singleton WHERE runtime_mode.singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(mode == "direct", "direct Codex cutover has not completed");
    ensure!(
        digest == cfg.fixed_digest(),
        "direct Codex instance identity changed"
    );
    Ok(path)
}
