//! Explicit recovery reviews contain metadata only. Missing remote IDs never authorize replay.
use crate::{
    application::App,
    domain,
    proxy::{Proxy, path_id},
    proxy_v2::Binding,
};
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest::Method;
use rusqlite::params;
use serde_json::{Value, json};
impl App {
    async fn recovery_probe(&self) -> Result<(Proxy, Binding)> {
        let s = self.settings().await;
        let p = Proxy::new(s.cfg.proxy.base_url.clone(), s.proxy.secret())?;
        let caps = p.get("/v2/codex/capabilities").await?;
        let mut contract = caps.clone();
        contract["recovery_state"] = json!("ready");
        crate::proxy_v2::validate(&contract)?;
        p.bind_v2(&caps).await?;
        let b =
            p.v2.binding
                .read()
                .unwrap()
                .clone()
                .context("binding missing")?;
        Ok((p, b))
    }
    async fn recovery_inventory(&self) -> Result<Value> {
        self.store.call(true, inventory).await
    }
    async fn probe_inventory(p: Proxy, inventory: &Value) -> Result<Vec<Value>> {
        let entries = inventory["entries"]
            .as_array()
            .context("inventory missing")?
            .clone();
        stream::iter(entries.into_iter().map(|entry|{let p=p.clone();async move{
            let id=path_id(entry["id"].as_str().context("resource identity missing")?)?;
            let path=match entry["kind"].as_str(){Some("conversation")=>format!("/v2/codex/conversations/{id}"),Some("response")=>format!("/v2/codex/responses/{id}"),Some("artifact")=>format!("/v2/codex/artifacts/{id}"),Some("lease")=>format!("/v2/codex/leases/{id}"),Some("operation")=>format!("/v2/codex/operations/by-key/{id}"),_=>anyhow::bail!("unknown recovery type")};
            let remote=match p.v2_json(Method::GET,&path,None,None).await {
                Ok(v)=>v,
                Err(e)=>{let api=e.downcast_ref::<crate::proxy_v2::ApiError>().context("recovery lookup failed")?;ensure!(matches!(api.status,403|404|410),"recovery lookup unavailable");return Ok(json!({"entry":entry,"state":"unavailable","code":api.code}));}
            };
            let identity=match entry["kind"].as_str(){Some("conversation")=>"conversation_id",Some("response")=>"response_id",Some("artifact")=>"artifact_id",Some("lease")=>"lease_id",_=>""};
            if !identity.is_empty(){ensure!(remote[identity]==entry["id"],"recovery identity mismatch");}
            if entry["kind"]=="conversation"&&!entry["expected"].is_null(){ensure!(remote["workspace_id"]==entry["expected"],"recovery workspace changed");}
            if entry["kind"]=="artifact"&&!entry["expected"].is_null(){ensure!(remote["sha256"]==entry["expected"]&&remote["size_bytes"]==entry["size"],"recovery artifact changed");}
            // Never persist a raw response that might contain input or output text.
            Ok::<_,anyhow::Error>(json!({"entry":entry,"state":remote["state"],"execution_status":remote["execution_status"],"phase":remote["phase"],"resource":remote["resource"],"output_state":remote["output"]["state"]}))
        }})).buffered(8).try_collect().await
    }
    pub async fn inspect_proxy_recovery(&self) -> Result<Value> {
        let _guard = self.config_mutation.lock().await;
        let (p, new) = self.recovery_probe().await?;
        let inventory = self.recovery_inventory().await?;
        let old: Binding = serde_json::from_value(inventory["binding"].clone())?;
        ensure!(
            old.instance_id == new.instance_id && old.base_url == new.base_url,
            "different Proxy instance requires a new Gateway instance"
        );
        ensure!(
            old.generation != new.generation,
            "Proxy generation has not changed"
        );
        self.store
            .call(true, |c| {
                c.execute("UPDATE proxy_binding SET blocked=1", [])?;
                Ok(())
            })
            .await?;
        self.settings().await.proxy.gate.invalidate();
        let results = Self::probe_inventory(p, &inventory).await?;
        let missing = results
            .iter()
            .filter(|v| v["state"] == "unavailable")
            .count();
        let token = domain::id();
        let snapshot = json!({"inventory":inventory,"observed":new,"results":results});
        let (t, body) = (token.clone(), serde_json::to_string(&snapshot)?);
        self.store
            .call(true, move |c| {
                c.execute(
                    "DELETE FROM recovery_reviews WHERE expires_at<=?1",
                    [domain::now_ms()],
                )?;
                c.execute(
                    "INSERT INTO recovery_reviews VALUES(?1,?2,?3)",
                    params![t, body, domain::now_ms() + 300_000],
                )?;
                Ok(())
            })
            .await?;
        Ok(
            json!({"review_token":token,"expires_in_seconds":300,"old_generation":old.generation,"new_generation":new.generation,"checked":results.len(),"unavailable":missing,"effect":"Existing pending executions stay quarantined; no automatic AI resend. Conversation queues stay paused."}),
        )
    }
    pub async fn accept_proxy_recovery(
        &self,
        token: &str,
        reason: &str,
        accept_risk: bool,
    ) -> Result<Value> {
        ensure!(
            accept_risk && !reason.trim().is_empty() && reason.len() <= 1024,
            "explicit risk acknowledgement and reason required"
        );
        let _guard = self.config_mutation.lock().await;
        let _deliveries = self.resource_mutation.write().await;
        let t = token.to_owned();
        let raw: String = self
            .store
            .call(true, move |c| {
                Ok(c.query_row(
                    "SELECT snapshot_json FROM recovery_reviews WHERE id=?1 AND expires_at>?2",
                    params![t, domain::now_ms()],
                    |r| r.get(0),
                )?)
            })
            .await?;
        let review: Value = serde_json::from_str(&raw)?;
        ensure!(
            self.recovery_inventory().await? == review["inventory"],
            "local recovery inventory changed; inspect again"
        );
        let (p, observed) = self.recovery_probe().await?;
        ensure!(
            serde_json::to_value(&observed)? == review["observed"],
            "Proxy changed since review"
        );
        let caps = p.get("/v2/codex/capabilities").await?;
        crate::proxy_v2::validate(&caps)?;
        let results = Self::probe_inventory(p.clone(), &review["inventory"]).await?;
        let old: Binding = serde_json::from_value(review["inventory"]["binding"].clone())?;
        let expected_inventory = review["inventory"].clone();
        let (n, t, reason) = (observed.clone(), token.to_owned(), reason.to_owned());
        self.store.call(true,move|c|{
            ensure!(inventory(c)?==expected_inventory,"recovery inventory changed");
            let tx=c.transaction()?;
            ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM recovery_reviews WHERE id=?1 AND expires_at>?2)",params![t,domain::now_ms()],|r|r.get::<_,bool>(0))?,"recovery review expired; inspect again");
            ensure!(tx.execute("UPDATE proxy_binding SET generation=?1,blocked=0 WHERE instance_id=?2 AND generation=?3 AND base_url=?4 AND blocked=1",params![n.generation,n.instance_id,old.generation,n.base_url])?==1,"recovery binding changed");
            tx.execute("UPDATE requests SET dispatch_eligible=0 WHERE state NOT IN ('COMPLETED','FAILED','CANCELLED')",[])?;
            tx.execute("UPDATE conversations SET paused=1",[])?;
            tx.execute("UPDATE admissions SET status='QUARANTINED',version=version+1 WHERE status='VALIDATING'",[])?;
            for result in results {
                if result["state"]=="unavailable" {
                    let entry=&result["entry"];let local=entry["local_id"].as_str().context("local identity missing")?;
                    match entry["kind"].as_str(){
                        Some("conversation")=>{tx.execute("UPDATE conversations SET continuation='NEW_CONVERSATION_REQUIRED' WHERE thread_id=?1",[local])?;},
                        Some("artifact"|"lease")=>{tx.execute("UPDATE resource_deliveries SET state='BLOCKED',error_code='recovery_resource_unavailable' WHERE id=?1",[local])?;},
                        _=>{}
                    }
                }
            }
            tx.execute("INSERT INTO admin_audit VALUES(?1,?2,'proxy_generation_accept',?3,?4,1,?5)",params![domain::id(),unsafe{libc::geteuid()},n.generation,reason,domain::now_ms()])?;
            tx.execute("DELETE FROM recovery_reviews WHERE id=?1",[t])?;tx.commit()?;Ok(())
        }).await?;
        self.output.lock().await.clear();
        let restored: bool = self
            .store
            .call(true, |c| {
                Ok(c.query_row("SELECT recovery_pending FROM schema_meta", [], |r| r.get(0))?)
            })
            .await?;
        self.recovery
            .store(restored, std::sync::atomic::Ordering::SeqCst);
        let mut settings = self.settings.write().await;
        settings.proxy = p.with_store(self.store.clone());
        Ok(
            json!({"generation":observed.generation,"accepted":true,"queues_paused":true,"old_requests_quarantined":true}),
        )
    }
}

