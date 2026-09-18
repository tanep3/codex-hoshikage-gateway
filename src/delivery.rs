use crate::{
    discord::{Discord, snowflake},
    domain::{self, digest},
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
#[derive(Clone)]
pub struct Delivery {
    pub store: Store,
    pub discord: Discord,
}
#[derive(Debug)]
struct Record {
    id: String,
    message: Option<String>,
    state: String,
    confirmed: Option<String>,
    pending: Option<String>,
}
impl Delivery {
    /// One dispatcher owns all calls for a given target/kind/part. No automatic POST retry.
    pub async fn text(
        &self,
        target: &str,
        thread: &str,
        kind: &str,
        part: i64,
        text: &str,
        components: Value,
    ) -> Result<bool> {
        ensure!(
            text.encode_utf16().count() <= 2000,
            "Discord text exceeds limit"
        );
        if kind == "status" {
            let (t, ch) = (target.to_owned(), thread.to_owned());
            let deleting: bool=self.store.call(false,move|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM deliveries WHERE target_id=?1 AND thread_id=?2 AND kind='status' AND state IN ('DELETE_PENDING','DELETED'))",params![t,ch],|r|r.get(0))?)).await?;
            if deleting && !self.clear_status(target, thread).await? {
                return Ok(false);
            }
        }
        let target = target.to_owned();
        let thread = thread.to_owned();
        let kind = kind.to_owned();
        let fingerprint = digest(&serde_json::to_vec(&(
            text,
            canonical_components(&components),
        ))?);
        let hash = fingerprint.clone();
        let t = thread.clone();
        let record=self.store.call(false,move|c|{
            let tx=c.transaction()?;
            let existing=tx.query_row("SELECT id,message_id,state,confirmed_digest,pending_digest FROM deliveries WHERE target_id=?1 AND kind=?2 AND part=?3",params![target,kind,part],|r|Ok(Record{id:r.get(0)?,message:r.get(1)?,state:r.get(2)?,confirmed:r.get(3)?,pending:r.get(4)?})).optional()?;
            let record=if let Some(mut r)=existing{
                if r.state=="CONFIRMED"&&r.confirmed.as_ref()!=Some(&hash){
                    tx.execute("UPDATE deliveries SET state='PATCH_PENDING',pending_revision=confirmed_revision+1,pending_digest=?2 WHERE id=?1",params![r.id,hash])?;r.state="PATCH_PENDING".into();r.pending=Some(hash);
                }else{return Ok((r,false));}r
            }else{
                let id=domain::id();tx.execute("INSERT INTO deliveries(id,target_id,thread_id,kind,part,state,pending_revision,pending_digest,created_at) VALUES(?1,?2,?3,?4,?5,'POST_PENDING',1,?6,?7)",params![id,target,t,kind,part,hash,domain::now_ms()])?;
                Record{id,message:None,state:"POST_PENDING".into(),confirmed:None,pending:Some(hash)}
            };tx.commit()?;Ok((record,true))
        }).await?;
        let (r, send) = record;
        if !send {
            if r.state == "CONFIRMED" {
                return Ok(r.confirmed.as_ref() == Some(&fingerprint));
            }
            let reconciled = self
                .reconcile(&r.id, &thread, r.message.as_deref(), r.pending.as_deref())
                .await?;
            // Reconciliation proves only the persisted pending version, not the
            // newer content requested by this caller. Update it on the next pass.
            return Ok(reconciled && r.pending.as_ref() == Some(&fingerprint));
        }
        let body = json!({"content":text,"components":components,"allowed_mentions":{"parse":[]},"nonce":nonce(&r.id),"enforce_nonce":true});
        let response = match &r.message {
            Some(mid) => {
                self.discord
                    .api(
                        reqwest::Method::PATCH,
                        &format!(
                            "/channels/{}/messages/{}",
                            snowflake(&thread)?,
                            snowflake(mid)?
                        ),
                        Some(body),
                    )
                    .await
            }
            None => {
                self.discord
                    .api(
                        reqwest::Method::POST,
                        &format!("/channels/{}/messages", snowflake(&thread)?),
                        Some(body),
                    )
                    .await
            }
        };
        match response {
            Ok(v) => {
                let mid = v["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Discord receipt missing"))?;
                ensure!(
                    v["channel_id"] == thread,
                    "Discord receipt channel mismatch"
                );
                self.confirm(&r.id, mid, &fingerprint).await?;
                Ok(true)
            }
            Err(_) => Ok(false), // The durable pending state is authoritative, including ambiguous 429/5xx.
        }
    }
    async fn confirm(&self, id: &str, message: &str, hash: &str) -> Result<()> {
        let (id, message, hash) = (id.to_owned(), message.to_owned(), hash.to_owned());
        self.store.call(true,move|c|{c.execute("UPDATE deliveries SET message_id=?2,state='CONFIRMED',confirmed_revision=pending_revision,confirmed_digest=pending_digest,pending_revision=NULL,pending_digest=NULL WHERE id=?1 AND pending_digest=?3",params![id,message,hash])?;Ok(())}).await
    }
    async fn reconcile(
        &self,
        id: &str,
        thread: &str,
        message: Option<&str>,
        hash: Option<&str>,
    ) -> Result<bool> {
        let Some(hash) = hash else { return Ok(false) };
        let found = if let Some(mid) = message {
            self.discord
                .get(&format!(
                    "/channels/{}/messages/{}",
                    snowflake(thread)?,
                    snowflake(mid)?
                ))
                .await
                .ok()
        } else {
            self.discord
                .get(&format!(
                    "/channels/{}/messages?limit=100",
                    snowflake(thread)?
                ))
                .await
                .ok()
                .and_then(|v| {
                    v.as_array().and_then(|a| {
                        a.iter()
                            .find(|v| v["nonce"].as_str() == Some(&nonce(id)))
                            .cloned()
                    })
                })
        };
        if let Some(v) = found {
            if !self.discord.owns_message(&v) {
                return Ok(false);
            }
            let content = v["content"].as_str().unwrap_or("");
            let components = v["components"].as_array().cloned().unwrap_or_default();
            // Discord may add component IDs/defaults. Content-only messages are directly comparable;
            // component messages stay pending unless the exact structured receipt agrees.
            let actual = digest(&serde_json::to_vec(&(
                content,
                canonical_components(&json!(components)),
            ))?);
            if actual == hash
                && v["channel_id"] == thread
                && let Some(mid) = v["id"].as_str()
            {
                self.confirm(id, mid, hash).await?;
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub async fn recover(&self) -> Result<()> {
        let rows=self.store.call(false,|c|{let mut st=c.prepare("SELECT id,thread_id,message_id,pending_digest FROM deliveries WHERE state IN ('POST_PENDING','PATCH_PENDING') AND kind!='direct-image' ORDER BY created_at LIMIT 50")?;Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
        for (id, t, m, h) in rows {
            let _ = self.reconcile(&id, &t, m.as_deref(), h.as_deref()).await?;
        }
        Ok(())
    }
}
pub fn nonce(id: &str) -> String {
    domain::digest(id.as_bytes())[..24].into()
}
pub fn chunks(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut part = String::new();
    let mut n = 0;
    for c in text.chars() {
        if n + c.len_utf16() > 1800 {
            out.push(std::mem::take(&mut part));
            n = 0;
        }
        n += c.len_utf16();
        part.push(c);
    }
    if !part.is_empty() {
        out.push(part);
    }
    out
}

pub(crate) fn canonical_components(v: &Value) -> Value {
    if let Some(a) = v.as_array() {
        return Value::Array(a.iter().map(canonical_components).collect());
    }
    if let Some(o) = v.as_object() {
        let mut out = serde_json::Map::new();
        for (k, value) in o {
            if [
                "type",
                "style",
                "label",
                "custom_id",
                "components",
                "url",
                "options",
                "value",
                "description",
                "placeholder",
                "emoji",
                "name",
            ]
            .contains(&k.as_str())
                || (k == "id" && value.is_string())
                || (["min_values", "max_values"].contains(&k.as_str()) && value != 1)
                || (k == "default" && value == true)
                || (k == "animated" && value == true)
                || (k == "disabled" && value == true)
            {
                out.insert(k.clone(), canonical_components(value));
            }
        }
        return Value::Object(out);
    }
    v.clone()
}

impl Delivery {
    /// Remove only our persisted surplus parts, after resolving any previous send ambiguity.
    pub async fn trim_answer(&self, target: &str, thread: &str, keep: usize) -> Result<bool> {
        self.trim_parts(target, thread, "answer", keep).await
    }
    pub async fn clear_draft(&self, target: &str, thread: &str) -> Result<bool> {
        self.trim_parts(target, thread, "draft", 0).await
    }
    /// Delete only a confirmed, bot-owned status; preserve its record for audit.
    pub async fn clear_status(&self, target: &str, thread: &str) -> Result<bool> {
        if !self.trim_parts(target, thread, "status", 0).await? {
            return Ok(false);
        }
        let (t, ch) = (target.to_owned(), thread.to_owned());
        self.store.call(true,move|c|{c.execute("UPDATE deliveries SET kind='retired-status-' || id WHERE target_id=?1 AND thread_id=?2 AND kind='status' AND state='DELETED'",params![t,ch])?;Ok(())}).await?;
        Ok(true)
    }
    pub async fn clear_notice(&self, target: &str, thread: &str) -> Result<bool> {
        self.trim_parts(target, thread, "notice", 0).await
    }
    pub async fn clear_mcp_cards(&self, target: &str, thread: &str) -> Result<bool> {
        if !self
            .trim_parts(target, thread, "mcp_description", 0)
            .await?
        {
            return Ok(false);
        }
        self.trim_parts(target, thread, "mcp_action", 0).await
    }
    async fn trim_parts(
        &self,
        target: &str,
        thread: &str,
        kind: &str,
        keep: usize,
    ) -> Result<bool> {
        let (t, ch, kind) = (target.to_owned(), thread.to_owned(), kind.to_owned());
        let rows=self.store.call(false,move|c|{
            let mut st=c.prepare("SELECT id,message_id,state,pending_digest FROM deliveries WHERE target_id=?1 AND thread_id=?2 AND kind=?4 AND part>=?3 AND state!='DELETED'")?;
            Ok(st.query_map(params![t,ch,keep as i64,kind],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await?;
        let mut complete = true;
        for (id, message, state, pending) in rows {
            if !matches!(state.as_str(), "CONFIRMED" | "DELETE_PENDING") {
                if !self
                    .reconcile(&id, thread, message.as_deref(), pending.as_deref())
                    .await?
                {
                    complete = false;
                    continue;
                }
                complete = false;
                continue;
            }
            let mid = message.context("surplus message identity missing")?;
            let i = id.clone();
            self.store
                .call(true, move |c| {
                    c.execute(
                        "UPDATE deliveries SET state='DELETE_PENDING' WHERE id=?1",
                        [i],
                    )?;
                    Ok(())
                })
                .await?;
            if !self
                .discord
                .remove_own_message(thread, &mid)
                .await
                .unwrap_or(false)
            {
                complete = false;
                continue;
            }
            self.store
                .call(true, move |c| {
                    c.execute("UPDATE deliveries SET state='DELETED' WHERE id=?1", [id])?;
                    Ok(())
                })
                .await?;
        }
        Ok(complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_receipt_defaults_preserve_identity_and_detect_changed_options() {
        let sent = json!([{"type":1,"components":[{"type":3,"custom_id":"choose","options":[{"label":"report","value":"artifact-a"}]}]}]);
        let mut received = sent.clone();
        received[0]["id"] = json!(1);
        received[0]["components"][0]["id"] = json!(2);
        received[0]["components"][0]["min_values"] = json!(1);
        received[0]["components"][0]["max_values"] = json!(1);
        received[0]["components"][0]["disabled"] = json!(false);
        assert_eq!(canonical_components(&sent), canonical_components(&received));
        received[0]["components"][0]["options"][0]["value"] = json!("artifact-b");
        assert_ne!(canonical_components(&sent), canonical_components(&received));
    }
}
