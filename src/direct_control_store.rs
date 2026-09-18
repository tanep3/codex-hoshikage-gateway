//! Durable control send boundaries. An uncertain interrupt or steer is never
//! repeated merely because the Discord interaction is delivered again.
use crate::{codex_execution::TurnIdentity, domain, storage::Store};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

impl Store {
    pub async fn begin_direct_interrupt(
        &self,
        interaction: String,
        request_id: String,
        identity: TurnIdentity,
    ) -> Result<()> {
        self.call(true,move|connection|{
            let tx=connection.transaction()?;
            let (kind,thread,target,state):(String,String,Option<String>,String)=tx.query_row(
                "SELECT kind,thread_id,target_request_id,send_state FROM operations WHERE interaction_id=?1",
                [&interaction],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            )?;
            ensure!(matches!(kind.as_str(),"stop"|"cancel") && state=="NOT_SENT" && target.as_deref().is_none_or(|value|value==request_id),"interrupt operation was already sent or changed");
            let bound:(String,String,String)=tx.query_row("SELECT d.discord_thread_id,d.codex_thread_id,d.codex_turn_id FROM direct_dispatches d WHERE d.request_id=?1 AND d.send_state='ACKED'",[&request_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
            ensure!(bound.0==thread && bound.1==identity.thread_id && bound.2==identity.turn_id,"interrupt target changed");
            let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM holds h JOIN requests r ON r.id=h.request_id WHERE h.request_id=?1 AND h.released=0 AND r.state IN ('RUNNING','APPROVAL_REQUIRED'))",[&request_id],|row|row.get(0))?;
            ensure!(active,"interrupt target is no longer active");
            ensure!(tx.execute("UPDATE operations SET target_request_id=?2,target_thread_id=?3,target_turn_id=?4,send_state='SENDING' WHERE interaction_id=?1 AND send_state='NOT_SENT'",params![interaction,request_id,identity.thread_id,identity.turn_id])?==1,"interrupt send boundary changed");
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn begin_direct_steer(
        &self,
        interaction: String,
        request_id: String,
        identity: TurnIdentity,
        input_digest: String,
    ) -> Result<()> {
        self.call(true,move|connection|{
            let tx=connection.transaction()?;
            let thread:String=tx.query_row("SELECT discord_thread_id FROM direct_dispatches WHERE request_id=?1 AND send_state='ACKED' AND codex_thread_id=?2 AND codex_turn_id=?3",params![request_id,identity.thread_id,identity.turn_id],|row|row.get(0))?;
            let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM holds h JOIN requests r ON r.id=h.request_id WHERE h.request_id=?1 AND h.released=0 AND r.state IN ('RUNNING','APPROVAL_REQUIRED') AND r.stop_requested=0)",[&request_id],|row|row.get(0))?;
            ensure!(active,"steer target is no longer active");
            let existing:Option<String>=tx.query_row("SELECT id FROM operations WHERE interaction_id=?1",[&interaction],|row|row.get(0)).optional()?;
            ensure!(existing.is_none(),"steer interaction was already handled");
            tx.execute("INSERT INTO operations(id,interaction_id,thread_id,kind,target_request_id,target_thread_id,target_turn_id,input_digest,state,send_state,created_at) VALUES(?1,?2,?3,'direct_steer',?4,?5,?6,?7,'APPLIED','SENDING',?8)",params![domain::id(),interaction,thread,request_id,identity.thread_id,identity.turn_id,input_digest,domain::now_ms()])?;
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn finish_direct_control(
        &self,
        interaction: String,
        acknowledged: bool,
    ) -> Result<()> {
        self.call(true,move|connection|{
            ensure!(connection.execute("UPDATE operations SET send_state=?2 WHERE interaction_id=?1 AND kind IN ('stop','cancel','direct_steer') AND send_state='SENDING'",params![interaction,if acknowledged{"SENT"}else{"UNKNOWN"}])?==1,"control send outcome changed");
            Ok(())
        }).await
    }
}