fn inventory(c: &mut rusqlite::Connection) -> Result<Value> {
    let binding:Value=c.query_row("SELECT instance_id,generation,base_url FROM proxy_binding",[],|r|Ok(json!({"instance_id":r.get::<_,String>(0)?,"generation":r.get::<_,String>(1)?,"base_url":r.get::<_,String>(2)?})))?;
    let mut entries = Vec::new();
    let queries = [
        (
            "conversation",
            "SELECT thread_id,conversation_id,workspace_id,NULL FROM proxy_conversations WHERE conversation_id IS NOT NULL ORDER BY thread_id",
        ),
        (
            "response",
            "SELECT id,response_id,NULL,NULL FROM requests WHERE response_id IS NOT NULL AND state NOT IN ('FAILED','CANCELLED') ORDER BY id",
        ),
        (
            "operation",
            "SELECT request_key,request_key,NULL,NULL FROM remote_operations WHERE state NOT IN ('failed','succeeded') ORDER BY request_key",
        ),
        (
            "artifact",
            "SELECT id,resource_id,sha256,size_bytes FROM resource_deliveries WHERE resource_type='artifact' AND state NOT IN ('DELIVERED','SUPERSEDED') ORDER BY id",
        ),
        (
            "lease",
            "SELECT id,lease_id,NULL,NULL FROM resource_deliveries WHERE lease_id IS NOT NULL AND state NOT IN ('DELIVERED','SUPERSEDED') ORDER BY id",
        ),
    ];
    for (kind, sql) in queries {
        let mut st = c.prepare(sql)?;
        for row in st.query_map([],|r|Ok(json!({"kind":kind,"local_id":r.get::<_,String>(0)?,"id":r.get::<_,String>(1)?,"expected":r.get::<_,Option<String>>(2)?,"size":r.get::<_,Option<i64>>(3)?})))?{entries.push(row?);}
    }
    // Include executions without a receipt via their persisted request key.
    let mut st=c.prepare("SELECT id,client_request_id FROM requests WHERE response_id IS NULL AND client_request_id IS NOT NULL ORDER BY id")?;
    for row in st.query_map([], |r| {
        Ok(json!({"kind":"operation","local_id":r.get::<_,String>(0)?,"id":r.get::<_,String>(1)?}))
    })? {
        entries.push(row?);
    }
    let mut st = c.prepare("SELECT request_id FROM admissions ORDER BY request_id")?;
    let admissions = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({"binding":binding,"entries":entries,"admissions":admissions}))
}
