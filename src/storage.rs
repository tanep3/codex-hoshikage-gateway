use crate::{
    config::{Config, Workspace},
    domain::{self, Conversation, Request, RequestState},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};
use std::{
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

pub const SCHEMA: i64 = 6;
pub const MIGRATION_V6: &str = include_str!("../migrations/006_interactions.sql");
pub const MIGRATION_V5: &str = include_str!("../migrations/005_generated_images.sql");
pub const MIGRATION_V4: &str = include_str!("../migrations/004_recovery_delivery.sql");
pub const MIGRATION_V3: &str = include_str!("../migrations/003_delivery_controls.sql");
pub const MIGRATION_V2: &str = include_str!("../migrations/002_proxy_v2.sql");
pub const MIGRATION: &str = include_str!("../migrations/001_initial.sql");
pub fn private_dir(p: &Path) -> Result<()> {
    std::fs::create_dir_all(p)?;
    let md = std::fs::symlink_metadata(p)?;
    ensure!(
        md.is_dir() && !md.file_type().is_symlink() && md.uid() == unsafe { libc::geteuid() },
        "private directory identity invalid"
    );
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub struct StateLock(File);
impl StateLock {
    pub fn acquire(dir: &Path) -> Result<Self> {
        private_dir(dir)?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join("instance.lock"))?;
        ensure!(
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another process owns state directory"
        );
        Ok(Self(f))
    }
}
impl Drop for StateLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
// Internal execution group only; this is not a configured filesystem workspace.
pub const PROXY_SCOPE: &str = "ffffffff-ffff-4fff-8fff-ffffffffffff";
fn ensure_proxy_scope(c: &mut Connection, model: &str, fresh: bool) -> Result<()> {
    let tx = c.transaction()?;
    tx.execute("INSERT OR IGNORE INTO projects VALUES(?1,'proxy-default','Proxy default','',0,0,'ACTIVE',?2)",params![PROXY_SCOPE,model])?;
    tx.execute(
        "UPDATE projects SET default_model=?2 WHERE id=?1",
        params![PROXY_SCOPE, model],
    )?;
    let migrated = tx
        .prepare("SELECT 1 FROM admin_audit WHERE id='proxy-default-scope-v1'")?
        .exists([])?;
    if !migrated {
        if !fresh {
            tx.execute("UPDATE admissions SET status='REJECTED',version=version+1,error_code='execution_scope_changed' WHERE status='VALIDATING'",[])?;
            tx.execute("UPDATE requests SET state='FAILED',dispatch_eligible=0,error_code='execution_scope_changed' WHERE state IN ('QUEUED','RECEIVED')",[])?;
            tx.execute("UPDATE conversations SET continuation='NEW_CONVERSATION_REQUIRED',paused=1 WHERE EXISTS(SELECT 1 FROM requests r WHERE r.thread_id=conversations.thread_id AND (r.response_id IS NOT NULL OR r.turn_id IS NOT NULL OR r.state NOT IN ('FAILED','CANCELLED')))",[])?;
            // No prior execution identity or uncertain execution: only new messages may start.
            tx.execute("UPDATE conversations SET continuation='NEW',paused=0 WHERE NOT EXISTS(SELECT 1 FROM requests r WHERE r.thread_id=conversations.thread_id AND (r.response_id IS NOT NULL OR r.turn_id IS NOT NULL OR r.state NOT IN ('FAILED','CANCELLED')))",[])?;
        }
        tx.execute("INSERT INTO admin_audit VALUES('proxy-default-scope-v1',?1,'execution_scope_change',?2,'Proxy owns cwd; old pending requests not replayed',0,?3)",params![unsafe{libc::geteuid()},PROXY_SCOPE,domain::now_ms()])?;
    }
    tx.commit()?;
    Ok(())
}
pub fn db_path(cfg: &Config) -> PathBuf {
    cfg.storage.state_dir.join("gateway.sqlite3")
}
pub fn initialize(cfg: &Config) -> Result<String> {
    let ws = cfg.validate()?;
    let _lock = StateLock::acquire(&cfg.storage.state_dir)?;
    let path = db_path(cfg);
    ensure!(
        !path.exists(),
        "database already exists; refusing initialization"
    );
    let instance = domain::id();
    let _file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    let mut c = Connection::open(&path)?;
    configure(&c)?;
    let tx = c.transaction()?;
    tx.execute_batch(MIGRATION)?;
    tx.execute("INSERT INTO schema_meta(singleton,schema_version,instance_uuid,fixed_digest) VALUES(1,?1,?2,?3)",params![1,instance,cfg.fixed_digest()])?;
    tx.execute(
        "INSERT INTO schema_migrations VALUES(?1,?2,?3)",
        params![1, domain::digest(MIGRATION.as_bytes()), domain::now_ms()],
    )?;
    for w in ws {
        insert_project(&tx, &w)?;
    }
    tx.commit()?;
    ensure_proxy_scope(&mut c, cfg.registration_model().unwrap_or(""), true)?;
    migrate_v2(&mut c, false)?;
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    Ok(instance)
}
fn migrate_v2(c: &mut Connection, quarantine: bool) -> Result<()> {
    let version: i64 = c.query_row("SELECT schema_version FROM schema_meta", [], |r| r.get(0))?;
    if version == SCHEMA {
        return Ok(());
    }
    ensure!((1..=5).contains(&version), "unsupported schema migration");
    if version == 1 {
        let tx = c.transaction()?;
        tx.execute_batch(MIGRATION_V2)?;
        if quarantine {
            // Preserve old request identities and history; never dispatch them under a new workspace.
            tx.execute("UPDATE requests SET dispatch_eligible=0 WHERE state NOT IN ('COMPLETED','FAILED','CANCELLED')", [])?;
            tx.execute(
                "UPDATE conversations SET paused=1,continuation='NEW_CONVERSATION_REQUIRED'",
                [],
            )?;
            tx.execute("UPDATE admissions SET status='QUARANTINED',version=version+1 WHERE status='VALIDATING'", [])?;
        }
        tx.execute(
            "INSERT INTO schema_migrations VALUES(2,?1,?2)",
            params![domain::digest(MIGRATION_V2.as_bytes()), domain::now_ms()],
        )?;
        tx.execute("UPDATE schema_meta SET schema_version=2", [])?;
        tx.commit()?;
    }
    for (target, sql) in [
        (3, MIGRATION_V3),
        (4, MIGRATION_V4),
        (5, MIGRATION_V5),
        (6, MIGRATION_V6),
    ] {
        if version >= target {
            continue;
        }
        let tx = c.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations VALUES(?1,?2,?3)",
            params![target, domain::digest(sql.as_bytes()), domain::now_ms()],
        )?;
        tx.execute("UPDATE schema_meta SET schema_version=?1", [target])?;
        tx.commit()?;
    }
    Ok(())
}
fn configure(c: &Connection) -> Result<()> {
    c.busy_timeout(Duration::from_secs(2))?;
    c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    ensure!(
        c.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))? == 2,
        "SQLite durability unavailable"
    );
    Ok(())
}
pub fn validate_database(path: &Path) -> Result<(i64, String)> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (v, id): (i64, String) = c.query_row(
        "SELECT schema_version,instance_uuid FROM schema_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        (1..=SCHEMA).contains(&v),
        "unsupported schema; no database writes performed"
    );
    let hash: String = c.query_row(
        "SELECT checksum FROM schema_migrations WHERE version=?1",
        [1],
        |r| r.get(0),
    )?;
    ensure!(
        hash == domain::digest(MIGRATION.as_bytes()),
        "schema migration checksum mismatch"
    );
    for (version, migration) in [
        (2, MIGRATION_V2),
        (3, MIGRATION_V3),
        (4, MIGRATION_V4),
        (5, MIGRATION_V5),
        (6, MIGRATION_V6),
    ] {
        if v < version {
            continue;
        }
        let checksum: String = c.query_row(
            "SELECT checksum FROM schema_migrations WHERE version=?1",
            [version],
            |r| r.get(0),
        )?;
        ensure!(
            checksum == domain::digest(migration.as_bytes()),
            "schema migration checksum mismatch"
        );
    }
    let integrity: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "database integrity check failed");
    ensure!(
        !c.prepare("PRAGMA foreign_key_check")?.exists([])?,
        "foreign key integrity check failed"
    );
    Ok((v, id))
}
pub(crate) fn insert_project(c: &Connection, w: &Workspace) -> Result<()> {
    c.execute(
        "INSERT INTO projects VALUES(?1,?2,?3,?4,?5,?6,'ACTIVE',?7)",
        params![
            w.project.id,
            w.project.channel_id,
            w.project.name,
            w.path.to_str().context("workspace is not UTF-8")?,
            w.dev as i64,
            w.ino as i64,
            w.project.default_model
        ],
    )?;
    Ok(())
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;
#[derive(Clone)]
pub struct Store {
    urgent: mpsc::Sender<Job>,
    normal: mpsc::Sender<Job>,
    pub path: PathBuf,
}
impl Store {
    pub fn open(cfg: &Config) -> Result<(Self, oneshot::Receiver<()>)> {
        Self::open_mode(cfg, false)
    }
    pub fn open_recovery(cfg: &Config) -> Result<(Self, oneshot::Receiver<()>)> {
        Self::open_mode(cfg, true)
    }
    fn open_mode(cfg: &Config, recovery: bool) -> Result<(Self, oneshot::Receiver<()>)> {
        let path = db_path(cfg);
        validate_database(&path)?;
        let mut c = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        let fixed: String = c.query_row(
            "SELECT fixed_digest FROM schema_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            fixed == cfg.fixed_digest(),
            "initialized identity does not match configuration"
        );
        cfg.validate()?;
        let _ = recovery;
        configure(&c)?;
        ensure_proxy_scope(&mut c, cfg.registration_model().unwrap_or(""), false)?;
        migrate_v2(&mut c, true)?;
        let (urgent, mut ur) = mpsc::channel::<Job>(64);
        let (normal, mut nr) = mpsc::channel::<Job>(128);
        let (finished, done) = oneshot::channel();
        let handle = tokio::runtime::Handle::current();
        std::thread::Builder::new()
            .name("gateway-store".into())
            .spawn(move || {
                let _completion = finished;
                loop {
                    let job = handle.block_on(async {
                        tokio::select! {biased; j=ur.recv()=>j,j=nr.recv()=>j}
                    });
                    match job {
                        Some(j) => j(&mut c),
                        None => break,
                    }
                }
            })?;
        Ok((
            Self {
                urgent,
                normal,
                path,
            },
            done,
        ))
    }
    pub async fn call<T: Send + 'static>(
        &self,
        urgent: bool,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |c| {
            let result = f(c);
            let _ = tx.send(result);
        });
        let sender = if urgent { &self.urgent } else { &self.normal };
        tokio::time::timeout(Duration::from_secs(10), sender.send(job))
            .await
            .context("store queue timeout")?
            .map_err(|_| anyhow::anyhow!("store worker unavailable"))?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .context("store response timeout")?
            .context("store worker terminated")?
    }
    pub async fn add_conversation(&self, thread: String, project: String) -> Result<()> {
        self.call(false,move|c|{c.execute("INSERT OR IGNORE INTO conversations(thread_id,project_id,selected_model) SELECT ?1,id,default_model FROM projects WHERE id=?2 AND lifecycle='ACTIVE'",params![thread,project])?;Ok(())}).await
    }
    pub async fn conversation(&self, thread: &str) -> Result<Conversation> {
        let thread = thread.to_owned();
        self.call(true, move |c| read_conversation(c, &thread))
            .await
    }
    pub async fn request(&self, id: &str) -> Result<Request> {
        let id = id.to_owned();
        self.call(true, move |c| read_request(c, &id)).await
    }
    pub async fn reserve(
        &self,
        message: String,
        thread: String,
        metadata: String,
        limits: crate::config::Limits,
    ) -> Result<Option<String>> {
        self.call(false,move|c|{
            let tx=c.transaction()?;
            if tx.query_row("SELECT 1 FROM admissions WHERE message_id=?1",[&message],|r|r.get::<_,i64>(0)).optional()?.is_some(){return Ok(None);}
            ensure!(!recovery_pending(&tx)?,"recovery pending");
            let floor:i64=tx.query_row("SELECT admission_floor_ms FROM schema_meta",[],|r|r.get(0))?;
            if floor>0{let snowflake=message.parse::<u64>()?;let created=((snowflake>>22) as i64)+1420070400000;ensure!(created>floor&&created<=domain::now_ms()+5000,"old or invalid Discord message");}
            let cv=read_conversation(&tx,&thread)?;
            if cv.continuation=="NEW_CONVERSATION_REQUIRED" {
                let safe:bool=tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM proxy_conversations WHERE thread_id=?1) AND NOT EXISTS(SELECT 1 FROM requests WHERE thread_id=?1 AND state NOT IN ('COMPLETED','FAILED','CANCELLED')) AND NOT EXISTS(SELECT 1 FROM holds h JOIN requests r ON r.id=h.request_id WHERE r.thread_id=?1 AND h.released=0)",[&thread],|r|r.get(0))?;
                ensure!(safe,"conversation recovery required");
                let stopped:bool=tx.query_row("SELECT coalesce((SELECT kind='stop' FROM operations WHERE thread_id=?1 AND kind IN ('stop','resume') AND state='APPLIED' ORDER BY created_at DESC,rowid DESC LIMIT 1),0)",[&thread],|r|r.get(0))?;
                tx.execute("UPDATE conversations SET continuation='NEW',paused=?2,last_response_id=NULL,proxy_thread_id=NULL,effective_model=NULL WHERE thread_id=?1",params![thread,stopped])?;
                tx.execute("INSERT INTO admin_audit VALUES(?1,?2,'conversation_initialize',?3,'Initialize missing Proxy conversation for a new message; prior executions retained',0,?4)",params![domain::id(),unsafe{libc::geteuid()},thread,domain::now_ms()])?;
            }
            let active:bool=tx.query_row("SELECT lifecycle='ACTIVE' FROM projects WHERE id=?1",[&cv.project_id],|r|r.get(0))?;ensure!(active,"project retired");
            let count=|t:Option<&str>|->Result<i64>{Ok(tx.query_row("SELECT count(*) FROM admissions a WHERE (?1 IS NULL OR thread_id=?1) AND (status='VALIDATING' OR (status='ACCEPTED' AND EXISTS(SELECT 1 FROM requests r WHERE r.id=a.request_id AND r.state IN ('RECEIVED','QUEUED') AND r.dispatch_eligible=1)))",[t],|r|r.get(0))?)};
            ensure!(count(None)?<(limits.queue_global as i64)&&count(Some(&thread))?<(limits.queue_conversation as i64),"queue capacity exceeded");
            tx.execute("UPDATE conversations SET next_sequence=next_sequence+1 WHERE thread_id=?1",[&thread])?;
            let seq:i64=tx.query_row("SELECT next_sequence FROM conversations WHERE thread_id=?1",[&thread],|r|r.get(0))?;
            let id=domain::id();let now=domain::now_ms();
            tx.execute("INSERT INTO admissions(message_id,request_id,thread_id,sequence,status,metadata_digest,created_at,expires_at) VALUES(?1,?2,?3,?4,'VALIDATING',?5,?6,?7)",params![message,id,thread,seq,metadata,now,now.saturating_add(limits.validation_secs.saturating_mul(1000).min(i64::MAX as u64) as i64)])?;
            tx.execute("UPDATE admissions SET limits_json=?2 WHERE request_id=?1",params![id,serde_json::to_string(&limits)?])?;tx.commit()?;Ok(Some(id))
        }).await
    }
    pub async fn input_limits(&self, id: String) -> Result<crate::config::Limits> {
        self.call(true, move |c| {
            let json: String = c.query_row(
                "SELECT limits_json FROM admissions WHERE request_id=?1",
                [id],
                |r| r.get(0),
            )?;
            Ok(serde_json::from_str(&json)?)
        })
        .await
    }
    pub async fn admissible_event(&self, id: String) -> Result<bool> {
        self.call(true, move |c| {
            let floor: i64 =
                c.query_row("SELECT admission_floor_ms FROM schema_meta", [], |r| {
                    r.get(0)
                })?;
            if floor == 0 {
                return Ok(true);
            }
            let created = ((id.parse::<u64>()? >> 22) as i64) + 1420070400000;
            Ok(created > floor && created <= domain::now_ms() + 5000)
        })
        .await
    }
    pub async fn finalize(
        &self,
        id: String,
        metadata: String,
        input_digest: String,
        attachments: Vec<(String, usize, String, String)>,
    ) -> Result<()> {
        self.call(false,move|c|{
        let tx=c.transaction()?;
        let (thread,message,seq,meta,expires,status):(String,String,i64,String,i64,String)=tx.query_row("SELECT thread_id,message_id,sequence,metadata_digest,expires_at,status FROM admissions WHERE request_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        ensure!(status=="VALIDATING"&&meta==metadata&&expires>domain::now_ms(),"admission expired or changed");
        ensure!(read_conversation(&tx,&thread)?.continuation!="NEW_CONVERSATION_REQUIRED","conversation cannot continue");
        tx.execute("INSERT INTO requests(id,message_id,thread_id,sequence,state,input_digest,updated_at) VALUES(?1,?2,?3,?4,'QUEUED',?5,?6)",params![id,message,thread,seq,input_digest,domain::now_ms()])?;
        for (i,(aid,bytes,hash,kind)) in attachments.into_iter().enumerate(){tx.execute("INSERT INTO request_attachments VALUES(?1,?2,?3,?4,?5,?6)",params![id,aid,i as i64,bytes as i64,hash,kind])?;}
        tx.execute("UPDATE admissions SET status='ACCEPTED',version=version+1 WHERE request_id=?1 AND status='VALIDATING'",[&id])?;
        event(&tx,&id,None,"QUEUED","validated")?;tx.commit()?;Ok(())
    }).await
    }
    pub async fn reject_admission(&self, id: String, reason: &'static str) -> Result<()> {
        self.call(true,move|c|{c.execute("UPDATE admissions SET status='REJECTED',version=version+1,error_code=?2 WHERE request_id=?1 AND status='VALIDATING'",params![id,reason])?;Ok(())}).await
    }
    pub async fn sweep(&self) -> Result<Vec<String>> {
        self.call(true,move|c|{
        let tx=c.transaction()?;let ids={let mut st=tx.prepare("SELECT request_id FROM admissions WHERE status='VALIDATING' AND expires_at<=?1")?;st.query_map([domain::now_ms()],|r|r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?};
        tx.execute("UPDATE admissions SET status='REJECTED',version=version+1,error_code='validation_expired' WHERE status='VALIDATING' AND expires_at<=?1",[domain::now_ms()])?;tx.execute("UPDATE operations SET state='FAILED',error_code='validation_expired' WHERE kind='model' AND state='VALIDATING' AND created_at<?1",[domain::now_ms()-120000])?;tx.commit()?;Ok(ids)
    }).await
    }
    pub async fn candidates(&self) -> Result<Vec<Request>> {
        self.call(false,|c|{
        let mut st=c.prepare(&format!("{REQUEST_SELECT} WHERE r.state='QUEUED' AND r.dispatch_eligible=1 AND p.lifecycle='ACTIVE' AND NOT EXISTS(SELECT 1 FROM holds h JOIN requests held ON held.id=h.request_id WHERE held.thread_id=r.thread_id AND h.released=0) AND (SELECT count(*) FROM holds WHERE released=0)<2 AND cv.paused=0 AND cv.continuation IN ('NEW','READY') AND NOT EXISTS(SELECT 1 FROM admissions a LEFT JOIN requests prior ON a.request_id=prior.id WHERE a.thread_id=r.thread_id AND a.sequence<r.sequence AND (a.status='VALIDATING' OR prior.state IN ('RECEIVED','QUEUED'))) ORDER BY r.updated_at,r.sequence LIMIT 20"))?;
        Ok(st.query_map([],request_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await
    }
    pub async fn begin_send(&self, id: String) -> Result<Request> {
        self.begin_send_inner(id, None).await
    }
    pub async fn begin_send_authorized(
        &self,
        id: String,
        permit: crate::proxy::DispatchPermit,
    ) -> Result<Request> {
        self.begin_send_inner(id, Some(permit)).await
    }
    async fn begin_send_inner(
        &self,
        id: String,
        permit: Option<crate::proxy::DispatchPermit>,
    ) -> Result<Request> {
        self.call(false,move|c|{
        let tx=c.transaction()?;ensure!(!recovery_pending(&tx)?,"recovery pending");let r=read_request(&tx,&id)?;let cv=read_conversation(&tx,&r.thread_id)?;
        ensure!(r.state==RequestState::Queued&&r.dispatch_eligible&&!cv.paused&&matches!(cv.continuation.as_str(),"NEW"|"READY"),"request not dispatchable");
        let (total,project):(i64,i64)=tx.query_row("SELECT count(*),coalesce(sum(project_id=?1),0) FROM holds WHERE released=0",[&r.project_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let _=project;
        ensure!(total<2&&!tx.prepare("SELECT 1 FROM holds h JOIN requests r ON r.id=h.request_id WHERE r.thread_id=?1 AND h.released=0")?.exists([&r.thread_id])?,"execution capacity unavailable");
        ensure!(!tx.prepare("SELECT 1 FROM holds h JOIN requests held ON held.id=h.request_id JOIN proxy_conversations occupied ON occupied.thread_id=held.thread_id JOIN proxy_conversations desired ON desired.thread_id=?1 WHERE h.released=0 AND occupied.workspace_id=desired.workspace_id")?.exists([&r.thread_id])?,"workspace occupied");
        ensure!(!tx.prepare("SELECT 1 FROM admissions a LEFT JOIN requests r ON r.id=a.request_id WHERE a.thread_id=?1 AND a.sequence<?2 AND (a.status='VALIDATING' OR r.state IN ('RECEIVED','QUEUED'))")?.exists(params![r.thread_id,r.sequence])?,"prior admission pending");
        ensure!(!tx.prepare("SELECT 1 FROM operations WHERE thread_id=?1 AND kind='model' AND state='VALIDATING'")?.exists([&r.thread_id])?,"model validation pending");
        ensure!(tx.query_row("SELECT lifecycle='ACTIVE' FROM projects WHERE id=?1",[&r.project_id],|r|r.get::<_,bool>(0))?,"project retired");
        if let Some(p)=&permit{ensure!(p.valid(&id),"dispatch permit expired or mismatched");let revision:i64=tx.query_row("SELECT config_revision FROM schema_meta",[],|r|r.get(0))?;ensure!(revision==p.revision,"configuration revision changed");}
        let key=format!("req-{}",domain::id());
        tx.execute("UPDATE requests SET state='SENDING',version=version+1,client_request_id=?2,dispatch_started_at=?3,updated_at=?3,model=?4,previous_response_id=?5,model_revision=(SELECT selection_revision FROM conversations WHERE thread_id=?6) WHERE id=?1",params![id,key,domain::now_ms(),cv.selected_model,cv.last_response_id,r.thread_id])?;
        tx.execute("UPDATE conversations SET continuation='VERIFYING' WHERE thread_id=?1",[&r.thread_id])?;
        tx.execute("INSERT INTO holds(request_id,project_id) VALUES(?1,?2)",params![id,r.project_id])?;
        if let Some(p)=permit{tx.execute("UPDATE requests SET capability_epoch=?2,capability_digest=?3,config_revision=?4 WHERE id=?1",params![id,p.epoch as i64,p.digest,p.revision])?;}
        tx.execute("INSERT INTO output_state VALUES(?1,'VOLATILE')",[&id])?;event(&tx,&id,Some("QUEUED"),"SENDING","dispatch_boundary")?;let result=read_request(&tx,&id)?;tx.commit()?;Ok(result)
    }).await
    }
    pub async fn identify(
        &self,
        id: String,
        response: String,
        thread: String,
        turn: String,
    ) -> Result<()> {
        self.call(true,move|c|{
        let tx=c.transaction()?;let r=read_request(&tx,&id)?;let cv=read_conversation(&tx,&r.thread_id)?;
        ensure!(cv.proxy_thread_id.as_ref().is_none_or(|x|x==&thread)&&r.response_id.as_ref().is_none_or(|x|x==&response)&&r.turn_id.as_ref().is_none_or(|x|x==&turn),"Proxy identity mismatch");
        tx.execute("UPDATE requests SET response_id=?2,proxy_thread_id=?3,turn_id=?4 WHERE id=?1",params![id,response,thread,turn])?;
        tx.execute("UPDATE conversations SET proxy_thread_id=?2,effective_model=CASE WHEN effective_sequence<=?3 THEN ?4 ELSE effective_model END,effective_sequence=max(effective_sequence,?3) WHERE thread_id=?1",params![r.thread_id,thread,r.sequence,r.model])?;tx.commit()?;Ok(())
    }).await
    }
    pub async fn observe(
        &self,
        id: String,
        next: RequestState,
        reason: &str,
        continuable: bool,
    ) -> Result<()> {
        let reason = reason.to_owned();
        self.call(true,move|c|{
        let tx=c.transaction()?;let r=read_request(&tx,&id)?;
        if r.state.terminal(){
            if r.state==RequestState::Completed && next==RequestState::Completed && continuable && r.response_id.is_some(){tx.execute("UPDATE conversations SET continuation='READY',last_response_id=?2,last_success_sequence=?3 WHERE thread_id=?1 AND last_success_sequence<=?3 AND continuation='VERIFYING'",params![r.thread_id,r.response_id,r.sequence])?;tx.commit()?;}
            return Ok(());
        }
        ensure!(r.state.allows(next),"illegal state transition");
        tx.execute("UPDATE requests SET state=?2,version=version+1,error_code=?3,updated_at=?4 WHERE id=?1",params![id,next.as_str(),reason,domain::now_ms()])?;
        event(&tx,&id,Some(r.state.as_str()),next.as_str(),&reason)?;
        if next.terminal(){
            tx.execute("UPDATE holds SET released=1 WHERE request_id=?1",[&id])?;
            let cv=read_conversation(&tx,&r.thread_id)?;
            if next==RequestState::Completed&&continuable&&r.response_id.is_some(){
                tx.execute("UPDATE conversations SET continuation='READY',last_response_id=?2,last_success_sequence=?3 WHERE thread_id=?1 AND last_success_sequence<=?3",params![r.thread_id,r.response_id,r.sequence])?;
            }else if continuable {
                tx.execute("UPDATE conversations SET continuation='READY' WHERE thread_id=?1 AND continuation!='NEW_CONVERSATION_REQUIRED'",[&r.thread_id])?;
            }else if next!=RequestState::Completed {
                if cv.last_response_id.is_some(){tx.execute("UPDATE conversations SET continuation='READY' WHERE thread_id=?1",[&r.thread_id])?;}
                else if r.client_request_id.is_some(){close_conversation(&tx,&r.thread_id)?;}
            }
        }else if next==RequestState::Running||next==RequestState::ApprovalRequired{
            tx.execute("INSERT INTO holds(request_id,project_id) VALUES(?1,?2) ON CONFLICT(request_id) DO UPDATE SET released=0,generation=generation+CASE WHEN released=1 THEN 1 ELSE 0 END",params![id,r.project_id])?;
        }
        tx.commit()?;Ok(())
    }).await
    }
    pub async fn pending(&self) -> Result<Vec<Request>> {
        self.call(true,|c|{let mut st=c.prepare(&format!("{REQUEST_SELECT} WHERE (r.state NOT IN ('COMPLETED','FAILED','CANCELLED','QUEUED','RECEIVED') AND EXISTS(SELECT 1 FROM holds h WHERE h.request_id=r.id AND (h.released=0 OR r.state='UNKNOWN'))) OR (r.state='COMPLETED' AND cv.continuation='VERIFYING') ORDER BY r.updated_at ASC LIMIT 40"))?;Ok(st.query_map([],request_row)?.collect::<rusqlite::Result<Vec<_>>>()?)}).await
    }
    pub async fn active(&self, thread: &str) -> Result<Option<Request>> {
        let t = thread.to_owned();
        self.call(true,move|c|{let mut st=c.prepare(&format!("{REQUEST_SELECT} JOIN holds h ON h.request_id=r.id WHERE r.thread_id=?1 AND h.released=0 ORDER BY r.sequence DESC LIMIT 1"))?;Ok(st.query_row([t],request_row).optional()?)}).await
    }
    pub async fn stop(&self, interaction: String, thread: String) -> Result<Option<Request>> {
        self.stop_target(interaction, thread, None).await
    }
    pub async fn stop_target(
        &self,
        interaction: String,
        thread: String,
        expected: Option<String>,
    ) -> Result<Option<Request>> {
        self.call(true,move|c|{
        let tx=c.transaction()?;
        if tx.prepare("SELECT 1 FROM operations WHERE interaction_id=?1")?.exists([&interaction])?{return Ok(None);}
        tx.execute("UPDATE conversations SET paused=1,pause_revision=pause_revision+1,next_control_sequence=next_control_sequence+1 WHERE thread_id=?1",[&thread])?;
        let target=tx.query_row(&format!("{REQUEST_SELECT} JOIN holds h ON h.request_id=r.id WHERE r.thread_id=?1 AND h.released=0 ORDER BY r.sequence DESC LIMIT 1"),[&thread],request_row).optional()?;
        ensure!(expected.as_ref().is_none_or(|id|target.as_ref().is_some_and(|r|&r.id==id)),"stop target no longer active");
        if let Some(r)=&target {tx.execute("UPDATE requests SET stop_requested=1 WHERE id=?1",[&r.id])?;}
        tx.execute("INSERT INTO operations(id,interaction_id,thread_id,kind,state,created_at) VALUES(?1,?2,?3,'stop','APPLIED',?4)",params![domain::id(),interaction,thread,domain::now_ms()])?;
        tx.commit()?;Ok(target)
    }).await
    }
    pub async fn reserve_resume(
        &self,
        interaction: String,
        thread: String,
    ) -> Result<Option<(String, i64)>> {
        self.call(true,move|c|{
        let tx=c.transaction()?;if tx.prepare("SELECT 1 FROM operations WHERE interaction_id=?1")?.exists([&interaction])?{return Ok(None);}
        let cv=read_conversation(&tx,&thread)?;let id=domain::id();tx.execute("INSERT INTO operations(id,interaction_id,thread_id,kind,expected_revision,created_at) VALUES(?1,?2,?3,'resume',?4,?5)",params![id,interaction,thread,cv.pause_revision,domain::now_ms()])?;tx.commit()?;Ok(Some((id,cv.pause_revision)))
    }).await
    }
    pub async fn apply_resume(&self, id: String) -> Result<bool> {
        self.call(true,move|c|{
        let tx=c.transaction()?;let(t,rev,state):(String,i64,String)=tx.query_row("SELECT thread_id,expected_revision,state FROM operations WHERE id=?1 AND kind='resume'",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if state!="VALIDATING"{return Ok(false);}
        let changed=tx.execute("UPDATE conversations SET paused=0,pause_revision=pause_revision+1 WHERE thread_id=?1 AND pause_revision=?2 AND continuation!='NEW_CONVERSATION_REQUIRED'",params![t,rev])?==1;
        tx.execute("UPDATE operations SET state=?2 WHERE id=?1",params![id,if changed{"APPLIED"}else{"SUPERSEDED"}])?;tx.commit()?;Ok(changed)
    }).await
    }
    pub async fn startup_recover(&self) -> Result<()> {
        self.call(true,|c|{
        c.execute_batch("BEGIN; UPDATE output_state SET state='UNAVAILABLE' WHERE state='VOLATILE' AND NOT EXISTS(SELECT 1 FROM requests r JOIN proxy_conversations pc ON pc.thread_id=r.thread_id WHERE r.id=output_state.request_id); UPDATE operations SET state='UNKNOWN',send_state='UNKNOWN' WHERE send_state='SENDING'; UPDATE admissions SET status='REJECTED',version=version+1,error_code='validation_interrupted' WHERE status='VALIDATING'; UPDATE operations SET state='SUPERSEDED' WHERE state='VALIDATING' AND kind IN ('resume','model'); COMMIT;")?;Ok(())
    }).await
    }
    pub async fn status(&self) -> Result<serde_json::Value> {
        self.call(true, |c| {
            let mut st = c.prepare("SELECT state,count(*) FROM requests GROUP BY state")?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(serde_json::json!({"requests":rows,"recovery_pending":recovery_pending(c)?}))
        })
        .await
    }
}
pub(crate) fn recovery_pending(c: &Connection) -> Result<bool> {
    Ok(c.query_row(
        "SELECT recovery_pending OR EXISTS(SELECT 1 FROM proxy_binding WHERE blocked=1) FROM schema_meta WHERE singleton=1",
        [],
        |r| r.get(0),
    )?)
}
fn close_conversation(c: &Connection, thread: &str) -> Result<()> {
    c.execute(
        "UPDATE conversations SET continuation='NEW_CONVERSATION_REQUIRED' WHERE thread_id=?1",
        [thread],
    )?;
    c.execute("INSERT INTO request_events(request_id,old_state,new_state,reason,created_at) SELECT id,state,'FAILED','conversation_not_continuable_before_send',?2 FROM requests WHERE thread_id=?1 AND state IN ('RECEIVED','QUEUED') AND dispatch_started_at IS NULL",params![thread,domain::now_ms()])?;
    c.execute("UPDATE requests SET state='FAILED',version=version+1,error_code='conversation_not_continuable_before_send' WHERE thread_id=?1 AND state IN ('RECEIVED','QUEUED') AND dispatch_started_at IS NULL",[thread])?;
    c.execute("UPDATE admissions SET status='REJECTED',version=version+1,error_code='conversation_not_continuable' WHERE thread_id=?1 AND status='VALIDATING'",[thread])?;
    Ok(())
}
fn event(c: &Connection, id: &str, old: Option<&str>, next: &str, reason: &str) -> Result<()> {
    c.execute("INSERT INTO request_events(request_id,old_state,new_state,reason,created_at) VALUES(?1,?2,?3,?4,?5)",params![id,old,next,reason,domain::now_ms()])?;
    Ok(())
}
fn read_conversation(c: &Connection, thread: &str) -> Result<Conversation> {
    Ok(c.query_row("SELECT thread_id,project_id,paused,pause_revision,selected_model,effective_model,proxy_thread_id,last_response_id,continuation FROM conversations WHERE thread_id=?1",[thread],|r|Ok(Conversation{thread_id:r.get(0)?,project_id:r.get(1)?,paused:r.get(2)?,pause_revision:r.get(3)?,selected_model:r.get(4)?,effective_model:r.get(5)?,proxy_thread_id:r.get(6)?,last_response_id:r.get(7)?,continuation:r.get(8)?}))?)
}
const REQUEST_SELECT: &str = "SELECT r.id,r.message_id,r.thread_id,cv.project_id,r.sequence,r.state,r.input_digest,r.client_request_id,r.response_id,r.proxy_thread_id,r.turn_id,r.model,r.previous_response_id,r.stop_requested,r.dispatch_eligible FROM requests r JOIN conversations cv ON cv.thread_id=r.thread_id JOIN projects p ON p.id=cv.project_id";
fn request_row(r: &Row<'_>) -> rusqlite::Result<Request> {
    let state: String = r.get(5)?;
    let state = RequestState::parse(&state).map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(Request {
        id: r.get(0)?,
        message_id: r.get(1)?,
        thread_id: r.get(2)?,
        project_id: r.get(3)?,
        sequence: r.get(4)?,
        state,
        input_digest: r.get(6)?,
        client_request_id: r.get(7)?,
        response_id: r.get(8)?,
        proxy_thread_id: r.get(9)?,
        turn_id: r.get(10)?,
        model: r.get(11)?,
        previous_response_id: r.get(12)?,
        stop_requested: r.get(13)?,
        dispatch_eligible: r.get(14)?,
    })
}
fn read_request(c: &Connection, id: &str) -> Result<Request> {
    Ok(c.query_row(
        &format!("{REQUEST_SELECT} WHERE r.id=?1"),
        [id],
        request_row,
    )?)
}
