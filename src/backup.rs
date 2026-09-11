use crate::{
    config::Config,
    domain,
    storage::{self, Store},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format_version: u32,
    pub backup_id: String,
    pub schema_version: i64,
    pub instance_uuid: String,
    pub started_at: i64,
    pub completed_at: i64,
    pub build: String,
    pub db_bytes: u64,
    pub sha256: String,
    pub integrity: String,
}
pub fn atomic_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing parent")?;
    storage::private_dir(parent)?;
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn create(source: &Path, target: &Path) -> Result<Manifest> {
    ensure!(!target.exists(), "backup target already exists");
    let parent = target.parent().context("backup parent required")?;
    ensure!(parent.is_dir(), "backup parent unavailable");
    let stage = parent.join(format!(".backup-{}", domain::id()));
    storage::private_dir(&stage)?;
    let result = (|| {
        let start = domain::now_ms();
        let src = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let db = stage.join("gateway.sqlite3");
        atomic_new(&db, &[])?;
        let mut dst = Connection::open(&db)?;
        {
            let copy = rusqlite::backup::Backup::new(&src, &mut dst)?;
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                ensure!(Instant::now() < deadline, "backup timed out");
                match copy.step(128)? {
                    rusqlite::backup::StepResult::Done => break,
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
        dst.execute_batch("PRAGMA journal_mode=DELETE;")?;
        dst.close().map_err(|(_, e)| e)?;
        let (schema, instance) = storage::validate_database(&db)?;
        fs::File::open(&db)?.sync_all()?;
        let (db_bytes, checksum) = hash_file(&db)?;
        let manifest = Manifest {
            format_version: 1,
            backup_id: domain::id(),
            schema_version: schema,
            instance_uuid: instance,
            started_at: start,
            completed_at: domain::now_ms(),
            build: env!("CARGO_PKG_VERSION").into(),
            db_bytes,
            sha256: checksum,
            integrity: "ok".into(),
        };
        atomic_new(
            &stage.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        ensure!(!target.exists(), "backup target collision");
        rename_new(&stage, target)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(manifest)
    })();
    // A failed staging directory is retained for diagnosis, never accepted as a completed bundle.
    result
}
pub fn verify(bundle: &Path) -> Result<Manifest> {
    ensure!(
        bundle
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| !s.starts_with(".backup-")),
        "incomplete backup"
    );
    let manifest: Manifest = serde_json::from_slice(&fs::read(bundle.join("manifest.json"))?)?;
    ensure!(
        manifest.format_version == 1 && manifest.integrity == "ok",
        "unsupported backup manifest"
    );
    let db = bundle.join("gateway.sqlite3");
    let (bytes, checksum) = hash_file(&db)?;
    ensure!(
        bytes == manifest.db_bytes && checksum == manifest.sha256,
        "backup checksum mismatch"
    );
    let (v, id) = storage::validate_database(&db)?;
    ensure!(
        v == manifest.schema_version && id == manifest.instance_uuid,
        "backup identity mismatch"
    );
    Ok(manifest)
}
pub fn marker(config_path: &Path) -> Result<PathBuf> {
    Ok(config_path
        .parent()
        .context("config parent unavailable")?
        .join("recovery/restore-pending.json"))
}
pub fn restore(cfg: &Config, config_path: &Path, bundle: &Path) -> Result<String> {
    let _lock = storage::StateLock::acquire(&cfg.storage.state_dir)?;
    let m = verify(bundle)?;
    let (_, instance) = storage::validate_database(&storage::db_path(cfg))?;
    ensure!(m.instance_uuid == instance, "different backup instance");
    let marker = marker(config_path)?;
    let restore_id = domain::id();
    ensure!(!marker.exists(), "recovery already pending");
    atomic_new(
        &marker,
        &serde_json::to_vec(
            &serde_json::json!({"restore_id":restore_id,"instance_uuid":instance,"state_dir":cfg.storage.state_dir,"backup_id":m.backup_id}),
        )?,
    )?;
    // Online Backup API into the offline destination avoids mixing an old WAL with a replacement DB.
    let source = Connection::open_with_flags(
        bundle.join("gateway.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut target = Connection::open(storage::db_path(cfg))?;
    {
        let copy = rusqlite::backup::Backup::new(&source, &mut target)?;
        copy.run_to_completion(128, Duration::from_millis(10), None)?;
    }
    target.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    target.close().map_err(|(_, e)| e)?;
    Ok(restore_id)
}
pub async fn quarantine(store: &Store, restore_id: String) -> Result<()> {
    store.call(true,move|c|{
    let tx=c.transaction()?;let prior:Option<String>=tx.query_row("SELECT restore_id FROM schema_meta WHERE singleton=1",[],|r|r.get(0))?;
    if prior.as_ref()==Some(&restore_id){return Ok(());}
    tx.execute("UPDATE schema_meta SET restore_id=?1,recovery_pending=1 WHERE singleton=1",[&restore_id])?;
    tx.execute("UPDATE requests SET state='UNKNOWN',version=version+1,dispatch_eligible=0,stop_requested=0,restore_id=?1,error_code='restore_quarantined' WHERE state NOT IN ('COMPLETED','FAILED','CANCELLED')",[&restore_id])?;
    tx.execute_batch("UPDATE admissions SET status='QUARANTINED',version=version+1 WHERE status='VALIDATING'; UPDATE operations SET state='SUPERSEDED',send_state=CASE WHEN send_state='NOT_SENT' THEN 'UNKNOWN' ELSE send_state END; INSERT OR IGNORE INTO holds(request_id,project_id) SELECT r.id,c.project_id FROM requests r JOIN conversations c ON c.thread_id=r.thread_id WHERE r.state='UNKNOWN'; UPDATE conversations SET paused=1,pause_revision=pause_revision+1;")?;
    tx.execute("INSERT OR IGNORE INTO restore_blocks(restore_id,project_id) SELECT ?1,id FROM projects",[&restore_id])?;tx.commit()?;Ok(())
}).await
}
pub async fn release(
    store: &Store,
    marker: PathBuf,
    restore_id: String,
    reason: String,
) -> Result<()> {
    ensure!(
        !reason.trim().is_empty() && reason.len() <= 1024,
        "reason required"
    );
    if marker.exists() {
        let v: serde_json::Value = serde_json::from_slice(&fs::read(&marker)?)?;
        ensure!(v["restore_id"] == restore_id, "marker mismatch");
    }
    let rid = restore_id.clone();
    store
        .call(true, move |c| {
            let tx = c.transaction()?;
            let current: Option<String> =
                tx.query_row("SELECT restore_id FROM schema_meta", [], |r| r.get(0))?;
            ensure!(current.as_deref() == Some(&rid), "restore ID mismatch");
            tx.execute(
                "INSERT INTO admin_audit VALUES(?1,?2,'recovery_release',?3,?4,1,?5)",
                params![
                    domain::id(),
                    unsafe { libc::geteuid() },
                    rid,
                    reason,
                    domain::now_ms()
                ],
            )?;
            tx.execute(
                "UPDATE schema_meta SET recovery_pending=0,admission_floor_ms=?1",
                [domain::now_ms()],
            )?;
            tx.execute(
                "UPDATE restore_blocks SET released=1 WHERE restore_id=?1",
                [rid],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    if marker.exists() {
        let v: serde_json::Value = serde_json::from_slice(&fs::read(&marker)?)?;
        ensure!(v["restore_id"] == restore_id, "marker mismatch");
        fs::remove_file(&marker)?;
        fs::File::open(marker.parent().unwrap())?.sync_all()?;
    }
    Ok(())
}

fn rename_new(from: &Path, to: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let a = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let b = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    ensure!(
        unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } == 0,
        "backup publish failed or destination exists"
    );
    Ok(())
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "backup object must be a regular file"
    );
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .context("backup size overflow")?;
        hash.update(&buffer[..n]);
    }
    Ok((bytes, format!("{:x}", hash.finalize())))
}

/// Include projects created after the backup before establishing the global restore block.
pub async fn register_recovery_projects(store: &Store, cfg: &Config) -> Result<()> {
    cfg.validate()?;
    let model = cfg.registration_model().unwrap_or("").to_owned();
    store.call(true,move|c|{
        c.execute("INSERT OR IGNORE INTO projects VALUES(?1,'proxy-default','Proxy default','',0,0,'ACTIVE',?2)",params![crate::storage::PROXY_SCOPE,model])?;
        c.execute("UPDATE schema_meta SET recovery_pending=1",[])?;
        Ok(())
    }).await
}
