//! Durable discovery is independent of response text and of the execution semaphore.
use crate::{
    application::App,
    domain,
    proxy::path_id,
    proxy_v2::{ApiError, Binding},
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use std::{collections::HashSet, sync::atomic::Ordering, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageError {
    pub code: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageItem {
    pub image_id: String,
    pub ordinal: u64,
    pub state: String,
    pub artifact_id: Option<String>,
    pub error: Option<ImageError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub response_id: String,
    pub conversation_id: String,
    pub workspace_id: String,
    pub revision: u64,
    pub state: String,
    pub items: Vec<ImageItem>,
    pub error: Option<ImageError>,
    pub expires_at: Option<String>,
}
impl Snapshot {
    pub fn validate(&self, previous: Option<&Self>) -> Result<()> {
        for id in [&self.response_id, &self.conversation_id, &self.workspace_id] {
            path_id(id)?;
        }
        ensure!(
            self.revision <= i64::MAX as u64 && self.items.len() <= 64,
            "image inventory limit"
        );
        ensure!(
            matches!(self.state.as_str(), "pending" | "complete" | "unknown"),
            "invalid image inventory state"
        );
        if let Some(t) = &self.expires_at {
            OffsetDateTime::parse(t, &Rfc3339)?;
        }
        let (mut ids, mut ordinals) = (HashSet::new(), HashSet::new());
        for i in &self.items {
            path_id(&i.image_id)?;
            ensure!(
                ids.insert(&i.image_id) && ordinals.insert(i.ordinal) && i.ordinal < 64,
                "duplicate image identity"
            );
            ensure!(
                matches!(
                    i.state.as_str(),
                    "creating" | "ready" | "failed" | "unknown"
                ),
                "invalid image item state"
            );
            ensure!(
                self.state != "complete" || i.state != "creating",
                "incomplete final image inventory"
            );
            ensure!(
                i.state != "ready" || i.artifact_id.is_some(),
                "image artifact missing"
            );
            if let Some(a) = &i.artifact_id {
                path_id(a)?;
            }
            if let Some(e) = &i.error {
                path_id(&e.code)?;
            }
        }
        if let Some(e) = &self.error {
            path_id(&e.code)?;
        }
        if let Some(p) = previous {
            ensure!(
                self.response_id == p.response_id
                    && self.conversation_id == p.conversation_id
                    && self.workspace_id == p.workspace_id
                    && self.revision >= p.revision,
                "image inventory identity changed"
            );
            if self.revision == p.revision {
                ensure!(
                    serde_json::to_value(self)? == serde_json::to_value(p)?,
                    "image revision reused"
                );
            }
            if p.state == "complete" {
                ensure!(
                    self.state == "complete" && self.items.len() == p.items.len(),
                    "final image inventory changed"
                );
            }
            for old in &p.items {
                let item = self
                    .items
                    .iter()
                    .find(|i| i.image_id == old.image_id)
                    .context("image disappeared")?;
                ensure!(item.ordinal == old.ordinal, "image ordinal changed");
                if old.state == "ready" {
                    ensure!(
                        item.state == "ready" && item.artifact_id == old.artifact_id,
                        "ready image changed"
                    );
                }
                if let Some(a) = &old.artifact_id {
                    ensure!(
                        item.artifact_id.as_ref() == Some(a),
                        "image artifact changed"
                    );
                }
            }
        }
        Ok(())
    }
}

/// Shared by explicit /get and automatic discovery; no hash-based identity inference.
pub(crate) fn claim_artifact(
    tx: &Transaction<'_>,
    b: &Binding,
    id: &str,
    thread: &str,
    artifact: &str,
) -> Result<(String, bool)> {
    let claimed:Option<String>=tx.query_row("SELECT delivery_id FROM artifact_delivery_claims WHERE instance_id=?1 AND generation=?2 AND artifact_id=?3 AND thread_id=?4",params![b.instance_id,b.generation,artifact,thread],|r|r.get(0)).optional()?;
    if let Some(id) = claimed {
        return Ok((id, false));
    }
    let existing:Option<String>=tx.query_row("SELECT id FROM resource_deliveries WHERE thread_id=?1 AND resource_type='artifact' AND resource_id=?2 ORDER BY created_at,id LIMIT 1",params![thread,artifact],|r|r.get(0)).optional()?;
    let fresh = existing.is_none();
    let delivery = existing.unwrap_or_else(|| id.into());
    if fresh {
        tx.execute("INSERT INTO resource_deliveries(id,thread_id,resource_type,resource_id,request_key,created_at) VALUES(?1,?2,'artifact',?3,?4,?5)",params![delivery,thread,artifact,format!("lease-{delivery}"),domain::now_ms()])?;
    }
    tx.execute(
        "INSERT INTO artifact_delivery_claims VALUES(?1,?2,?3,?4,?5)",
        params![b.instance_id, b.generation, artifact, thread, delivery],
    )?;
    Ok((delivery, fresh))
}
impl App {
    pub async fn generated_images_loop(&self) -> Result<()> {
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=timer.tick()=>{}}
            if self.recovery.load(Ordering::SeqCst) || !self.connected.load(Ordering::SeqCst) {
                continue;
            }
            let settings = self.settings().await;
            if !settings.proxy.gate.is_ready() {
                continue;
            }
            let Ok(caps) = settings.proxy.get("/v2/codex/capabilities").await else {
                continue;
            };
            if caps["features"]["generated_image_artifacts"] != true
                || caps["features"]["response_generated_images"] != true
            {
                continue;
            }
            let Some(binding) = settings.proxy.v2.binding.read().unwrap().clone() else {
                continue;
            };
            // The capability read is not allowed to silently accept a changed recovery generation.
            if caps["instance_id"] != binding.instance_id
                || caps["recovery_generation"] != binding.generation
            {
                continue;
            }
            let grace = caps["limits"]["generated_images_settle_seconds"]
                .as_u64()
                .unwrap_or(600)
                .clamp(1, 315360000);
            let b = binding.clone();
            self.store.call(true,move|c|{
                c.execute("INSERT OR IGNORE INTO generated_image_watches(request_id,thread_id,instance_id,generation,response_id,conversation_id,workspace_id) SELECT r.id,r.thread_id,?1,?2,r.response_id,p.conversation_id,p.workspace_id FROM requests r JOIN proxy_conversations p ON p.thread_id=r.thread_id WHERE r.response_id IS NOT NULL AND p.state='READY' AND p.conversation_id IS NOT NULL AND p.workspace_id IS NOT NULL",params![b.instance_id,b.generation])?;Ok(())
            }).await?;
            let ids=self.store.call(false,|c|{
                let mut s=c.prepare("SELECT request_id FROM generated_image_watches WHERE state='WATCHING' AND next_poll_ms<=?1 ORDER BY next_poll_ms,request_id LIMIT 20")?;
                Ok(s.query_map([domain::now_ms()],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
            }).await?;
            for id in ids {
                if self.cancel.is_cancelled() {
                    return Ok(());
                }
                if let Err(e) = self.poll_generated_images(&id, &binding, grace).await {
                    let api = e.downcast_ref::<ApiError>();
                    let (state, code) = match api {
                        Some(e) if e.code == "generated_images_not_tracked" => {
                            ("UNSUPPORTED", e.code.clone())
                        }
                        Some(e) if e.status == 410 => ("EXPIRED", e.code.clone()),
                        Some(e) if e.status == 403 => ("BLOCKED", e.code.clone()),
                        _ => ("WATCHING", "image_lookup_unconfirmed".into()),
                    };
                    let i = id.clone();
                    let st = state.to_owned();
                    self.store.call(true,move|c|{c.execute("UPDATE generated_image_watches SET state=CASE WHEN deadline_ms IS NOT NULL AND deadline_ms<=?4 THEN 'TIMED_OUT' ELSE ?2 END,error_code=?3,attempts=min(attempts+1,6),next_poll_ms=?4+min(60000,5000*(1 << min(attempts,4))) WHERE request_id=?1",params![i,st,code,domain::now_ms()])?;Ok(())}).await?;
                }
                self.image_progress(&id).await?;
            }
        }
    }
    pub async fn poll_generated_images(&self, id: &str, b: &Binding, grace: u64) -> Result<()> {
        let _guard = self.resource_mutation.read().await;
        let i = id.to_owned();
        let (thread,instance,generation,response,conversation,workspace,deadline):(String,String,String,String,String,String,Option<i64>)=self.store.call(false,move|c|Ok(c.query_row("SELECT thread_id,instance_id,coalesce(accepted_generation,generation),response_id,conversation_id,workspace_id,deadline_ms FROM generated_image_watches WHERE request_id=?1",[i],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?)).await?;
        ensure!(
            instance == b.instance_id && generation == b.generation,
            "image recovery reconciliation required"
        );
        self.authorized_thread(&thread).await?;
        let settings = self.settings().await;
        let terminal = self.store.request(id).await?.state.terminal();
        let now = domain::now_ms();
        if terminal {
            let i = id.to_owned();
            let d = now.saturating_add((grace.clamp(1, 315360000) * 1000) as i64);
            self.store.call(true,move|c|{c.execute("UPDATE generated_image_watches SET terminal_observed_ms=?3,deadline_ms=CASE WHEN deadline_ms IS NULL THEN ?2 ELSE min(deadline_ms,?2) END WHERE request_id=?1 AND terminal_observed_ms IS NULL",params![i,d,now])?;Ok(())}).await?;
        }
        if deadline.is_some_and(|d| d <= now) {
            let i = id.to_owned();
            self.store.call(true,move|c|{c.execute("UPDATE generated_image_watches SET state='TIMED_OUT',error_code='image_watch_deadline' WHERE request_id=?1",[i])?;Ok(())}).await?;
            return Ok(());
        }
        let v = settings
            .proxy
            .v2_json(
                Method::GET,
                &format!(
                    "/v2/codex/responses/{}/generated-images",
                    path_id(&response)?
                ),
                None,
                None,
            )
            .await?;
        ensure!(
            serde_json::to_vec(&v)?.len() <= 128 * 1024,
            "image snapshot too large"
        );
        let snapshot: Snapshot = serde_json::from_value(v)?;
        ensure!(
            snapshot.response_id == response
                && snapshot.conversation_id == conversation
                && snapshot.workspace_id == workspace,
            "image source mismatch"
        );
        self.apply_image_snapshot(id, b, snapshot).await
    }
    pub async fn apply_image_snapshot(&self, id: &str, b: &Binding, s: Snapshot) -> Result<()> {
        let (i, b) = (id.to_owned(), b.clone());
        self.store.call(true,move|c|{
            let tx=c.transaction()?;
            ensure!(!crate::storage::recovery_pending(&tx)?,"recovery pending");
            let (thread,instance,generation,response,cv,ws,old):(String,String,String,String,String,String,Option<String>)=tx.query_row("SELECT thread_id,instance_id,coalesce(accepted_generation,generation),response_id,conversation_id,workspace_id,snapshot FROM generated_image_watches WHERE request_id=?1",[&i],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
            ensure!(instance==b.instance_id&&generation==b.generation&&s.response_id==response&&s.conversation_id==cv&&s.workspace_id==ws,"image watch identity mismatch");
            let old:Option<Snapshot>=old.map(|v|serde_json::from_str(&v)).transpose()?;
            s.validate(old.as_ref())?;
            let raw=serde_json::to_string(&s)?;let digest=domain::digest(raw.as_bytes());
            let expires=s.expires_at.as_ref().map(|t|OffsetDateTime::parse(t,&Rfc3339).map(|v|(v.unix_timestamp_nanos()/1_000_000) as i64)).transpose()?;
            tx.execute("UPDATE generated_image_watches SET revision=?2,digest=?3,snapshot=?4,inventory_state=?5,error_code=NULL,attempts=CASE WHEN revision=?2 THEN min(attempts+1,6) ELSE 0 END,next_poll_ms=?6+CASE WHEN revision=?2 THEN min(55000,5000*(1 << min(attempts,4))) ELSE 0 END,deadline_ms=CASE WHEN ?7 IS NULL THEN deadline_ms WHEN deadline_ms IS NULL THEN ?7 ELSE min(deadline_ms,?7) END WHERE request_id=?1",params![i,s.revision as i64,digest,raw,s.state,domain::now_ms()+5000,expires])?;
            let mut items=s.items.clone();items.sort_by_key(|x|x.ordinal);
            for item in items {
                tx.execute("INSERT INTO generated_image_items(request_id,image_id,ordinal,state,artifact_id,first_seen_ms) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(request_id,image_id) DO UPDATE SET state=excluded.state,artifact_id=excluded.artifact_id",params![i,item.image_id,item.ordinal as i64,item.state,item.artifact_id,domain::now_ms()])?;
                if item.state=="ready" {
                    let artifact=item.artifact_id.context("image artifact missing")?;
                    let (delivery,fresh)=claim_artifact(&tx,&b,&domain::id(),&thread,&artifact)?;
                    if fresh {tx.execute("UPDATE resource_deliveries SET image_request_id=?2,image_ordinal=?3 WHERE id=?1",params![delivery,i,item.ordinal as i64])?;}
                    tx.execute("UPDATE generated_image_items SET delivery_id=?3 WHERE request_id=?1 AND image_id=?2",params![i,item.image_id,delivery])?;
                }
            }
            tx.commit()?;Ok(())
        }).await
    }
    pub async fn image_progress(&self, id: &str) -> Result<()> {
        let i = id.to_owned();
        let (thread,state,inventory,total,ready,delivered):(String,String,Option<String>,i64,i64,i64)=self.store.call(false,move|c|Ok(c.query_row("SELECT w.thread_id,w.state,w.inventory_state,(SELECT count(*) FROM generated_image_items WHERE request_id=w.request_id),(SELECT count(*) FROM generated_image_items WHERE request_id=w.request_id AND state='ready'),(SELECT count(*) FROM generated_image_items i JOIN resource_deliveries d ON d.id=i.delivery_id WHERE i.request_id=w.request_id AND (d.message_id IS NOT NULL OR d.state IN ('DELIVERED','RELEASE_PENDING'))) FROM generated_image_watches w WHERE request_id=?1",[i],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?)).await?;
        if inventory.as_deref() == Some("complete") && total == ready && ready == delivered {
            let i = id.to_owned();
            self.store
                .call(true, move |c| {
                    c.execute(
                        "UPDATE generated_image_watches SET state='DONE' WHERE request_id=?1",
                        [i],
                    )?;
                    Ok(())
                })
                .await?;
            let n = format!("image-progress-{id}");
            let text = if total == 0 {
                "画像の確認が終わりました。この回答に生成画像はありません。".to_owned()
            } else {
                format!("画像{total}件の配信を確認できました。")
            };
            self.store
                .call(false, move |c| {
                    c.execute("UPDATE notices SET code=?2 WHERE id=?1", params![n, text])?;
                    Ok(())
                })
                .await?;
            return Ok(());
        }
        if state == "UNSUPPORTED" || state == "DONE" {
            return Ok(());
        }
        let i = id.to_owned();
        let waiting_long: bool = self.store.call(false, move |c| Ok(c.query_row("SELECT coalesce(terminal_observed_ms <= ?2,0) FROM generated_image_watches WHERE request_id=?1", params![i,domain::now_ms()-10_000], |r|r.get(0))?)).await?;
        if state == "WATCHING"
            && (total > delivered || inventory.as_deref() == Some("pending") && waiting_long)
        {
            let text = if total > 0 {
                format!(
                    "画像を準備・送信中です（{total}件中{delivered}件配信済み）。もう少しお待ちください。"
                )
            } else {
                "画像の有無・準備状況を確認しています。もう少しお待ちください。".into()
            };
            let n = format!("image-progress-{id}");
            let t = thread.clone();
            self.store.call(false,move|c|{c.execute("INSERT INTO notices VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET code=excluded.code",params![n,t,text,domain::now_ms()])?;Ok(())}).await?;
        }
        let i = id.to_owned();
        let failed_delivery:bool=self.store.call(false,move|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM generated_image_items i JOIN resource_deliveries d ON d.id=i.delivery_id WHERE i.request_id=?1 AND d.state IN ('FAILED','EXPIRED','BLOCKED'))",[i],|r|r.get(0))?)).await?;
        if inventory.as_deref() == Some("unknown")
            || matches!(state.as_str(), "TIMED_OUT" | "EXPIRED" | "BLOCKED")
            || inventory.as_deref() == Some("complete") && (ready < total || failed_delivery)
        {
            let mut text = if inventory.as_deref() == Some("complete") {
                format!(
                    "画像{total}件中{delivered}件を配信済みです。一部の画像の登録・配信を確認できていません。AIは再実行していません。"
                )
            } else {
                format!(
                    "画像を{delivered}件配信済みです。画像の総件数・登録完了を確認できていません。AIは再実行していません。"
                )
            };
            let i = id.to_owned();
            let errors:Vec<String>=self.store.call(false,move|c|{let mut q=c.prepare("SELECT DISTINCT d.error_code FROM generated_image_items i JOIN resource_deliveries d ON d.id=i.delivery_id WHERE i.request_id=?1 AND d.error_code IS NOT NULL ORDER BY d.error_code LIMIT 2")?;Ok(q.query_map([i],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
            for code in errors {
                text.push('\n');
                text.push_str(crate::resources::resource_error_message(&code));
            }
            let n = format!("image-progress-{id}");
            self.store.call(false,move|c|{c.execute("INSERT INTO notices VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET code=excluded.code",params![n,thread,text,domain::now_ms()])?;Ok(())}).await?;
        }
        Ok(())
    }
    pub async fn reconcile_images(&self, id: &str) -> Result<()> {
        let i = id.to_owned();
        self.store.call(true,move|c|{c.execute("UPDATE generated_image_watches SET state='WATCHING',deadline_ms=NULL,terminal_observed_ms=NULL,next_poll_ms=0,error_code=NULL WHERE request_id=?1 AND state!='DONE'",[i])?;Ok(())}).await
    }
}

/// Decode with a bounded allocation as well as checking the declared dimensions.
pub fn validate_png(bytes: &[u8], max_pixels: u64) -> Result<()> {
    use std::io::Cursor;
    ensure!(
        image::guess_format(bytes)? == image::ImageFormat::Png,
        "PNG required"
    );
    let (w, h) = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png)
        .into_dimensions()?;
    ensure!(
        w > 0 && h > 0 && u64::from(w) * u64::from(h) <= max_pixels,
        "image dimensions exceeded"
    );
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(max_pixels.saturating_mul(8).min(256 * 1024 * 1024));
    reader.limits(limits);
    reader.decode()?;
    Ok(())
}
