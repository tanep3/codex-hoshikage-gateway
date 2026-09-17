//! v0.6 controller. Display text and webhook credentials stay in memory.
use crate::{
    application::App,
    domain,
    mcp_ui::Record,
    mcp_v06::{self, PageReceipts, Presentation},
};
use anyhow::{Context, Result, ensure};
use hmac::{Hmac, Mac};
use reqwest::Method;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub(crate) struct Runtime {
    pub boot: String,
    secret: String,
    views: Mutex<HashMap<String, Arc<Mutex<View>>>>,
    polls: Mutex<HashMap<String, std::time::Instant>>,
    display_slots: tokio::sync::Semaphore,
    failures: Mutex<HashMap<String, u32>>,
    stable_cards: Mutex<HashMap<String, (i64, i64)>>,
}
struct View {
    receipt: PageReceipts,
    presentation: String,
    requester: bool,
    next: u64,
    expires: i64,
    control_message: Option<String>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            boot: domain::id(),
            secret: format!("{}{}", domain::id(), domain::id()),
            views: Mutex::new(HashMap::new()),
            polls: Mutex::new(HashMap::new()),
            display_slots: tokio::sync::Semaphore::new(4),
            failures: Mutex::new(HashMap::new()),
            stable_cards: Mutex::new(HashMap::new()),
        }
    }
}
impl Runtime {
    fn mac(&self, text: &str) -> String {
        let mut h = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes())
            .expect("HMAC accepts arbitrary key length");
        h.update(text.as_bytes());
        h.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}
