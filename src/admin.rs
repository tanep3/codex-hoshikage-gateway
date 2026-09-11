use crate::{
    application::App,
    backup,
    config::{Config, secret},
    domain, storage,
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
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
    ProxyInspect,
    ProxyAccept {
        review_token: String,
        reason: String,
        accept_risk: bool,
    },
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
    let reload_lock = app.config_mutation.clone();
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
        Command::ProxyInspect => app.inspect_proxy_recovery().await,
        Command::ProxyAccept {
            review_token,
            reason,
            accept_risk,
        } => {
            app.accept_proxy_recovery(&review_token, &reason, accept_risk)
                .await
        }
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
            let request = app.store.request(&request_id).await?;
            if let Some(response) = request.response_id.as_deref() {
                let remote = app
                    .settings()
                    .await
                    .proxy
                    .v2_json(
                        reqwest::Method::GET,
                        &format!("/v2/codex/responses/{}", crate::proxy::path_id(response)?),
                        None,
                        None,
                    )
                    .await?;
                ensure!(
                    remote["response_id"] == response
                        && remote["execution_status"] == "unknown"
                        && remote["hold_state"] == "administratively_released"
                        && remote["dispatch_eligible"] == false,
                    "Proxy operator must release the hold first"
                );
            } else {
                anyhow::bail!("Proxy execution identity must be reconciled before release");
            }
            app.store.call(true,move|c|{let tx=c.transaction()?;let n=tx.execute("UPDATE holds SET released=1 WHERE request_id=?1 AND generation=?2 AND released=0 AND EXISTS(SELECT 1 FROM requests WHERE id=?1 AND state='UNKNOWN')",params![request_id,generation])?;ensure!(n==1,"UNKNOWN hold or generation mismatch");tx.execute("UPDATE requests SET dispatch_eligible=0 WHERE id=?1",[&request_id])?;tx.execute("UPDATE conversations SET paused=1,continuation='NEW_CONVERSATION_REQUIRED' WHERE thread_id=(SELECT thread_id FROM requests WHERE id=?1)",[&request_id])?;tx.execute("INSERT INTO admin_audit VALUES(?1,?2,'abandon',?3,?4,1,?5)",params![domain::id(),unsafe{libc::geteuid()},request_id,reason,domain::now_ms()])?;tx.commit()?;Ok(())}).await?;
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
            let next = Config::read(config_path)?.with_registered_projects(&app.store.path)?;
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
            let proxy = proxy.with_store(app.store.clone());
            proxy.check().await?;
            let model = next.registration_model().unwrap_or("").to_owned();
            let revision = current.revision;
            let mut settings = app.settings.write().await;
            let result=app.store.call(true,move|c|{let tx=c.transaction()?;
                tx.execute("UPDATE projects SET default_model=?2 WHERE id=?1",params![crate::storage::PROXY_SCOPE,model])?;
                ensure!(tx.execute("UPDATE schema_meta SET config_revision=config_revision+1 WHERE config_revision=?1",[revision])?==1,"config revision conflict");
                tx.commit()?;Ok(())
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
