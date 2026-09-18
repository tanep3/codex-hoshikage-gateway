//! Durable local-Codex execution identity and send boundary.
//!
//! These records never reinterpret historical Proxy responses. Only the
//! direct-mode admission path may call `prepare_direct` for a fresh request.
use crate::{codex_execution::TurnIdentity, direct_content::StoredAnswer, domain, storage::Store};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::{os::unix::fs::MetadataExt, path::PathBuf};

#[derive(Debug, Clone)]
pub struct DirectDispatch {
    pub request_id: String,
    pub intent_id: String,
    pub discord_thread_id: String,
    pub workspace_path: PathBuf,
    pub workspace_dev: u64,
    pub workspace_ino: u64,
    pub codex_thread_id: Option<String>,
    pub selected_model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectRecoveryOffer {
    pub request_id: String,
    pub generation: i64,
    pub waiting: i64,
    pub pause_revision: i64,
    pub next_sequence: i64,
}

impl Store {
    pub async fn direct_recovery_offer(
        &self,
        discord_thread_id: &str,
    ) -> Result<Option<DirectRecoveryOffer>> {
        let thread = discord_thread_id.to_owned();
        self.call(false, move |c| {
            let pending_restore: bool = c.query_row(
                "SELECT recovery_pending!=0 FROM schema_meta WHERE singleton=1", [], |r| r.get(0)
            )?;
            if pending_restore { return Ok(None); }
            let mut holds = c.prepare(
                "SELECT h.request_id,h.generation FROM holds h JOIN requests r ON r.id=h.request_id JOIN direct_dispatches d ON d.request_id=r.id WHERE r.thread_id=?1 AND r.state='UNKNOWN' AND d.send_state='UNKNOWN' AND h.released=0"
            )?;
            let found = holds.query_map([&thread], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if found.len()!=1 { return Ok(None); }
            let held: i64=c.query_row(
                "SELECT count(*) FROM holds h JOIN requests r ON r.id=h.request_id WHERE r.thread_id=?1 AND h.released=0",
                [&thread],|r|r.get(0))?;
            if held!=1 { return Ok(None); }
            let busy: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM requests WHERE thread_id=?1 AND state IN ('SENDING','RUNNING','APPROVAL_REQUIRED','CANCEL_REQUESTED')) OR EXISTS(SELECT 1 FROM admissions WHERE thread_id=?1 AND status='VALIDATING')",
                [&thread], |r| r.get(0)
            )?;
            if busy { return Ok(None); }
            let waiting: i64 = c.query_row(
                "SELECT count(*) FROM requests WHERE thread_id=?1 AND state IN ('RECEIVED','QUEUED') AND dispatch_started_at IS NULL",
                [&thread], |r| r.get(0)
            )?;
            let (pause_revision,next_sequence):(i64,i64)=c.query_row(
                "SELECT pause_revision,next_sequence FROM conversations WHERE thread_id=?1",
                [&thread],|r|Ok((r.get(0)?,r.get(1)?)))?;
            Ok(Some(DirectRecoveryOffer {request_id:found[0].0.clone(),generation:found[0].1,waiting,pause_revision,next_sequence}))
        }).await
    }

