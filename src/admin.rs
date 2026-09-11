use crate::{
    application::App,
    backup,
    config::{Config, secret},
    domain, storage,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{Mutex, Semaphore},
    task::JoinSet,
};
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Status,
    Reload,
    Reconcile {
        request_id: String,
    },
    Abandon {
        request_id: String,
        generation: i64,
        reason: String,
        accept_risk: bool,
    },
    Backup {
        to: PathBuf,
    },
    RecoveryRelease {
        restore_id: String,
        reason: String,
        accept_risk: bool,
    },
}
pub fn binding(config: &Path) -> Result<PathBuf> {
    Ok(config
        .parent()
        .context("configuration parent unavailable")?
        .join("gateway-instance.json"))
}
pub fn validate_binding(cfg: &Config, config: &Path) -> Result<()> {
    let value: Value = serde_json::from_slice(&std::fs::read(binding(config)?)?)?;
    let (_, id) = storage::validate_database(&storage::db_path(cfg))?;
    ensure!(
        value["instance_uuid"] == id
            && value["state_dir"] == cfg.storage.state_dir.to_string_lossy().as_ref(),
        "external instance binding mismatch"
    );
    Ok(())
}
pub async fn call(socket: &Path, command: Command) -> Result<Value> {
    let mut s = UnixStream::connect(socket)
        .await
        .context("gateway admin socket unavailable")?;
    ensure!(
        s.peer_cred()?.uid() == unsafe { libc::geteuid() },
        "admin server UID mismatch"
    );
    s.write_all(&serde_json::to_vec(&command)?).await?;
    s.shutdown().await?;
    let mut bytes = vec![];
    tokio::time::timeout(
        Duration::from_secs(150),
        s.take(65537).read_to_end(&mut bytes),
    )
    .await??;
    ensure!(bytes.len() <= 65536, "admin response too large");
    serde_json::from_slice(&bytes).context("invalid admin response")
}
pub async fn serve(app: App, config_path: PathBuf) -> Result<()> {
    let cfg = app.settings().await.cfg;
    let path = &cfg.storage.socket_path;
    let parent = path.parent().context("socket parent missing")?;
    storage::private_dir(parent)?;
    if let Ok(md) = std::fs::symlink_metadata(path) {
        ensure!(
            md.file_type().is_socket() && md.uid() == unsafe { libc::geteuid() },
            "unsafe existing admin socket"
        );
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let backup_slots = Arc::new(Semaphore::new(1));
    let reload_lock = Arc::new(Mutex::new(()));
    let mut jobs = JoinSet::new();
    loop {
        tokio::select! {
            _=app.cancel.cancelled()=>{jobs.shutdown().await;std::fs::remove_file(path)?;return Ok(())},
            result=jobs.join_next(),if !jobs.is_empty()=>{if result.is_some_and(|r|r.is_err()){anyhow::bail!("admin worker panic")}},
            accepted=listener.accept()=>{
                let(mut socket,_)=accepted?;if socket.peer_cred()?.uid()!=unsafe{libc::geteuid()}||jobs.len()>=8{continue;}
                let(app,config_path,slots,reload_lock)=(app.clone(),config_path.clone(),backup_slots.clone(),reload_lock.clone());
                jobs.spawn(async move{
                    let result=async{let mut bytes=vec![];tokio::time::timeout(Duration::from_secs(5),(&mut socket).take(65537).read_to_end(&mut bytes)).await??;ensure!(bytes.len()<=65536,"admin request too large");let command:Command=serde_json::from_slice(&bytes)?;Ok::<_,anyhow::Error>(command)};
                    // Borrow the socket while reading, so it remains available for the reply.
                    let command=result.await;
                    let result=match command{Ok(c)=>execute(&app,&config_path,c,slots,reload_lock).await,Err(e)=>Err(e)};
                    let response=match result{Ok(v)=>json!({"ok":true,"result":v}),Err(_)=>json!({"ok":false,"error":"管理操作を完了できませんでした。設定・状態・対象IDを確認してください。"})};
                    let _=socket.write_all(&serde_json::to_vec(&response).unwrap()).await;
                });
            }
        }
    }
}
async fn execute(
    app: &App,
    config_path: &Path,
    command: Command,
    backup_slots: Arc<Semaphore>,
    reload_lock: Arc<Mutex<()>>,
) -> Result<Value> {
    match command {
        Command::Status => {
            let s = app.settings().await;
            let db = app.store.status().await?;
            let holds=app.store.call(true,|c|{let mut st=c.prepare("SELECT request_id,project_id,generation FROM holds WHERE released=0")?;Ok(st.query_map([],|r|Ok(json!({"request_id":r.get::<_,String>(0)?,"project_id":r.get::<_,String>(1)?,"generation":r.get::<_,i64>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
            Ok(
                json!({"database":db,"holds":holds,"connected":app.connected.load(Ordering::SeqCst),"proxy_ready":s.proxy.gate.is_ready(),"recovery_pending":app.recovery.load(Ordering::SeqCst)}),
            )
        }
        Command::Reconcile { request_id } => {
            let state = app
                .settings()
                .await
                .proxy
                .reconcile(&app.store, &request_id)
                .await?;
            Ok(json!({"state":state}))
        }
        Command::Abandon {
            request_id,
            generation,
            reason,
            accept_risk,
        } => {
            ensure!(
                accept_risk && !reason.trim().is_empty() && reason.len() <= 1024,
                "risk acknowledgement and reason required"
            );
            app.store.call(true,move|c|{let tx=c.transaction()?;let n=tx.execute("UPDATE holds SET released=1 WHERE request_id=?1 AND generation=?2 AND released=0 AND EXISTS(SELECT 1 FROM requests WHERE id=?1 AND state='UNKNOWN')",params![request_id,generation])?;ensure!(n==1,"UNKNOWN hold or generation mismatch");tx.execute("INSERT INTO admin_audit VALUES(?1,?2,'abandon',?3,?4,1,?5)",params![domain::id(),unsafe{libc::geteuid()},request_id,reason,domain::now_ms()])?;tx.commit()?;Ok(())}).await?;
            Ok(json!({"hold_released":true,"state":"UNKNOWN"}))
        }
        Command::Backup { to } => {
            let _permit = backup_slots.try_acquire_owned()?;
            let source = app.store.path.clone();
            let result =
                tokio::task::spawn_blocking(move || backup::create(&source, &to)).await??;
            Ok(serde_json::to_value(result)?)
        }
        Command::RecoveryRelease {
            restore_id,
            reason,
            accept_risk,
        } => {
            ensure!(accept_risk, "risk acknowledgement required");
            let _guard = reload_lock.lock().await;
            backup::release(&app.store, backup::marker(config_path)?, restore_id, reason).await?;
            app.recovery.store(false, Ordering::SeqCst);
            Ok(json!({"recovery_released":true,"holds_and_pause_unchanged":true}))
        }
        Command::Reload => {
            let _guard = reload_lock.lock().await;
            let current = app.settings().await;
            let next = Config::read(config_path)?;
            let workspaces = next.validate()?;
            current.cfg.check_reload(&next)?;
            // Existing work snapshots must not be invalidated by shrinking live limits.
            let old = serde_json::to_value(&current.cfg.limits)?;
            let new = serde_json::to_value(&next.limits)?;
            let shrink = old
                .as_object()
                .unwrap()
                .iter()
                .any(|(k, v)| new[k].as_u64() < v.as_u64());
            ensure!(
                app.files.used() <= next.limits.temp_bytes,
                "temporary budget below current usage"
            );
            if shrink {
                let busy=app.store.call(true,|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM holds WHERE released=0) OR EXISTS(SELECT 1 FROM admissions WHERE status='VALIDATING') OR EXISTS(SELECT 1 FROM requests WHERE state IN ('QUEUED','RECEIVED'))",[],|r|r.get::<_,bool>(0))?)).await?;
                ensure!(
                    !busy && app.output.lock().await.is_empty(),
                    "limit reduction requires an idle gateway"
                );
            }
            app.reloading.store(true, Ordering::SeqCst);
            let _reloading = ReloadGuard(app.reloading.clone());
            current.proxy.gate.invalidate();
            let proxy = crate::proxy::Proxy::new(
                next.proxy.base_url.clone(),
                secret(&next.proxy.api_key_file)?,
            )?;
            proxy.check().await?;
            let known=app.store.call(true,|c|Ok(c.query_row("SELECT client_request_id FROM requests WHERE client_request_id IS NOT NULL LIMIT 1",[],|r|r.get::<_,String>(0)).optional()?)).await?;
            if let Some(key) = known {
                let v = proxy
                    .get(&format!(
                        "/v1/codex/requests/{}",
                        crate::proxy::path_id(&key)?
                    ))
                    .await?;
                ensure!(
                    v["client_request_id"] == key,
                    "new credential cannot read known request"
                );
            }
            let n = next.clone();
            let revision = current.revision;
            let ws = workspaces.clone();
            let mut settings = app.settings.write().await;
            let result=app.store.call(true,move|c|{let tx=c.transaction()?;
            let existing={let mut st=tx.prepare("SELECT id,channel_id,cwd,lifecycle FROM projects")?;st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?};
            for(id,channel,cwd,lifecycle)in existing{
                let candidate=n.projects.iter().find(|p|p.id==id);if let Some(p)=candidate{ensure!(p.channel_id==channel,"project channel changed");if p.lifecycle=="ACTIVE"{let w=ws.iter().find(|w|w.project.id==id).context("workspace missing")?;ensure!(w.path.to_string_lossy()==cwd&&lifecycle=="ACTIVE","project identity changed or resurrected");}}
                if candidate.is_none_or(|p|p.lifecycle=="RETIRED"){
                    ensure!(!tx.prepare("SELECT 1 FROM holds WHERE project_id=?1 AND released=0")?.exists([&id])?,"project has execution hold");
                    ensure!(!tx.prepare("SELECT 1 FROM admissions a JOIN conversations cv ON cv.thread_id=a.thread_id LEFT JOIN requests r ON r.id=a.request_id WHERE cv.project_id=?1 AND (a.status='VALIDATING' OR r.state IN ('QUEUED','RECEIVED'))")?.exists([&id])?,"project has queued work");tx.execute("UPDATE projects SET lifecycle='RETIRED' WHERE id=?1",[&id])?;
                }
            }
            for w in ws{let existing:Option<i64>=tx.query_row("SELECT 1 FROM projects WHERE id=?1",[&w.project.id],|r|r.get(0)).optional()?;
                if existing.is_some(){tx.execute("UPDATE projects SET name=?2,default_model=?3 WHERE id=?1",params![w.project.id,w.project.name,w.project.default_model])?;}else{tx.execute("INSERT INTO projects VALUES(?1,?2,?3,?4,?5,?6,'ACTIVE',?7)",params![w.project.id,w.project.channel_id,w.project.name,w.path.to_string_lossy(),w.dev as i64,w.ino as i64,w.project.default_model])?;}
            }ensure!(tx.execute("UPDATE schema_meta SET config_revision=config_revision+1 WHERE config_revision=?1",[revision])?==1,"config revision conflict");tx.commit()?;Ok(())
        }).await;
            if result.is_err() {
                app.cancel.cancel();
                result?;
            }
            app.retired_secrets
                .write()
                .unwrap()
                .push(current.proxy.secret());
            *settings = crate::application::Settings {
                cfg: next,
                revision: current.revision + 1,
                workspaces,
                proxy,
            };
            Ok(json!({"reloaded":true}))
        }
    }
}

struct ReloadGuard(Arc<std::sync::atomic::AtomicBool>);
impl Drop for ReloadGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
