//! Durable approval send boundary for Gateway-owned App Server requests.
use crate::{
    direct_approval::{DirectInteraction, ManualDecision},
    domain,
    storage::Store,
};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

impl Store {
    pub async fn record_direct_interaction(
        &self,
        request_id: String,
        interaction: DirectInteraction,
    ) -> Result<String> {
        self.call(true, move |c| {
            let tx = c.transaction()?;
            let identity: (Option<String>, Option<String>, String) = tx.query_row(
                "SELECT codex_thread_id,codex_turn_id,send_state FROM direct_dispatches WHERE request_id=?1",
                [&request_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            ensure!(
                identity.0.as_deref() == Some(interaction.thread_id.as_str())
                    && identity.1.as_deref() == Some(interaction.turn_id.as_str())
                    && identity.2 == "ACKED",
                "approval does not belong to the active direct turn"
            );
            let rpc = serde_json::to_string(&interaction.rpc_id)?;
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT id,fingerprint FROM direct_interactions WHERE request_id=?1 AND rpc_id_json=?2",
                    params![request_id, rpc],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((id, fingerprint)) = existing {
                ensure!(
                    fingerprint == interaction.fingerprint,
                    "App Server request ID reused for another operation"
                );
                return Ok(id);
            }
            let id = domain::id();
            let offered = interaction
                .params
                .get("availableDecisions")
                .map(serde_json::to_string)
                .transpose()?;
            tx.execute(
                "INSERT INTO direct_interactions(id,request_id,rpc_id_json,method,thread_id,turn_id,item_id,fingerprint,offered_decisions_json,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'PENDING',?10,?10)",
                params![id,request_id,rpc,interaction.method,interaction.thread_id,interaction.turn_id,interaction.item_id,interaction.fingerprint,offered,domain::now_ms()],
            )?;
            ensure!(
                tx.execute(
                    "UPDATE requests SET state='APPROVAL_REQUIRED',version=version+1,updated_at=?2 WHERE id=?1 AND state IN ('RUNNING','APPROVAL_REQUIRED')",
                    params![request_id,domain::now_ms()],
                )? == 1,
                "approval request is not running"
            );
            tx.commit()?;
            Ok(id)
        })
        .await
    }

    /// This commit precedes the JSON-RPC reply write. A crash after it may
    /// leave an unknown outcome and must not silently send the same decision.
    pub async fn begin_direct_approval_reply(
        &self,
        request_id: String,
        interaction_id: String,
        expected_fingerprint: String,
        decision: ManualDecision,
    ) -> Result<Value> {
        self.begin_direct_approval_reply_inner(
            request_id,
            interaction_id,
            expected_fingerprint,
            decision,
            false,
        )
        .await
    }

    pub async fn begin_direct_mcp_reply(
        &self,
        request_id: String,
        interaction_id: String,
        expected_fingerprint: String,
        decision: ManualDecision,
    ) -> Result<Value> {
        self.begin_direct_approval_reply_inner(
            request_id,
            interaction_id,
            expected_fingerprint,
            decision,
            true,
        )
        .await
    }

    async fn begin_direct_approval_reply_inner(
        &self,
        request_id: String,
        interaction_id: String,
        expected_fingerprint: String,
        decision: ManualDecision,
        mcp: bool,
    ) -> Result<Value> {
        self.call(true, move |c| {
            let tx = c.transaction()?;
            let (rpc, fingerprint, state, dispatch, method, offered): (String, String, String, String, String, Option<String>) = tx.query_row(
                "SELECT i.rpc_id_json,i.fingerprint,i.state,d.send_state,i.method,i.offered_decisions_json FROM direct_interactions i JOIN direct_dispatches d ON d.request_id=i.request_id WHERE i.id=?1 AND i.request_id=?2",
                params![interaction_id,request_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
            )?;
            ensure!(
                fingerprint == expected_fingerprint && state == "PENDING" && dispatch == "ACKED",
                "approval is stale or does not match the displayed operation"
            );
            ensure!(
                if mcp {method == "item/tool/requestUserInput"} else {
                    matches!(method.as_str(),
                        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval")
                },
                "approval requires a different method-specific reply"
            );
            let choice = match (mcp,decision) {
                (true,ManualDecision::AcceptOnce)=>"accept",
                (true,ManualDecision::Decline | ManualDecision::Cancel)=>"decline",
                (false,ManualDecision::AcceptOnce)=>"accept",
                (false,ManualDecision::Decline)=>"decline",
                (false,ManualDecision::Cancel)=>"cancel",
            };
            if !mcp && let Some(offered) = offered {
                let choices: Vec<String> = serde_json::from_str(&offered)?;
                ensure!(
                    choices.iter().any(|item| item == choice),
                    "decision not offered by App Server"
                );
            }
            tx.execute(
                "UPDATE direct_interactions SET state='SENDING',decision=?2,updated_at=?3 WHERE id=?1 AND state='PENDING'",
                params![interaction_id,choice,domain::now_ms()],
            )?;
            tx.commit()?;
            Ok(serde_json::from_str(&rpc)?)
        })
        .await
    }

    pub async fn mark_direct_approval_sent(&self, interaction_id: String) -> Result<()> {
        self.call(true, move |c| {
            let changed = c.execute(
                "UPDATE direct_interactions SET state='SENT',updated_at=?2 WHERE id=?1 AND state='SENDING'",
                params![interaction_id,domain::now_ms()],
            )?;
            ensure!(changed == 1, "approval send boundary changed");
            Ok(())
        })
        .await
    }

    pub async fn mark_direct_approval_unknown(&self, interaction_id: String) -> Result<()> {
        self.call(true, move |c| {
            let changed = c.execute(
                "UPDATE direct_interactions SET state='UNKNOWN',updated_at=?2 WHERE id=?1 AND state IN ('SENDING','SENT')",
                params![interaction_id,domain::now_ms()],
            )?;
            ensure!(changed == 1, "approval result is not uncertain");
            Ok(())
        })
        .await
    }

    pub async fn resolve_direct_approval(&self, interaction_id: String) -> Result<()> {
        self.call(true, move |c| {
            let tx = c.transaction()?;
            let request_id: String = tx.query_row(
                "SELECT request_id FROM direct_interactions WHERE id=?1",
                [&interaction_id],
                |row| row.get(0),
            )?;
            let changed = tx.execute(
                "UPDATE direct_interactions SET state='RESOLVED',updated_at=?2 WHERE id=?1 AND state IN ('SENDING','SENT','UNKNOWN')",
                params![interaction_id,domain::now_ms()],
            )?;
            ensure!(changed == 1, "approval resolution not correlated");
            let pending: i64 = tx.query_row(
                "SELECT count(*) FROM direct_interactions WHERE request_id=?1 AND state IN ('PENDING','SENDING','SENT','UNKNOWN')",
                [&request_id],
                |row| row.get(0),
            )?;
            if pending == 0 {
                tx.execute(
                    "UPDATE requests SET state='RUNNING',version=version+1,updated_at=?2 WHERE id=?1 AND state='APPROVAL_REQUIRED'",
                    params![request_id,domain::now_ms()],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn fence_direct_approvals_after_restart(&self) -> Result<usize> {
        self.call(true, |c| {
            let tx = c.transaction()?;
            let unavailable = tx.execute(
                "UPDATE direct_interactions SET state='UNAVAILABLE',updated_at=?1 WHERE state='PENDING'",
                [domain::now_ms()],
            )?;
            let unknown = tx.execute(
                "UPDATE direct_interactions SET state='UNKNOWN',updated_at=?1 WHERE state IN ('SENDING','SENT')",
                [domain::now_ms()],
            )?;
            tx.commit()?;
            Ok(unavailable + unknown)
        })
        .await
    }

    pub async fn close_direct_approvals(&self, request_id: String) -> Result<usize> {
        self.call(true, move |c| {
            let tx = c.transaction()?;
            let unavailable = tx.execute(
                "UPDATE direct_interactions SET state='UNAVAILABLE',updated_at=?2 WHERE request_id=?1 AND state='PENDING'",
                params![request_id,domain::now_ms()],
            )?;
            let unknown = tx.execute(
                "UPDATE direct_interactions SET state='UNKNOWN',updated_at=?2 WHERE request_id=?1 AND state IN ('SENDING','SENT')",
                params![request_id,domain::now_ms()],
            )?;
            tx.commit()?;
            Ok(unavailable + unknown)
        })
        .await
    }
}
