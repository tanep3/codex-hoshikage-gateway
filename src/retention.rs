//! Retention has its own supervisor task: a slow file transfer cannot starve lease renewal.
use crate::{
    application::App,
    domain,
    proxy::{field, path_id},
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Returns a bounded extension only while a lease is active and approaching expiration.
pub fn renewal_deadline(
    now: &str,
    hold: &str,
    max: &str,
    window_secs: u64,
) -> Result<Option<String>> {
    let now = OffsetDateTime::parse(now, &Rfc3339)?;
    let hold = OffsetDateTime::parse(hold, &Rfc3339)?;
    let max = OffsetDateTime::parse(max, &Rfc3339)?;
    ensure!(hold > now, "lease expired");
    ensure!(max >= hold, "invalid lease lifetime");
    let window = time::Duration::seconds(i64::try_from(window_secs.clamp(60, 86400))?);
    if hold - now > window / 2 || hold == max {
        return Ok(None);
    }
    let next = std::cmp::min(max, now + window);
    Ok((next > hold).then(|| next.format(&Rfc3339)).transpose()?)
}
impl App {
    pub async fn retention_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{}}
            if self.recovery.load(std::sync::atomic::Ordering::SeqCst) {
                continue;
            }
            let s = self.settings().await;
            if !s.proxy.gate.is_ready() {
                continue;
            }
            let rows=self.store.call(false,|c|{
                let mut st=c.prepare("SELECT id,lease_id,resource_type,resource_id FROM resource_deliveries WHERE lease_id IS NOT NULL AND state IN ('WAITING','CACHED','POST_PENDING') ORDER BY hold_until LIMIT 100")?;
                Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
            }).await?;
            if rows.is_empty() {
                continue;
            }
            // Use Proxy's clock to avoid silently promising retention from a skewed Gateway clock.
            let Ok(caps) = s.proxy.get("/v2/codex/capabilities").await else {
                continue;
            };
            let Ok(now) = field(&caps, "server_time") else {
                tracing::warn!(event = "retention_server_time_missing");
                continue;
            };
            for (id, lease, kind, resource) in rows {
                if self.cancel.is_cancelled() {
                    return Ok(());
                }
                if self
                    .renew_resource_lease(&id, &lease, &kind, &resource, &now)
                    .await
                    .is_err()
                {
                    let i = id.clone();
                    self.store.call(false,move|c|{c.execute("UPDATE resource_deliveries SET error_code='retention_unconfirmed' WHERE id=?1",[i])?;Ok(())}).await?;
                    tracing::warn!(event="retention_unconfirmed",delivery_id=%id);
                }
            }
        }
    }
    async fn renew_resource_lease(
        &self,
        id: &str,
        lease: &str,
        kind: &str,
        resource: &str,
        now: &str,
    ) -> Result<()> {
        let s = self.settings().await;
        let path = format!("/v2/codex/leases/{}", path_id(lease)?);
        let current = s.proxy.v2_json(Method::GET, &path, None, None).await?;
        ensure!(
            current["lease_id"] == lease
                && current["resource"]["type"] == kind
                && current["resource"]["id"] == resource,
            "lease identity mismatch"
        );
        ensure!(current["state"] == "active", "lease no longer active");
        let hold = field(&current, "hold_until")?;
        let max = field(&current, "max_hold_until")?;
        let key = format!(
            "extend-{}-{}",
            path_id(lease)?,
            &domain::digest(hold.as_bytes())[..16]
        );
        // An unknown extension must be polled with its original deadline, not a newly computed body.
        let k = key.clone();
        let saved: Option<String> = self
            .store
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT request_json FROM remote_operations WHERE request_key=?1",
                    [k],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await?;
        let body: Value = if let Some(saved) = saved {
            serde_json::from_str(&saved)?
        } else {
            let Some(next) =
                renewal_deadline(now, &hold, &max, s.cfg.limits.delivery_retention_secs)?
            else {
                return Ok(());
            };
            json!({"hold_until":next})
        };
        s.proxy
            .metadata_operation(
                &self.store,
                &key,
                "lease.extend",
                &format!("{path}/extend"),
                body.clone(),
            )
            .await?;
        let confirmed = s.proxy.v2_json(Method::GET, &path, None, None).await?;
        ensure!(
            confirmed["lease_id"] == lease
                && confirmed["resource"] == current["resource"]
                && confirmed["state"] == "active",
            "extension not confirmed"
        );
        let actual = field(&confirmed, "hold_until")?;
        ensure!(
            OffsetDateTime::parse(&actual, &Rfc3339)?
                >= OffsetDateTime::parse(
                    body["hold_until"]
                        .as_str()
                        .context("extension deadline missing")?,
                    &Rfc3339
                )?,
            "extension pending"
        );
        let (i, l) = (id.to_owned(), lease.to_owned());
        self.store.call(false,move|c|{c.execute("UPDATE resource_deliveries SET hold_until=?3,error_code=CASE WHEN error_code='retention_unconfirmed' THEN NULL ELSE error_code END WHERE id=?1 AND lease_id=?2",params![i,l,actual])?;Ok(())}).await
    }
}