fn controls(id: &str, rev: i64, view: &str, p: Option<&Presentation>, next: Option<u64>) -> Value {
    if p.is_some_and(|p| p.value()["state"] == "closed") {
        return json!([]);
    }
    let mut buttons = vec![];
    let mut add = |action: &str, label: &str, style: u8| {
        buttons.push(json!({"type":2,"style":style,"custom_id":format!("ma6:{action}:{id}:{rev}:{view}"),"label":label}))
    };
    if let Some(p) = p {
        if p.value()["state"] == "private_required" {
            add("private", "自分だけに表示して確認", 1);
        }
        if let Some(n) = next {
            add(&format!("page{n}"), "次のページを確認", 1);
        } else {
            if p.value()["actions"]["allow_turn_tool"] == true {
                add("turn", "この依頼中、このツールを許可", 3);
            }
            if p.value()["actions"]["allow_once"] == true {
                add("once", "今回だけ許可", 3);
            }
        }
    }
    if p.is_none()
        || p.is_some_and(|p| {
            p.value()["state"] == "unavailable" && p.value()["actions"]["retry"] == true
        })
    {
        add("retry", "再確認", 1);
    }
    add("decline", "拒否", 4);
    json!([{"type":1,"components":buttons}])
}
fn expired(p: &Presentation) -> Result<i64> {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    Ok((OffsetDateTime::parse(
        p.value()["expires_at"].as_str().context("display expiry")?,
        &Rfc3339,
    )?
    .unix_timestamp_nanos()
        / 1_000_000) as i64)
}
impl App {
    async fn verify_v06_page(&self, rec: &Record, p: &Presentation) -> Result<()> {
        let v = p.value();
        ensure!(
            v["interaction_id"] == rec.remote
                && v["response_id"] == rec.response
                && v["revision"] == rec.revision,
            "display target changed"
        );
        if !v["expires_at"].is_null() {
            ensure!(
                expired(p)? <= rec.expires,
                "presentation outlives interaction"
            );
        }
        let ctx = self.context(&rec.request).await?;
        ensure!(
            v["audience"]["channel_id"] == ctx["channel_id"]
                && (v["audience"]["kind"] != "requester"
                    || v["audience"]["principal_id"] == ctx["principal_id"]),
            "display audience changed"
        );
        ensure!(
            self.store
                .v06_selection(&rec.request)
                .await?
                .context("missing v06 run")?
                == p.policy.selection,
            "effective policy differs from request"
        );
        let rid = rec.request.clone();
        let saved: Option<String> = self
            .store
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT execution_policy_json FROM mcp_v06_runs WHERE request_id=?1",
                    [rid],
                    |r| r.get(0),
                )?)
            })
            .await?;
        if let Some(saved) = saved {
            let saved = mcp_v06::ExecutionPolicy::parse(&serde_json::from_str(&saved)?)?;
            ensure!(
                saved.binding_id == p.policy.binding_id
                    && (saved.generation.is_none() || saved.generation == p.policy.generation),
                "display policy differs from persisted response"
            );
        }
        if p.scope.is_some() {
            self.scope_matches(&self.store.request(&rec.request).await?, &v["scope"])
                .await?;
        }
        let s = self.settings().await;
        ensure!(
            s.proxy.v2.binding.read().unwrap().as_ref() == Some(&rec.binding),
            "display proxy generation changed"
        );
        Ok(())
    }
    pub(crate) async fn render_v06_mcp(&self, rec: &Record, thread: &str) -> Result<()> {
        self.authorized_thread(thread).await?;
        {
            let mut polls = self.mcp_v06.polls.lock().await;
            let now = std::time::Instant::now();
            polls.retain(|_, at| *at > now);
            if polls.contains_key(&rec.id) {
                return Ok(());
            }
            polls.insert(rec.id.clone(), now + Duration::from_millis(2000));
        }
        if !self.v06_poll_window(rec, false).await? {
            self.delivery.text(&rec.id,thread,"mcp_action",0,"操作情報の自動確認をいったん止めました。「再確認」で読み直すか、不要な操作なら「拒否」を選んでください。AIは再実行しません。",controls(&rec.id,rec.revision,"-",None,None)).await?;
            return Ok(());
        }
        let _slot = self.mcp_v06.display_slots.acquire().await?;
        let s = self.settings().await;
        let page = s.proxy.approval_v06_page(&rec.remote, false, 0, None).await;
        let p = match page {
            Ok(p) => p,
            Err(error) => {
                let mut failures = self.mcp_v06.failures.lock().await;
                if failures.len() > 512 {
                    failures.clear();
                }
                let count = failures.entry(rec.id.clone()).or_default();
                *count = count.saturating_add(1);
                let delay = error
                    .downcast_ref::<crate::proxy_v2::RetryAfter>()
                    .map(|r| r.0)
                    .unwrap_or(
                        [2000, 4000, 8000, 10000][(*count as usize).saturating_sub(1).min(3)],
                    );
                if let Some(at) =
                    std::time::Instant::now().checked_add(Duration::from_millis(delay))
                {
                    self.mcp_v06.polls.lock().await.insert(rec.id.clone(), at);
                }
                drop(failures);
                self.mark_v06_wait(rec).await?;
                self.delivery.text(&rec.id,thread,"mcp_action",0,"操作内容をまだ取得できません。許可せず待つか、不要な操作なら「拒否」を選んでください。作業全体を止める場合は /stop を使ってください。",controls(&rec.id,rec.revision,"-",None,None)).await?;
                return Ok(());
            }
        };
        self.mcp_v06.failures.lock().await.remove(&rec.id);
        self.verify_v06_page(rec, &p).await?;
        if p.value()["state"] == "private_required" {
            let id = rec.id.clone();
            self.store.call(true,move|c|{c.execute("UPDATE mcp_v06_views SET state='DELIVERING' WHERE interaction_local_id=?1 AND audience='source_conversation' AND state='WAITING' AND active=1",[id])?;Ok(())}).await?;
        } else if p.value()["state"] == "unavailable" {
            self.mark_v06_wait(rec).await?;
            if matches!(
                p.value()["reason"].as_str(),
                Some("catalog_loading" | "catalog_failed")
            ) && self
                .mcp_v06
                .stable_cards
                .lock()
                .await
                .get(&rec.id)
                .is_some_and(|(revision, expires)| {
                    *revision == rec.revision && *expires > domain::now_ms()
                })
            {
                // Preserve the UI while refreshing; every permission click still
                // fetches and validates current evidence before sending anything.
                return Ok(());
            }
        }
        if p.value()["state"] != "ready" {
            let text = if p.value()["state"] == "private_required" {
                if p.value()["reason"] == "display_too_large" {
                    "操作内容が長いため、同じ会話の自分だけに見える画面で全ページを確認してください。"
                } else {
                    "操作に公開できない情報が含まれます。「自分だけに表示して確認」で内容を確認してから選んでください。"
                }
            } else if matches!(
                p.value()["reason"].as_str(),
                Some("catalog_loading" | "catalog_failed")
            ) {
                "操作情報を更新しています。許可はまだ送っていません。画面の更新をお待ちください。自動確認が止まった場合は「再確認」、不要な操作なら「拒否」を選んでください。"
            } else if p.value()["reason"] == "policy_denied" {
                "この依頼に適用中のポリシーにより、この操作は実行できません。「拒否」を選ぶか、作業全体を止める場合は /stop を使ってください。別の操作へ自動で切り替えることはありません。"
            } else if p.value()["reason"] == "policy_check_unavailable" {
                "適用中のポリシー条件を確認できないため、許可は保留しています。待つか「拒否」を選んでください。作業全体の停止は /stop です。"
            } else {
                "操作内容を確認できないため許可できません。不要な操作なら「拒否」、作業全体を止める場合は /stop を選んでください。"
            };
            self.delivery
                .text(
                    &rec.id,
                    thread,
                    "mcp_action",
                    0,
                    text,
                    controls(&rec.id, rec.revision, "-", Some(&p), None),
                )
                .await?;
            if p.value()["state"] == "private_required" {
                self.remember_v06_card(rec, &p).await?;
            }
            return Ok(());
        }
        let id = self.v06_view(rec, &p, false).await?;
        if self
            .v06_display_page(&id, rec, thread, &p, None)
            .await
            .is_err()
        {
            self.delivery.text(&rec.id,thread,"mcp_action",0,"操作内容の表示を確認できません。許可は保留しています。表示が揃うまで待つか、「拒否」を選んでください。",controls(&rec.id,rec.revision,"-",None,None)).await?;
            return Ok(());
        }
        ensure!(
            self.delivery.clear_mcp_cards(&rec.id, thread).await?,
            "old control cleanup incomplete"
        );
        let local = rec.id.clone();
        self.store.call(true,move|c|{c.execute("UPDATE deliveries SET kind='retired-mcp-' || id WHERE target_id=?1 AND kind='mcp_action' AND state='DELETED'",[local])?;Ok(())}).await?;
        self.retire_v06_posts(&rec.id, thread, &id).await?;
        self.remember_v06_card(rec, &p).await?;
        Ok(())
    }
    async fn remember_v06_card(&self, rec: &Record, p: &Presentation) -> Result<()> {
        let mut cards = self.mcp_v06.stable_cards.lock().await;
        cards.retain(|_, (_, expires)| *expires > domain::now_ms());
        if cards.len() < 512 || cards.contains_key(&rec.id) {
            cards.insert(rec.id.clone(), (rec.revision, expired(p)?));
        }
        Ok(())
    }
    async fn v06_view(&self, rec: &Record, p: &Presentation, requester: bool) -> Result<String> {
        let (local, v, boot) = (rec.id.clone(), p.value().clone(), self.mcp_v06.boot.clone());
        let expiry = expired(p)?;
        ensure!(expiry > domain::now_ms(), "expired display");
        let ctx = self.context(&rec.request).await?;
        let viewer = if requester {
            ctx["principal_id"]
                .as_str()
                .context("principal")?
                .to_owned()
        } else {
            String::new()
        };
        let id=self.store.call(true,move|c|{
            let tx=c.transaction()?;
            let old:Option<(String,String,Option<String>,String)>=tx.query_row("SELECT id,boot_id,presentation_fingerprint,state FROM mcp_v06_views WHERE interaction_local_id=?1 AND audience=?2 AND viewer_id=?3 AND active=1",params![local,v["audience"]["kind"].as_str(),viewer],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            if let Some((id,_oldboot,fp,state))=&old && fp.as_deref()==v["presentation_fingerprint"].as_str() && !requester && state!="STALE" {
                let identical:bool=tx.query_row("SELECT revision=?2 AND scope_json=?3 AND scope_fingerprint=?4 AND policy_binding_id=?5 AND presentation_id=?6 AND content_fingerprint=?7 AND page_count=?8 AND expires_at=?9 FROM mcp_v06_views WHERE id=?1",params![id,v["revision"].as_i64(),serde_json::to_string(&v["scope"])?,v["scope_fingerprint"].as_str(),v["execution_policy"]["binding_id"].as_str(),v["presentation_id"].as_str(),v["page"]["content_fingerprint"].as_str(),v["page"]["count"].as_i64(),expiry],|r|r.get(0))?;
                ensure!(identical,"presentation fingerprint reused with different binding");
                return Ok(id.clone());
            }
            tx.execute("UPDATE mcp_v06_views SET active=0,state='STALE' WHERE interaction_local_id=?1 AND audience=?2 AND viewer_id=?3 AND active=1",params![local,v["audience"]["kind"].as_str(),viewer])?;
            let id=domain::id();
            tx.execute("INSERT INTO mcp_v06_views(id,interaction_local_id,audience,viewer_id,revision,scope_json,scope_fingerprint,policy_binding_id,presentation_id,presentation_fingerprint,content_fingerprint,page_count,expires_at,state,boot_id,next_poll_ms,poll_deadline_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'RENDERING',?14,?15,?13)",params![id,local,v["audience"]["kind"].as_str(),viewer,v["revision"].as_i64(),serde_json::to_string(&v["scope"])?,v["scope_fingerprint"].as_str(),v["execution_policy"]["binding_id"].as_str(),v["presentation_id"].as_str(),v["presentation_fingerprint"].as_str(),v["page"]["content_fingerprint"].as_str(),v["page"]["count"].as_i64(),expiry,boot,domain::now_ms()])?;
            tx.commit()?;Ok(id)
        }).await?;
        let mut views = self.mcp_v06.views.lock().await;
        views.retain(|_, cell| {
            cell.try_lock()
                .map(|v| v.expires > domain::now_ms())
                .unwrap_or(true)
        });
        ensure!(
            views.contains_key(&id) || views.len() < 512,
            "too many open approval views"
        );
        // No sensitive text in the receipt; expired entries are never usable.
        if !views.contains_key(&id) {
            views.insert(
                id.clone(),
                Arc::new(Mutex::new(View {
                    receipt: PageReceipts::new(p)?,
                    presentation: p.value()["presentation_id"]
                        .as_str()
                        .context("presentation ID")?
                        .into(),
                    requester,
                    next: 0,
                    expires: expiry,
                    control_message: None,
                })),
            );
        }
        Ok(id)
    }
    async fn v06_display_page(
        &self,
        view: &str,
        rec: &Record,
        thread: &str,
        p: &Presentation,
        webhook: Option<(&str, &str)>,
    ) -> Result<()> {
        self.verify_v06_page(rec, p).await?;
        let cell = self
            .mcp_v06
            .views
            .lock()
            .await
            .get(view)
            .cloned()
            .context("display not loaded")?;
        let mut loaded = cell.lock().await;
        ensure!(
            loaded.expires > domain::now_ms() && loaded.requester == webhook.is_some(),
            "display expired or audience mismatch"
        );
        let n = p.value()["page"]["index"].as_i64().context("page number")?;
        ensure!(n as u64 <= loaded.next, "page skipped");
        let chunks = mcp_v06::render_page(p)?;
        let (vid, token, parts, bytes) = (
            view.to_owned(),
            p.value()["page"]["token"]
                .as_str()
                .context("page token")?
                .to_owned(),
            chunks.len() as i64,
            serde_json::to_vec(&p.value()["display"])?.len() as i64,
        );
        self.store.call(true,move|c|{c.execute("INSERT OR IGNORE INTO mcp_v06_pages VALUES(?1,?2,?3,'PREPARED',?4,?5)",params![vid,n,token,parts,bytes])?;let old:(String,i64)=c.query_row("SELECT page_token,part_count FROM mcp_v06_pages WHERE view_id=?1 AND page_index=?2",params![vid,n],|r|Ok((r.get(0)?,r.get(1)?)))?;ensure!(old==(token,parts),"page changed");Ok(())}).await?;
        let mut ids = vec![];
        for (i, text) in chunks.iter().enumerate() {
            let (vid, ch, boot, mac) = (
                view.to_owned(),
                thread.to_owned(),
                self.mcp_v06.boot.clone(),
                self.mcp_v06.mac(text),
            );
            let part = i as i64;
            let (state,message,oldboot)=self.store.call(true,move|c|{
                let tx=c.transaction()?;
                let old:Option<(String,Option<String>,String)>=tx.query_row("SELECT state,message_id,boot_id FROM mcp_v06_parts WHERE view_id=?1 AND page_index=?2 AND part_index=?3",params![vid,n,part],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                if let Some(old)=old{return Ok(old);}
                tx.execute("INSERT INTO mcp_v06_parts VALUES(?1,?2,?3,NULL,?4,'SENDING',?5,?6,?7)",params![vid,n,part,ch,domain::id(),mac,boot])?;
                tx.commit()?;Ok(("NEW".to_owned(),None,boot))
            }).await?;
            let mid = if state == "NEW" {
                self.authorized_thread(thread).await?;
                let components = if webhook.is_none() && i + 1 == chunks.len() {
                    controls(&rec.id, rec.revision, view, Some(p), None)
                } else {
                    json!([])
                };
                let body = json!({"content":text,"allowed_mentions":{"parse":[]},"components":components,"flags":if webhook.is_some(){64}else{0}});
                let (method, path) = if let Some((app, token)) = webhook {
                    if i == 0 {
                        (
                            Method::PATCH,
                            format!("/webhooks/{app}/{token}/messages/@original"),
                        )
                    } else {
                        (Method::POST, format!("/webhooks/{app}/{token}?wait=true"))
                    }
                } else {
                    (Method::POST, format!("/channels/{thread}/messages"))
                };
                let sent = self.discord.api(method, &path, Some(body)).await;
                let value = sent?;
                let id = value["id"]
                    .as_str()
                    .context("message receipt missing")?
                    .to_owned();
                crate::discord::snowflake(&id)?;
                ensure!(value["content"] == *text, "message content mismatch");
                id
            } else {
                ensure!(
                    state == "CONFIRMED",
                    "previous display POST is uncertain; do not repost"
                );
                let mid = message.context("message missing")?;
                if webhook.is_some() {
                    ensure!(
                        oldboot == self.mcp_v06.boot,
                        "private view needs explicit redisplay"
                    );
                } else {
                    let got = self
                        .discord
                        .api(
                            Method::GET,
                            &format!("/channels/{thread}/messages/{mid}"),
                            None,
                        )
                        .await?;
                    ensure!(
                        self.discord.owns_message(&got) && got["content"] == *text,
                        "display changed or deleted"
                    );
                }
                mid
            };
            let (vid, ch, boot, mac, m) = (
                view.to_owned(),
                thread.to_owned(),
                self.mcp_v06.boot.clone(),
                self.mcp_v06.mac(text),
                mid.clone(),
            );
            self.store.call(true,move|c|{c.execute("UPDATE mcp_v06_parts SET state='CONFIRMED',message_id=?4,render_mac=?5,boot_id=?6 WHERE view_id=?1 AND page_index=?2 AND part_index=?3 AND channel_id=?7",params![vid,n,part,m,mac,boot,ch])?;Ok(())}).await?;
            ids.push(mid);
        }
        loaded.receipt.confirm(p, &ids)?;
        loaded.next = loaded.next.max(n as u64 + 1);
        let (vid, boot, complete) = (
            view.to_owned(),
            self.mcp_v06.boot.clone(),
            loaded.next == p.value()["page"]["count"].as_u64().unwrap_or(0),
        );
        self.store
            .call(true, move |c| {
                let tx = c.transaction()?;
                tx.execute(
                    "UPDATE mcp_v06_pages SET state='CONFIRMED' WHERE view_id=?1 AND page_index=?2",
                    params![vid, n],
                )?;
                tx.execute(
                    "UPDATE mcp_v06_views SET state=?2,boot_id=?3 WHERE id=?1 AND active=1",
                    params![vid, if complete { "READY" } else { "DELIVERING" }, boot],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(())
    }
    pub async fn handle_v06_mcp(&self, v: &Value) -> Result<()> {
        let id = v["id"].as_str().context("Discord interaction ID")?;
        let token = v["token"].as_str().context("interaction token")?;
        let app = v["application_id"].as_str().context("application ID")?;
        let custom = v["data"]["custom_id"].as_str().context("button ID")?;
        let parts: Vec<_> = custom.split(':').collect();
        ensure!(parts.len() == 5 && parts[0] == "ma6", "invalid v06 button");
        let action = parts[1];
        let viewing = action == "private" || action.starts_with("page");
        if viewing {
            self.discord.acknowledge(id, token).await?;
        } else {
            self.discord.acknowledge_update(id, token).await?;
        }
        let result = tokio::time::timeout(
            Duration::from_secs(40),
            self.v06_action(v, parts[2], parts[3].parse()?, parts[4], action),
        )
        .await;
        if !matches!(result, Ok(Ok(()))) {
            let catalog_wait = matches!(&result, Ok(Err(e)) if e.downcast_ref::<crate::proxy_v2::ApiError>().is_some_and(|e| matches!(e.code.as_str(),"catalog_loading"|"catalog_failed")));
            tracing::warn!(
                event = "mcp_v06_action_failed",
                action,
                category = if catalog_wait {
                    "catalog_wait"
                } else if result.is_err() {
                    "timeout"
                } else {
                    "validation_or_delivery"
                }
            );
            let message = if viewing {
                "操作詳細の取得または表示を確認できませんでした。このボタン操作では許可は送っていません。元の会話の「自分だけに表示して確認」をもう一度押してください。不要なら「拒否」、作業全体を止めるなら /stop を使えます。"
            } else if catalog_wait {
                "操作情報の更新を確認できなかったため、許可は送っていません。元の会話の「再確認」で内容を読み直し、更新された画面で許可を選んでください。不要なら「拒否」、作業全体を止めるなら /stop を使えます。"
            } else {
                "この操作を完了確認できませんでした。元の確認画面を開き直してください。許可は自動再送しません。拒否または /stop は引き続き利用できます。"
            };
            if viewing {
                self.discord
                    .reply_components(app, token, message, json!([]))
                    .await?;
            } else {
                self.discord.followup_error(app, token, message).await?;
            }
        }
        Ok(())
    }
    async fn v06_fetch_page(
        &self,
        rec: &Record,
        requester: bool,
        index: u64,
        pid: Option<&str>,
    ) -> Result<Presentation> {
        let s = self.settings().await;
        let mut p = s
            .proxy
            .approval_v06_page(&rec.remote, requester, index, pid)
            .await?;
        for attempt in 0..3 {
            self.verify_v06_page(rec, &p).await?;
            if !matches!(
                p.value()["reason"].as_str(),
                Some("catalog_loading" | "catalog_failed")
            ) {
                break;
            }
            if attempt == 2 {
                return Err(crate::proxy_v2::ApiError {
                    status: 409,
                    code: p.value()["reason"].as_str().unwrap().into(),
                    retry: "refetch".into(),
                }
                .into());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            p = s
                .proxy
                .approval_v06_page(&rec.remote, requester, index, pid)
                .await?;
        }
        self.verify_v06_page(rec, &p).await?;
        Ok(p)
    }
    async fn v06_action(
        &self,
        event: &Value,
        local: &str,
        revision: i64,
        view: &str,
        action: &str,
    ) -> Result<()> {
        let s = self.settings().await;
        let thread = event["channel_id"].as_str().context("channel")?;
        let user = event["member"]["user"]["id"]
            .as_str()
            .or(event["user"]["id"].as_str());
        ensure!(
            event["guild_id"] == s.cfg.discord.guild_id
                && user == Some(s.cfg.discord.allowed_user_id.as_str()),
            "unauthorized actor"
        );
        self.authorized_thread(thread).await?;
        let rec = self.mcp_record(local).await?;
        let req = self.store.request(&rec.request).await?;
        ensure!(
            req.thread_id == thread
                && rec.revision == revision
                && !req.state.terminal()
                && !req.stop_requested
                && rec.expires > domain::now_ms(),
            "stale approval"
        );
        ensure!(
            self.store.v06_selection(&rec.request).await?.is_some(),
            "not a v06 execution"
        );
        ensure!(
            rec.key.is_none(),
            "decision already sent; query its outcome"
        );
        let current = self.mcp_current(&rec).await?;
        ensure!(
            current["state"] == "pending" && current["revision"] == revision,
            "interaction changed"
        );
        // Decline is deliberately independent of argument/schema/presentation retrieval.
        if action == "decline" {
            return self
                .v06_decide(
                    &rec,
                    event,
                    None,
                    action,
                    mcp_v06::decline_body(revision as u64)?,
                )
                .await;
        }
        if action == "retry" {
            self.v06_poll_window(&rec, true).await?;
            self.mcp_v06.polls.lock().await.remove(local);
            return self.render_v06_mcp(&rec, thread).await;
        }
        let _slot = self.mcp_v06.display_slots.acquire().await?;
        let app = event["application_id"].as_str().context("app")?;
        let token = event["token"].as_str().context("token")?;
        if action == "private" || action.starts_with("page") {
            let (index, pid) = if action == "private" {
                (0, None)
            } else {
                let index = action
                    .strip_prefix("page")
                    .context("page action")?
                    .parse::<u64>()?;
                let cell = self
                    .mcp_v06
                    .views
                    .lock()
                    .await
                    .get(view)
                    .cloned()
                    .context("表示期限が切れました。元の会話からもう一度開いてください")?;
                let loaded = cell.lock().await;
                ensure!(
                    loaded.requester
                        && loaded.next == index
                        && loaded.expires > domain::now_ms()
                        && event["message"]["id"].as_str() == loaded.control_message.as_deref(),
                    "stale page"
                );
                (index, Some(loaded.presentation.clone()))
            };
            let p = self
                .v06_fetch_page(&rec, true, index, pid.as_deref())
                .await?;
            self.verify_v06_page(&rec, &p).await?;
            ensure!(p.value()["state"] == "ready", "private display unavailable");
            let view = if index == 0 {
                self.v06_view(&rec, &p, true).await?
            } else {
                view.into()
            };
            self.v06_display_page(&view, &rec, thread, &p, Some((app, token)))
                .await?;
            let next = if index + 1 < p.value()["page"]["count"].as_u64().context("count")? {
                Some(index + 1)
            } else {
                None
            };
            // Discord uses the first followup after a deferred response as
            // @original. Keep that operation detail intact when adding controls.
            let body = json!({"components":controls(local,revision,&view,Some(&p),next),"allowed_mentions":{"parse":[]}});
            let sent = self
                .discord
                .api(
                    Method::PATCH,
                    &format!("/webhooks/{app}/{token}/messages/@original"),
                    Some(body),
                )
                .await?;
            let mid = sent["id"]
                .as_str()
                .context("private control receipt missing")?
                .to_owned();
            crate::discord::snowflake(&mid)?;
            let cell = self
                .mcp_v06
                .views
                .lock()
                .await
                .get(&view)
                .cloned()
                .context("view disappeared")?;
            cell.lock().await.control_message = Some(mid);
            return Ok(());
        }
        ensure!(matches!(action, "once" | "turn"), "unknown decision");
        let cell = self
            .mcp_v06
            .views
            .lock()
            .await
            .get(view)
            .cloned()
            .context("display requires refresh")?;
        let (requester, pid) = {
            let loaded = cell.lock().await;
            ensure!(loaded.expires > domain::now_ms(), "expired display");
            if loaded.requester {
                ensure!(
                    event["message"]["id"].as_str() == loaded.control_message.as_deref(),
                    "wrong private approval card"
                );
            }
            (loaded.requester, loaded.presentation.clone())
        };
        let p = self.v06_fetch_page(&rec, requester, 0, Some(&pid)).await?;
        if !requester {
            let (local, mid, custom) = (
                rec.id.clone(),
                event["message"]["id"]
                    .as_str()
                    .context("card ID")?
                    .to_owned(),
                event["data"]["custom_id"].clone(),
            );
            let found=self.store.call(false,move|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM mcp_v06_parts p JOIN mcp_v06_views v ON v.id=p.view_id WHERE v.interaction_local_id=?1 AND v.active=1 AND p.state='CONFIRMED' AND p.message_id=?2)",params![local,mid],|r|r.get::<_,bool>(0))?)).await?;
            ensure!(found, "wrong public approval card");
            let got = self
                .discord
                .api(
                    Method::GET,
                    &format!(
                        "/channels/{thread}/messages/{}",
                        event["message"]["id"].as_str().unwrap()
                    ),
                    None,
                )
                .await?;
            ensure!(
                self.discord.owns_message(&got)
                    && got["components"]
                        .as_array()
                        .is_some_and(|rows| rows.iter().any(|r| r["components"]
                            .as_array()
                            .is_some_and(|bs| bs.iter().any(|b| b["custom_id"] == custom)))),
                "approval controls changed"
            );
            self.v06_display_page(view, &rec, thread, &p, None).await?;
        }
        let body = cell
            .lock()
            .await
            .receipt
            .permit_body(&p, action == "turn", domain::now_ms())?;
        self.v06_decide(&rec, event, Some(view), action, body).await
    }
    async fn v06_decide(
        &self,
        rec: &Record,
        event: &Value,
        view: Option<&str>,
        action: &str,
        body: Value,
    ) -> Result<()> {
        let key = format!("mcp-{}", domain::id());
        let (r, k, v, a, event_id, encoded, boot) = (
            rec.clone(),
            key.clone(),
            view.map(str::to_owned),
            action.to_owned(),
            event["id"].as_str().context("event ID")?.to_owned(),
            serde_json::to_string(&body)?,
            self.mcp_v06.boot.clone(),
        );
        self.store.call(true,move|c|{
            let tx=c.transaction()?;
            let (state,stop):(String,bool)=tx.query_row("SELECT state,stop_requested FROM requests WHERE id=?1",[&r.request],|r|Ok((r.get(0)?,r.get(1)?)))?;
            ensure!((matches!(state.as_str(),"RUNNING"|"APPROVAL_REQUIRED") || (a=="decline" && matches!(state.as_str(),"SENDING"|"UNKNOWN")))&&!stop,"execution no longer accepts approvals");
            let binding:(String,String,String,bool)=tx.query_row("SELECT instance_id,generation,base_url,blocked FROM proxy_binding",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            ensure!(binding==(r.binding.instance_id,r.binding.generation,r.binding.base_url,false),"generation changed");
            if let Some(view)=&v {let ready:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM mcp_v06_views WHERE id=?1 AND interaction_local_id=?2 AND active=1 AND state='READY' AND expires_at>?3 AND boot_id=?4)",params![view,r.id,domain::now_ms(),boot],|r|r.get(0))?;ensure!(ready,"view is stale");}
            let proxy_action=if a=="decline"{"decline"}else{"accept"};
            ensure!(tx.execute("UPDATE mcp_interactions SET operation_key=?2,operation_state='SENDING',action=?3,grant_scope=?4 WHERE id=?1 AND operation_key IS NULL AND closed=0 AND state='pending' AND revision=?5 AND expires_at>?6",params![r.id,k,proxy_action,if a=="turn"{Some("turn_tool")}else{None},r.revision,domain::now_ms()])?==1,"decision already made");
            tx.execute("INSERT INTO mcp_v06_decisions(id,interaction_local_id,view_id,discord_interaction_id,action,operation_key,expected_revision,reply_json,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'SENDING')",params![domain::id(),r.id,v,event_id,a,k,r.revision,encoded])?;
            tx.commit()?;Ok(())
        }).await?;
        let s = self.settings().await;
        ensure!(
            s.proxy.v2.binding.read().unwrap().as_ref() == Some(&rec.binding),
            "generation changed after decision commit"
        );
        let path = format!(
            "/v2/codex/interactions/{}/reply",
            crate::proxy::path_id(&rec.remote)?
        );
        let result = s
            .proxy
            .approval_v06_json(Method::POST, &path, Some(&key), Some(&body))
            .await;
        let rejected = result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<crate::proxy_v2::ApiError>())
            .filter(|e| {
                matches!(e.status, 409 | 422)
                    && matches!(
                        e.code.as_str(),
                        "catalog_loading"
                            | "catalog_failed"
                            | "presentation_stale"
                            | "presentation_incomplete"
                            | "presentation_expired"
                            | "presentation_audience_mismatch"
                            | "approval_policy_binding_mismatch"
                            | "approval_policy_not_ready"
                            | "approval_policy_denied"
                            | "approval_policy_check_unavailable"
                            | "turn_grant_ineligible"
                    )
            })
            .map(|e| e.code.clone());
        if let Some(code) = rejected {
            let local = rec.id.clone();
            let key = key.clone();
            self.store.call(true,move|c|{let tx=c.transaction()?;
                tx.execute("UPDATE mcp_v06_decisions SET state='REJECTED',active=0,error_code=?2 WHERE interaction_local_id=?1 AND operation_key=?3",params![local,code,key])?;
                tx.execute("UPDATE mcp_interactions SET operation_key=NULL,operation_state=NULL,action=NULL,grant_scope=NULL WHERE id=?1 AND operation_key=?2",params![local,key])?;
                tx.execute("UPDATE mcp_v06_views SET state='STALE',active=0 WHERE interaction_local_id=?1",[local])?;
                tx.commit()?;Ok(())}).await?;
            // No automatic retry. A fresh user decision (including decline) remains possible.
            return Err(result.err().context("rejected reply missing error")?);
        }
        let state = match &result {
            Ok(v)
                if v["resource"]["type"] == "interaction" && v["resource"]["id"] == rec.remote =>
            {
                v["state"].as_str().unwrap_or("unknown")
            }
            _ => "unknown",
        }
        .to_owned();
        let local = rec.id.clone();
        self.store.call(true,move|c|{let tx=c.transaction()?;tx.execute("UPDATE mcp_interactions SET operation_state=?2 WHERE id=?1",params![local,state])?;tx.execute("UPDATE mcp_v06_decisions SET state=?2 WHERE interaction_local_id=?1 AND active=1",params![local,if state=="succeeded"{"RESOLVED"}else if state=="accepted"||state=="running"{"ACCEPTED"}else{"UNKNOWN"}])?;tx.commit()?;Ok(())}).await?;
        let op = result?;
        ensure!(
            op["resource"]["type"] == "interaction" && op["resource"]["id"] == rec.remote,
            "reply receipt target mismatch"
        );
        Ok(())
    }
}

impl App {
    /// Close or compact public detail posts without persisting their contents.
    pub(crate) async fn close_v06_posts(
        &self,
        local: &str,
        thread: &str,
        remove: bool,
    ) -> Result<()> {
        let id = local.to_owned();
        let rows:Vec<(String,String,i64,i64)>=self.store.call(false,move|c|{
            let mut q=c.prepare("SELECT p.message_id,p.view_id,p.page_index,p.part_index FROM mcp_v06_parts p JOIN mcp_v06_views v ON v.id=p.view_id WHERE v.interaction_local_id=?1 AND v.audience='source_conversation' AND p.state='CONFIRMED'")?;
            Ok(q.query_map([id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<rusqlite::Result<_>>()?)
        }).await?;
        for (mid, view, page, part) in rows {
            if remove {
                ensure!(
                    self.discord.remove_own_message(thread, &mid).await?,
                    "approval cleanup unconfirmed"
                );
                self.store.call(true,move|c|{c.execute("UPDATE mcp_v06_parts SET state='DELETED' WHERE view_id=?1 AND page_index=?2 AND part_index=?3",params![view,page,part])?;Ok(())}).await?;
            } else {
                let path = format!("/channels/{thread}/messages/{mid}");
                let got = match self.discord.api(Method::GET, &path, None).await {
                    Ok(got) => got,
                    Err(error)
                        if error
                            .downcast_ref::<crate::discord::HttpStatus>()
                            .is_some_and(|e| e.0 == 404) =>
                    {
                        self.store.call(true,move|c|{c.execute("UPDATE mcp_v06_parts SET state='DELETED' WHERE view_id=?1 AND page_index=?2 AND part_index=?3",params![view,page,part])?;Ok(())}).await?;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                ensure!(
                    self.discord.owns_message(&got),
                    "approval message ownership changed"
                );
                if got["components"].as_array().is_some_and(|x| !x.is_empty()) {
                    self.discord
                        .api(
                            Method::PATCH,
                            &path,
                            Some(json!({"components":[],"allowed_mentions":{"parse":[]}})),
                        )
                        .await?;
                }
            }
        }
        let id = local.to_owned();
        self.store.call(true,move|c|{c.execute("UPDATE mcp_v06_views SET state='CLOSED',active=0 WHERE interaction_local_id=?1",[id])?;Ok(())}).await?;
        Ok(())
    }
}

impl App {
    async fn retire_v06_posts(&self, local: &str, thread: &str, current: &str) -> Result<()> {
        let (id, keep) = (local.to_owned(), current.to_owned());
        let rows:Vec<(String,String,i64,i64)>=self.store.call(false,move|c|{
            let mut q=c.prepare("SELECT p.message_id,p.view_id,p.page_index,p.part_index FROM mcp_v06_parts p JOIN mcp_v06_views v ON v.id=p.view_id WHERE v.interaction_local_id=?1 AND v.id!=?2 AND v.active=0 AND v.audience='source_conversation' AND p.state='CONFIRMED'")?;
            Ok(q.query_map(params![id,keep],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<rusqlite::Result<_>>()?)
        }).await?;
        for (mid, view, page, part) in rows {
            ensure!(
                self.discord.remove_own_message(thread, &mid).await?,
                "old approval cleanup unconfirmed"
            );
            self.store.call(true,move|c|{c.execute("UPDATE mcp_v06_parts SET state='DELETED' WHERE view_id=?1 AND page_index=?2 AND part_index=?3",params![view,page,part])?;Ok(())}).await?;
        }
        Ok(())
    }
}

impl App {
    async fn v06_poll_window(&self, rec: &Record, reset: bool) -> Result<bool> {
        let (id, rev, expiry, boot) = (
            rec.id.clone(),
            rec.revision,
            rec.expires,
            self.mcp_v06.boot.clone(),
        );
        self.store.call(true,move|c|{
            let tx=c.transaction()?;let now=domain::now_ms();
            let old:Option<(String,String,i64)>=tx.query_row("SELECT id,state,poll_deadline_ms FROM mcp_v06_views WHERE interaction_local_id=?1 AND audience='source_conversation' AND viewer_id='' AND active=1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((view,state,deadline))=old{
                if reset && state=="WAITING" {tx.execute("UPDATE mcp_v06_views SET poll_deadline_ms=?2,next_poll_ms=?3 WHERE id=?1",params![view,expiry.min(now+60_000),now])?;tx.commit()?;return Ok(true);}
                return Ok(state!="WAITING"||deadline>now);
            }
            tx.execute("INSERT INTO mcp_v06_views(id,interaction_local_id,audience,viewer_id,revision,state,boot_id,next_poll_ms,poll_deadline_ms) VALUES(?1,?2,'source_conversation','',?3,'WAITING',?4,?5,?6)",params![domain::id(),id,rev,boot,now,expiry.min(now+60_000)])?;
            tx.commit()?;Ok(true)
        }).await
    }
    pub(crate) async fn sweep_v06_views(&self) -> Result<()> {
        let boot = self.mcp_v06.boot.clone();
        self.store.call(true,move|c|{
            let tx=c.transaction()?;
            tx.execute("UPDATE mcp_v06_parts SET state='UNKNOWN' WHERE state='SENDING' AND boot_id!=?1",[&boot])?;
            tx.execute("UPDATE mcp_v06_decisions SET state='UNKNOWN' WHERE state='SENDING'",[])?;
            tx.execute("UPDATE mcp_v06_views SET active=0,state='STALE' WHERE active=1 AND (expires_at<=?1 OR (audience='requester' AND boot_id!=?2))",params![domain::now_ms(),boot])?;
            tx.commit()?;Ok(())
        }).await
    }
}

impl App {
    async fn mark_v06_wait(&self, rec: &Record) -> Result<()> {
        let (id, expiry) = (rec.id.clone(), rec.expires);
        self.store.call(true,move|c|{c.execute("UPDATE mcp_v06_views SET state='WAITING',poll_deadline_ms=?2 WHERE interaction_local_id=?1 AND audience='source_conversation' AND active=1 AND state!='WAITING'",params![id,expiry.min(domain::now_ms()+60_000)])?;Ok(())}).await
    }
}
