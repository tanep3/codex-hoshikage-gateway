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
}

impl Store {
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
            ensure!(!tx.prepare("SELECT 1 FROM proxy_conversations WHERE thread_id=?1 AND send_started!=0")?.exists([&discord_thread_id])?, "legacy Proxy conversation has an execution boundary");
            ensure!(!tx.prepare("SELECT 1 FROM requests r WHERE r.thread_id=?1 AND (r.response_id IS NOT NULL OR r.client_request_id IS NOT NULL OR (r.dispatch_started_at IS NOT NULL AND NOT EXISTS(SELECT 1 FROM direct_dispatches d WHERE d.request_id=r.id)))")?.exists([&discord_thread_id])?, "legacy execution requires explicit migration before direct use");
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
            ensure!(tx.execute(
                "UPDATE direct_dispatches SET send_state='SENDING',send_started_at=?3,updated_at=?3,version=version+1 WHERE request_id=?1 AND intent_id=?2 AND send_state='PREPARED'",
                params![request_id,expected_intent,now],
            )?==1,"direct send intent already consumed");
            ensure!(tx.execute(
                "UPDATE requests SET state='SENDING',dispatch_started_at=?2,dispatch_eligible=0,updated_at=?2,version=version+1 WHERE id=?1 AND state='QUEUED' AND dispatch_started_at IS NULL AND dispatch_eligible=1",
                params![request_id,now],
            )?==1,"request is not queued for direct send");
            let value = tx.query_row(
                "SELECT d.request_id,d.intent_id,d.discord_thread_id,c.workspace_path,c.workspace_dev,c.workspace_ino,c.codex_thread_id FROM direct_dispatches d JOIN direct_conversations c ON c.discord_thread_id=d.discord_thread_id WHERE d.request_id=?1",
                [&request_id], |r| Ok(DirectDispatch {
                    request_id:r.get(0)?,intent_id:r.get(1)?,discord_thread_id:r.get(2)?,
                    workspace_path:PathBuf::from(r.get::<_,String>(3)?),
                    workspace_dev:r.get::<_,i64>(4)? as u64,workspace_ino:r.get::<_,i64>(5)? as u64,
                    codex_thread_id:r.get(6)?,
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