    /// User-confirmed, online recovery. The caller has already made a verified
    /// backup and checked that no live Run actor owns this conversation.
    pub async fn recover_direct_unknown(
        &self,
        discord_thread_id: String,
        offer: DirectRecoveryOffer,
        interaction_id: String,
        backup_id: String,
    ) -> Result<bool> {
        self.call(true, move |c| {
            let tx=c.transaction()?;
            if tx.prepare("SELECT 1 FROM operations WHERE interaction_id=?1")?.exists([&interaction_id])? {
                return Ok(false);
            }
            let pending_restore: bool=tx.query_row("SELECT recovery_pending!=0 FROM schema_meta WHERE singleton=1",[],|r|r.get(0))?;
            ensure!(!pending_restore,"restore quarantine is active");
            let (pause_revision,next_sequence):(i64,i64)=tx.query_row(
                "SELECT pause_revision,next_sequence FROM conversations WHERE thread_id=?1",
                [&discord_thread_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            ensure!(pause_revision==offer.pause_revision && next_sequence==offer.next_sequence,"conversation changed after recovery confirmation");
            let match_count: i64=tx.query_row(
                "SELECT count(*) FROM holds h JOIN requests r ON r.id=h.request_id JOIN direct_dispatches d ON d.request_id=r.id WHERE h.request_id=?1 AND r.thread_id=?2 AND r.state='UNKNOWN' AND d.send_state='UNKNOWN' AND h.released=0 AND h.generation=?3",
                params![offer.request_id,discord_thread_id,offer.generation],|r|r.get(0))?;
            ensure!(match_count==1,"unknown recovery target changed");
            let held: i64=tx.query_row(
                "SELECT count(*) FROM holds h JOIN requests r ON r.id=h.request_id WHERE r.thread_id=?1 AND h.released=0",
                [&discord_thread_id],|r|r.get(0))?;
            ensure!(held==1,"conversation has another execution hold");
            let active: bool=tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM requests WHERE thread_id=?1 AND state IN ('SENDING','RUNNING','APPROVAL_REQUIRED','CANCEL_REQUESTED')) OR EXISTS(SELECT 1 FROM admissions WHERE thread_id=?1 AND status='VALIDATING')",
                [&discord_thread_id],|r|r.get(0))?;
            ensure!(!active,"conversation has an active execution or input validation");
            let unsafe_waiting: bool=tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM requests WHERE thread_id=?1 AND state IN ('RECEIVED','QUEUED') AND dispatch_started_at IS NOT NULL)",
                [&discord_thread_id],|r|r.get(0))?;
            ensure!(!unsafe_waiting,"waiting request crossed the send boundary");
            let waiting={
                let mut statement=tx.prepare(
                    "SELECT id,state FROM requests WHERE thread_id=?1 AND state IN ('RECEIVED','QUEUED') AND dispatch_started_at IS NULL"
                )?;
                statement.query_map([&discord_thread_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            ensure!(waiting.len() as i64==offer.waiting,"waiting requests changed after recovery confirmation");
            let now=domain::now_ms();
            for (id,state) in &waiting {
                tx.execute("UPDATE requests SET state='CANCELLED',dispatch_eligible=0,error_code='user_recovery_before_send',updated_at=?2,version=version+1 WHERE id=?1",params![id,now])?;
                tx.execute("INSERT INTO request_events(request_id,old_state,new_state,reason,created_at) VALUES(?1,?2,'CANCELLED','user_recovery_before_send',?3)",params![id,state,now])?;
            }
            ensure!(tx.execute("UPDATE holds SET released=1,generation=generation+1 WHERE request_id=?1 AND released=0 AND generation=?2",params![offer.request_id,offer.generation])?==1,"unknown hold changed before release");
            ensure!(tx.execute("UPDATE direct_conversations SET codex_thread_id=NULL WHERE discord_thread_id=?1",[&discord_thread_id])?==1,"direct conversation binding missing");
            ensure!(tx.execute("UPDATE conversations SET continuation='NEW',paused=0,pause_revision=pause_revision+1 WHERE thread_id=?1",[&discord_thread_id])?==1,"conversation missing");
            tx.execute("INSERT INTO operations(id,interaction_id,thread_id,kind,target_request_id,decision,state,created_at) VALUES(?1,?2,?3,'direct_recover',?4,'new_context','APPLIED',?5)",params![domain::id(),interaction_id,discord_thread_id,offer.request_id,now])?;
            tx.execute("INSERT INTO admin_audit(id,uid,kind,target_id,reason,risk_accepted,created_at) VALUES(?1,?2,'direct_user_recovery',?3,?4,1,?5)",params![domain::id(),unsafe{libc::geteuid()},offer.request_id,format!("backup_id={backup_id};interaction_id={interaction_id}"),now])?;
            tx.commit()?;
            Ok(true)
        }).await
    }

    pub async fn direct_unknown_blocker(&self, discord_thread_id: &str) -> Result<bool> {
        let thread = discord_thread_id.to_owned();
        self.call(false, move |connection| {
            Ok(connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM holds h JOIN requests r ON r.id=h.request_id WHERE r.thread_id=?1 AND r.state='UNKNOWN' AND h.released=0)",
                [&thread],
                |row| row.get(0),
            )?)
        })
        .await
    }

    /// Operator-only recovery after the old runtime has stopped. The caller
    /// must hold the state lock and create a verified backup first.
    pub async fn abandon_direct_unknown(
        &self,
        request_id: String,
        expected_generation: i64,
        reason: String,
    ) -> Result<()> {
        ensure!(
            !reason.trim().is_empty() && reason.len() <= 1024,
            "recovery reason required (up to 1024 bytes)"
        );
        self.call(true, move |connection| {
            let tx = connection.transaction()?;
            let (thread, state, send_state): (String, String, String) = tx.query_row(
                "SELECT r.thread_id,r.state,d.send_state FROM requests r JOIN direct_dispatches d ON d.request_id=r.id WHERE r.id=?1",
                [&request_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            ensure!(state == "UNKNOWN" && send_state == "UNKNOWN", "request is not an unknown direct execution");
            let generation: i64 = tx.query_row(
                "SELECT generation FROM holds WHERE request_id=?1 AND released=0",
                [&request_id],
                |row| row.get(0),
            )?;
            ensure!(generation == expected_generation, "unknown hold generation changed");
            let active: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM requests WHERE thread_id=?1 AND state IN ('SENDING','RUNNING','APPROVAL_REQUIRED','CANCEL_REQUESTED'))",
                [&thread],
                |row| row.get(0),
            )?;
            ensure!(!active, "conversation still has an active direct request");
            let another_hold: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM holds h JOIN requests r ON r.id=h.request_id WHERE r.thread_id=?1 AND h.request_id!=?2 AND h.released=0)",
                params![thread, request_id],
                |row| row.get(0),
            )?;
            ensure!(!another_hold, "conversation has another unreleased hold");
            ensure!(
                tx.execute(
                    "UPDATE holds SET released=1,generation=generation+1 WHERE request_id=?1 AND released=0 AND generation=?2",
                    params![request_id, expected_generation],
                )? == 1,
                "unknown hold changed before release"
            );
            ensure!(
                tx.execute(
                    "UPDATE direct_conversations SET codex_thread_id=NULL WHERE discord_thread_id=?1",
                    [&thread],
                )? == 1,
                "direct conversation binding missing"
            );
            ensure!(
                tx.execute(
                    "UPDATE conversations SET continuation='NEW' WHERE thread_id=?1",
                    [&thread],
                )? == 1,
                "conversation missing"
            );
            tx.execute(
                "INSERT INTO admin_audit(id,uid,kind,target_id,reason,risk_accepted,created_at) VALUES(?1,?2,'direct_unknown_abandon',?3,?4,1,?5)",
                params![domain::id(), unsafe { libc::geteuid() }, request_id, reason, domain::now_ms()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn bound_direct_workspace(&self, discord_thread_id: &str) -> Result<Option<PathBuf>> {
        let thread = discord_thread_id.to_owned();
        self.call(false, move |connection| {
            Ok(connection
                .query_row(
                    "SELECT workspace_path FROM direct_conversations WHERE discord_thread_id=?1",
                    [&thread],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(PathBuf::from))
        })
        .await
    }
    pub async fn fail_direct_before_send(
        &self,
        request_id: String,
        reason: &'static str,
    ) -> Result<()> {
        self.call(true,move|connection|{
            let tx=connection.transaction()?;
            ensure!(tx.execute("UPDATE requests SET state='FAILED',dispatch_eligible=0,error_code=?2,updated_at=?3,version=version+1 WHERE id=?1 AND state='QUEUED' AND dispatch_started_at IS NULL",params![request_id,reason,domain::now_ms()])?==1,"direct request is no longer unsent");
            tx.execute("INSERT INTO request_events(request_id,old_state,new_state,reason,created_at) VALUES(?1,'QUEUED','FAILED',?2,?3)",params![request_id,reason,domain::now_ms()])?;
            tx.commit()?;
            Ok(())
        }).await
    }
    pub async fn direct_answer(&self, request_id: String) -> Result<Option<StoredAnswer>> {
        self.call(true,move|c| {
            let saved=c.query_row(
                "SELECT a.relative_path,a.sha256,a.bytes FROM direct_answers a JOIN direct_dispatches d ON d.request_id=a.request_id WHERE a.request_id=?1 AND d.send_state='TERMINAL'",
                [&request_id],|r|Ok(StoredAnswer {
                    relative_path:r.get(0)?,sha256:r.get(1)?,bytes:r.get::<_,i64>(2)? as usize,
                }),
            ).optional()?;
            Ok(saved)
        }).await
    }

    /// Establish a direct-mode identity for a newly admitted request. This
    /// must run before any App Server request, including thread/start.
    pub async fn prepare_direct(
        &self,
        request_id: String,
        discord_thread_id: String,
        workspace_path: PathBuf,
        model_provider: String,
    ) -> Result<String> {
        let canonical = workspace_path.canonicalize()?;
        ensure!(
            canonical == workspace_path,
            "workspace path is not canonical"
        );
        let md = workspace_path.metadata()?;
        ensure!(
            md.is_dir() && md.uid() == unsafe { libc::geteuid() },
            "workspace identity invalid"
        );
        let dev = md.dev() as i64;
        let ino = md.ino() as i64;
        let workspace = workspace_path
            .to_str()
            .context("workspace is not UTF-8")?
            .to_owned();
        ensure!(workspace_path.is_absolute(), "workspace must be absolute");
        self.call(true, move |c| {
            let tx = c.transaction()?;
            let eligible: bool = tx.query_row(
                "SELECT state='QUEUED' AND dispatch_started_at IS NULL AND dispatch_eligible=1 AND response_id IS NULL AND client_request_id IS NULL FROM requests WHERE id=?1 AND thread_id=?2",
                params![request_id,discord_thread_id], |r| r.get(0),
            ).optional()?.unwrap_or(false);
            ensure!(eligible, "request is not a fresh direct-mode candidate");
            let mode: String = tx.query_row("SELECT mode FROM runtime_mode WHERE singleton=1", [], |r| r.get(0))?;
            if mode != "direct" {
                // Compatibility while the legacy daemon still owns execution.
                // The direct-mode handover is explicit and resets continuation
                // pointers; no Proxy execution record becomes a local Turn.
                ensure!(!tx.prepare("SELECT 1 FROM proxy_conversations WHERE thread_id=?1 AND send_started!=0")?.exists([&discord_thread_id])?, "legacy Proxy conversation has an execution boundary");
                ensure!(!tx.prepare("SELECT 1 FROM requests r WHERE r.thread_id=?1 AND (r.response_id IS NOT NULL OR r.client_request_id IS NOT NULL OR (r.dispatch_started_at IS NOT NULL AND NOT EXISTS(SELECT 1 FROM direct_dispatches d WHERE d.request_id=r.id)))")?.exists([&discord_thread_id])?, "legacy execution requires explicit migration before direct use");
            }
            tx.execute(
                "INSERT OR IGNORE INTO direct_conversations(discord_thread_id,workspace_path,workspace_dev,workspace_ino,model_provider,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![discord_thread_id,workspace,dev,ino,model_provider,domain::now_ms()],
            )?;
            let (saved_path,saved_dev,saved_ino,saved_provider):(String,i64,i64,String) = tx.query_row(
                "SELECT workspace_path,workspace_dev,workspace_ino,model_provider FROM direct_conversations WHERE discord_thread_id=?1",
                [&discord_thread_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            )?;
            ensure!(saved_path==workspace && saved_dev==dev && saved_ino==ino && saved_provider==model_provider,"direct conversation binding changed");
            if let Some((intent,state))=tx.query_row(
                "SELECT intent_id,send_state FROM direct_dispatches WHERE request_id=?1 AND discord_thread_id=?2",
                params![request_id,discord_thread_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)),
            ).optional()? {
                ensure!(state=="PREPARED","direct intent already crossed the send boundary");
                tx.commit()?;
                return Ok(intent);
            }
            let intent = domain::id();
            tx.execute(
                "INSERT INTO direct_dispatches(request_id,intent_id,discord_thread_id,send_state,updated_at) VALUES(?1,?2,?3,'PREPARED',?4)",
                params![request_id,intent,discord_thread_id,domain::now_ms()],
            )?;
            tx.commit()?;
            Ok(intent)
        }).await
    }

    /// Commit before writing the first JSON-RPC byte. On crash, the intent is
    /// UNKNOWN even if the App Server never actually received the bytes.
    pub async fn begin_direct_send(
        &self,
        request_id: String,
        expected_intent: String,
    ) -> Result<DirectDispatch> {
        let dispatch = self.call(true, move |c| {
            let tx = c.transaction()?;
            let now = domain::now_ms();
            let recovery_pending: bool = tx.query_row(
                "SELECT recovery_pending!=0 FROM schema_meta WHERE singleton=1",[],|r|r.get(0),
            )?;
            ensure!(!recovery_pending,"database restore is not released");
            let thread: String = tx.query_row(
                "SELECT discord_thread_id FROM direct_dispatches WHERE request_id=?1 AND intent_id=?2 AND send_state='PREPARED'",
                params![request_id,expected_intent],
                |r| r.get(0),
            )?;
            let (project,paused,continuation,selected_model,model_revision):(String,bool,String,String,i64) = tx.query_row(
                "SELECT project_id,paused,continuation,selected_model,selection_revision FROM conversations WHERE thread_id=?1",
                [&thread],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
            )?;
            ensure!(!paused && matches!(continuation.as_str(),"NEW"|"READY"),"direct conversation is not dispatchable");
            ensure!(!selected_model.is_empty(),"direct conversation has no selected model");
            ensure!(tx.query_row("SELECT lifecycle='ACTIVE' FROM projects WHERE id=?1",[&project],|r|r.get::<_,bool>(0))?,"project retired");
            ensure!(!tx.prepare("SELECT 1 FROM operations WHERE thread_id=?1 AND kind='model' AND state='VALIDATING'")?.exists([&thread])?,"model validation pending");
            let occupied: i64 = tx.query_row(
                "SELECT count(*) FROM holds WHERE released=0",[],|r|r.get(0),
            )?;
            ensure!(occupied<2,"direct execution capacity is full");
            let same_conversation: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM holds h JOIN requests r ON r.id=h.request_id WHERE r.thread_id=?1 AND h.released=0)",
                [&thread],|r|r.get(0),
            )?;
            ensure!(!same_conversation,"direct conversation already has an active request");
            let earlier: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM requests current JOIN admissions prior ON prior.thread_id=current.thread_id AND prior.sequence<current.sequence LEFT JOIN requests preceding ON preceding.id=prior.request_id WHERE current.id=?1 AND (prior.status='VALIDATING' OR (preceding.state IN ('RECEIVED','QUEUED') AND preceding.dispatch_eligible=1)))",
                [&request_id],|r|r.get(0),
            )?;
            ensure!(!earlier,"earlier request has priority");
            ensure!(tx.execute(
                "UPDATE direct_dispatches SET send_state='SENDING',send_started_at=?3,updated_at=?3,version=version+1 WHERE request_id=?1 AND intent_id=?2 AND send_state='PREPARED'",
                params![request_id,expected_intent,now],
            )?==1,"direct send intent already consumed");
            ensure!(tx.execute(
                "UPDATE requests SET state='SENDING',dispatch_started_at=?2,dispatch_eligible=0,updated_at=?2,version=version+1,model=?3,model_revision=?4 WHERE id=?1 AND state='QUEUED' AND dispatch_started_at IS NULL AND dispatch_eligible=1",
                params![request_id,now,selected_model,model_revision],
            )?==1,"request is not queued for direct send");
            tx.execute("UPDATE conversations SET continuation='VERIFYING' WHERE thread_id=?1",[&thread])?;
            tx.execute(
                "INSERT INTO holds(request_id,project_id) VALUES(?1,?2)",
                params![request_id,project],
            )?;
            let value = tx.query_row(
                "SELECT d.request_id,d.intent_id,d.discord_thread_id,c.workspace_path,c.workspace_dev,c.workspace_ino,c.codex_thread_id,v.selected_model FROM direct_dispatches d JOIN direct_conversations c ON c.discord_thread_id=d.discord_thread_id JOIN conversations v ON v.thread_id=d.discord_thread_id WHERE d.request_id=?1",
                [&request_id], |r| Ok(DirectDispatch {
                    request_id:r.get(0)?,intent_id:r.get(1)?,discord_thread_id:r.get(2)?,
                    workspace_path:PathBuf::from(r.get::<_,String>(3)?),
                    workspace_dev:r.get::<_,i64>(4)? as u64,workspace_ino:r.get::<_,i64>(5)? as u64,
                    codex_thread_id:r.get(6)?,
                    selected_model:r.get(7)?,
                }),
            )?;
            tx.commit()?;
            Ok(value)
        }).await?;
        // The immutable send boundary is already committed. If the workspace
        // changed, return an error and leave this request uncertain, never queued.
        let canonical = dispatch.workspace_path.canonicalize()?;
        let md = dispatch.workspace_path.metadata()?;
        ensure!(
            canonical == dispatch.workspace_path
                && md.dev() == dispatch.workspace_dev
                && md.ino() == dispatch.workspace_ino,
            "workspace identity changed after send boundary"
        );
        Ok(dispatch)
    }

    pub async fn acknowledge_direct_turn(
        &self,
        request_id: String,
        codex_thread_id: String,
        codex_turn_id: String,
    ) -> Result<()> {
        self.call(true, move |c| {
            let tx = c.transaction()?;
            let existing:Option<String> = tx.query_row(
                "SELECT c.codex_thread_id FROM direct_dispatches d JOIN direct_conversations c ON c.discord_thread_id=d.discord_thread_id WHERE d.request_id=?1 AND d.send_state='SENDING'",
                [&request_id], |r| r.get(0),
            ).optional()?.context("direct send not in progress")?;
            ensure!(existing.as_deref().is_none_or(|old| old==codex_thread_id), "Codex thread changed");
            let now = domain::now_ms();
            let conversation:String = tx.query_row("SELECT discord_thread_id FROM direct_dispatches WHERE request_id=?1",[&request_id],|r|r.get(0))?;
            tx.execute("UPDATE direct_conversations SET codex_thread_id=?2 WHERE discord_thread_id=?1",params![conversation,codex_thread_id])?;
            ensure!(tx.execute(
                "UPDATE direct_dispatches SET send_state='ACKED',codex_thread_id=?2,codex_turn_id=?3,updated_at=?4,version=version+1 WHERE request_id=?1 AND send_state='SENDING'",
                params![request_id,codex_thread_id,codex_turn_id,now],
            )?==1,"direct send state changed");
            ensure!(tx.execute(
                "UPDATE requests SET state='RUNNING',turn_id=?2,updated_at=?3,version=version+1 WHERE id=?1 AND state='SENDING'",
                params![request_id,codex_turn_id,now],
            )?==1,"request send state changed");
            tx.commit()?;
            Ok(())
        }).await
    }

    /// Persist a successful thread/start before attempting turn/start. A crash
    /// after this point may leave an unused Codex thread, never a replayed Turn.
    pub async fn record_direct_thread(
        &self,
        request_id: String,
        codex_thread_id: String,
    ) -> Result<()> {
        self.call(true,move|c|{
            let tx=c.transaction()?;
            let (discord_thread,existing):(String,Option<String>)=tx.query_row(
                "SELECT d.discord_thread_id,c.codex_thread_id FROM direct_dispatches d JOIN direct_conversations c ON c.discord_thread_id=d.discord_thread_id WHERE d.request_id=?1 AND d.send_state='SENDING'",
                [&request_id],|r|Ok((r.get(0)?,r.get(1)?)),
            ).optional()?.context("direct send not in progress")?;
            ensure!(existing.as_deref().is_none_or(|old|old==codex_thread_id),"Codex thread changed");
            tx.execute("UPDATE direct_conversations SET codex_thread_id=?2 WHERE discord_thread_id=?1",params![discord_thread,codex_thread_id])?;
            tx.execute("UPDATE direct_dispatches SET codex_thread_id=?2,updated_at=?3,version=version+1 WHERE request_id=?1",params![request_id,codex_thread_id,domain::now_ms()])?;
            tx.commit()?;
            Ok(())
        }).await
    }

    /// No automatic replay follows this transition. Later trusted Codex
    /// inspection may correct the state while retaining the audit record.
    pub async fn mark_direct_unknown(&self, request_id: String, reason: String) -> Result<()> {
        self.call(true, move |c| {
            let tx = c.transaction()?;
            let old:String = tx.query_row("SELECT state FROM requests WHERE id=?1",[&request_id],|r|r.get(0))?;
            ensure!(matches!(old.as_str(),"SENDING"|"RUNNING"|"APPROVAL_REQUIRED"|"CANCEL_REQUESTED"|"UNKNOWN"),"request is not uncertain");
            let now=domain::now_ms();
            tx.execute("UPDATE direct_dispatches SET send_state='UNKNOWN',updated_at=?2,version=version+1 WHERE request_id=?1 AND send_state IN ('SENDING','ACKED','UNKNOWN')",params![request_id,now])?;
            tx.execute("UPDATE requests SET state='UNKNOWN',updated_at=?2,version=version+1 WHERE id=?1",params![request_id,now])?;
            tx.execute("INSERT INTO request_events(request_id,old_state,new_state,reason,created_at) VALUES(?1,?2,'UNKNOWN',?3,?4)",params![request_id,old,reason,now])?;
            tx.commit()?;
            Ok(())
        }).await
    }

    /// Call only after `thread/read` confirmed this exact turn. For a completed
    /// turn, its answer bytes must already be durably saved by DirectContent.
    pub async fn finish_direct(
        &self,
        request_id: String,
        identity: TurnIdentity,
        status: String,
        answer: Option<StoredAnswer>,
    ) -> Result<()> {
        let request_state = match status.as_str() {
            "completed" => "COMPLETED",
            "failed" => "FAILED",
            "interrupted" => "CANCELLED",
            _ => anyhow::bail!("unconfirmed Codex terminal status"),
        };
        ensure!(
            status != "completed" || answer.is_some(),
            "completed answer is not stored"
        );
        self.call(true,move|c|{
            let tx=c.transaction()?;
            let (old,thread,turn):(String,Option<String>,Option<String>)=tx.query_row(
                "SELECT r.state,d.codex_thread_id,d.codex_turn_id FROM requests r JOIN direct_dispatches d ON d.request_id=r.id WHERE r.id=?1 AND d.send_state IN ('ACKED','UNKNOWN')",
                [&request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            ).optional()?.context("direct turn not eligible for terminal confirmation")?;
            ensure!(thread.as_deref()==Some(&identity.thread_id) && turn.as_deref()==Some(&identity.turn_id),"Codex terminal identity mismatch");
            ensure!(matches!(old.as_str(),"RUNNING"|"APPROVAL_REQUIRED"|"CANCEL_REQUESTED"|"UNKNOWN"),"request is not active or uncertain");
            let now=domain::now_ms();
            if let Some(answer)=answer {
                tx.execute("INSERT INTO direct_answers(request_id,relative_path,sha256,bytes,stored_at) VALUES(?1,?2,?3,?4,?5)",params![request_id,answer.relative_path,answer.sha256,answer.bytes as i64,now])?;
            }
            tx.execute("UPDATE direct_dispatches SET send_state='TERMINAL',terminal_status=?2,updated_at=?3,version=version+1 WHERE request_id=?1",params![request_id,status,now])?;
            tx.execute("UPDATE requests SET state=?2,updated_at=?3,version=version+1 WHERE id=?1",params![request_id,request_state,now])?;
            tx.execute("UPDATE holds SET released=1,generation=generation+1 WHERE request_id=?1 AND released=0",[&request_id])?;
            tx.execute("UPDATE conversations SET continuation='READY',effective_model=(SELECT model FROM requests WHERE id=?1),effective_sequence=(SELECT sequence FROM requests WHERE id=?1),last_success_sequence=CASE WHEN ?2='COMPLETED' THEN (SELECT sequence FROM requests WHERE id=?1) ELSE last_success_sequence END WHERE thread_id=(SELECT discord_thread_id FROM direct_dispatches WHERE request_id=?1) AND continuation='VERIFYING'",params![request_id,request_state])?;
            tx.execute("INSERT INTO request_events(request_id,old_state,new_state,reason,created_at) VALUES(?1,?2,?3,'codex_terminal_verified',?4)",params![request_id,old,request_state,now])?;
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn fence_direct_after_restart(&self) -> Result<usize> {
        self.call(true, move |c| {
            let tx=c.transaction()?;
            let mut ids=Vec::new();
            {
                let mut query=tx.prepare("SELECT request_id FROM direct_dispatches WHERE send_state IN ('SENDING','ACKED')")?;
                let rows=query.query_map([],|r|r.get::<_,String>(0))?;
                for row in rows { ids.push(row?); }
            }
            let now=domain::now_ms();
            for id in &ids {
                let old:String=tx.query_row("SELECT state FROM requests WHERE id=?1",[id],|r|r.get(0))?;
                tx.execute("UPDATE direct_dispatches SET send_state='UNKNOWN',updated_at=?2,version=version+1 WHERE request_id=?1",params![id,now])?;
                tx.execute("UPDATE requests SET state='UNKNOWN',updated_at=?2,version=version+1 WHERE id=?1 AND state NOT IN ('COMPLETED','FAILED','CANCELLED')",params![id,now])?;
                tx.execute("INSERT INTO request_events(request_id,old_state,new_state,reason,created_at) VALUES(?1,?2,'UNKNOWN','direct_runtime_restarted',?3)",params![id,old,now])?;
            }
            tx.commit()?;
            Ok(ids.len())
        }).await
    }
}
