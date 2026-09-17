//! MCP interaction UI. Durable identities, volatile answers, no uncertain POST replay.
use crate::{
    application::App, delivery::chunks, domain, mcp_form, proxy::path_id, proxy_v2::Binding,
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::atomic::Ordering, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
#[derive(Clone)]
pub struct Draft {
    pub revision: i64,
    pub version: u64,
    pub expires: i64,
    pub answers: Map<String, Value>,
}
#[derive(Clone)]
pub(crate) struct Record {
    pub(crate) id: String,
    pub(crate) remote: String,
    pub(crate) request: String,
    pub(crate) response: String,
    pub(crate) conversation: String,
    pub(crate) workspace: String,
    pub(crate) binding: Binding,
    pub(crate) revision: i64,
    pub(crate) digest: String,
    pub(crate) expires: i64,
    pub(crate) key: Option<String>,
    pub(crate) action: Option<String>,
}
const SELECT: &str = "SELECT id,interaction_id,request_id,response_id,conversation_id,workspace_id,instance_id,generation,base_url,revision,request_digest,expires_at,operation_key,action FROM mcp_interactions WHERE id=?1";
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    Ok(Record {
        id: r.get(0)?,
        remote: r.get(1)?,
        request: r.get(2)?,
        response: r.get(3)?,
        conversation: r.get(4)?,
        workspace: r.get(5)?,
        binding: Binding {
            instance_id: r.get(6)?,
            generation: r.get(7)?,
            base_url: r.get(8)?,
        },
        revision: r.get(9)?,
        digest: r.get(10)?,
        expires: r.get(11)?,
        key: r.get(12)?,
        action: r.get(13)?,
    })
}
fn timestamp(v: &Value) -> Result<i64> {
    Ok(
        (OffsetDateTime::parse(v.as_str().context("期限がありません")?, &Rfc3339)?
            .unix_timestamp_nanos()
            / 1_000_000) as i64,
    )
}
fn button(id: &str, label: &str, style: u8) -> Value {
    json!({"type":2,"style":style,"label":label,"custom_id":id})
}
fn clean(s: &str) -> String {
    s.replace('@', "＠")
        .replace('`', "｀")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
fn short(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
/// Existing contract supplies confirmation prose, not verified tool-call arguments.
/// Never extract a tool identity or infer read-only permission from this prose.
pub fn confirmation_description(form: &Value) -> Result<String> {
    let server = form["serverName"].as_str().context("server missing")?;
    let message = form["message"].as_str().context("message missing")?;
    let is_tool = form["_meta"]["codex_approval_kind"] == "mcp_tool_call";
    let mut text = format!(
        "{}\nサーバー: {}\n\nMCPからの確認（原文）：\n{}",
        if is_tool {
            "MCPツールの実行許可が必要です"
        } else {
            "MCPから入力・確認が届きました"
        },
        clean(server),
        clean(message)
    );
    if is_tool {
        text.push_str("\n\nこの確認原文とは別に、実際の引数・実行コードを照合できる情報は取得できていません。ツール名が同じでも操作内容は変わります。内容が分からない場合は「拒否」を選び、作業全体を止める場合は /stop を使ってください。");
    }
    for key in ["title", "description"] {
        if let Some(value) = form["requestedSchema"].get(key) {
            text.push('\n');
            text.push_str(&clean(&mcp_form::display(value)));
        }
    }
    Ok(text)
}

impl App {
    pub(crate) async fn mcp_record(&self, id: &str) -> Result<Record> {
        let id = id.to_owned();
        self.store
            .call(false, move |c| Ok(c.query_row(SELECT, [id], row)?))
            .await
    }
    pub(crate) async fn mcp_current(&self, r: &Record) -> Result<Value> {
        let s = self.settings().await;
        ensure!(
            !self.recovery.load(Ordering::SeqCst) && !self.reloading.load(Ordering::SeqCst),
            "復旧確認中です"
        );
        ensure!(
            s.proxy.v2.binding.read().unwrap().as_ref() == Some(&r.binding),
            "Proxyの世代が変わりました"
        );
        let v = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/interactions/{}", path_id(&r.remote)?),
                None,
                None,
            )
            .await?;
        ensure!(
            v["interaction_id"] == r.remote
                && v["response_id"] == r.response
                && v["conversation_id"] == r.conversation
                && v["workspace_id"] == r.workspace,
            "対話の対象が一致しません"
        );
        ensure!(
            v["kind"] == "mcp_form" && v["revision"].as_i64().is_some_and(|n| n >= r.revision),
            "対話の種類またはrevisionが不正です"
        );
        Ok(v)
    }
    pub(crate) async fn mcp_pending(
        &self,
        r: &Record,
        thread: &str,
        revision: i64,
    ) -> Result<Value> {
        self.authorized_thread(thread).await?;
        let req = self.store.request(&r.request).await?;
        ensure!(
            req.thread_id == thread
                && !req.state.terminal()
                && !req.stop_requested
                && req.state != crate::domain::RequestState::CancelRequested,
            "この作業は回答できません"
        );
        ensure!(
            r.key.is_none() && r.revision == revision && r.expires > domain::now_ms(),
            "回答済み、期限切れ、または古い画面です"
        );
        let v = self.mcp_current(r).await?;
        ensure!(
            v["state"] == "pending"
                && v["revision"] == revision
                && timestamp(&v["expires_at"])? == r.expires,
            "承認画面が失効しました"
        );
        ensure!(
            domain::digest(&serde_json::to_vec(&v["request"])?) == r.digest,
            "承認内容が変わりました"
        );
        mcp_form::validate_schema(&v["request"]["requestedSchema"])?;
        Ok(v)
    }
    pub async fn mcp_interaction_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut retry_at = HashMap::<String, std::time::Instant>::new();
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{}}
            self.mcp_drafts
                .lock()
                .await
                .retain(|_, d| d.expires > domain::now_ms());
            if self.connected.load(Ordering::SeqCst) && self.expire_mcp_ui().await.is_err() {
                tracing::warn!(event = "mcp_expiry_ui_failed");
            }
            if !self.connected.load(Ordering::SeqCst) || self.recovery.load(Ordering::SeqCst) {
                continue;
            }
            if !self
                .settings()
                .await
                .proxy
                .v2
                .mcp_form
                .load(Ordering::SeqCst)
            {
                continue;
            }
            let ids=self.store.call(false,|c|{let mut q=c.prepare("SELECT id FROM requests WHERE response_id IS NOT NULL AND (dispatch_started_at >= (SELECT applied_at FROM schema_migrations WHERE version=6) OR EXISTS(SELECT 1 FROM mcp_interactions m WHERE m.request_id=requests.id)) AND (state NOT IN ('COMPLETED','FAILED','CANCELLED') OR interaction_scan_done=0) ORDER BY CASE WHEN state IN ('COMPLETED','FAILED','CANCELLED') THEN 1 ELSE 0 END,updated_at DESC LIMIT 20")?;Ok(q.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
            use futures_util::{StreamExt, stream};
            let now = std::time::Instant::now();
            let ids: Vec<_> = ids
                .into_iter()
                .filter(|id| retry_at.get(id).is_none_or(|at| *at <= now))
                .collect();
            let results = stream::iter(ids)
                .map(|id| async move {
                    let ok = self.cancel.is_cancelled() || self.scan_mcp(&id).await.is_ok();
                    (id, ok)
                })
                .buffer_unordered(4)
                .collect::<Vec<_>>()
                .await;
            for (id, ok) in results {
                if ok {
                    retry_at.remove(&id);
                } else {
                    tracing::warn!(event="mcp_interaction_poll_failed",request_id=%id);
                    retry_at.insert(id, std::time::Instant::now() + Duration::from_secs(30));
                }
            }
        }
    }
    pub async fn expire_mcp_ui(&self) -> Result<()> {
        self.sweep_v06_views().await?;
        let ids = self
            .store
            .call(false, |c| {
                let mut q = c.prepare("SELECT id FROM mcp_interactions WHERE closed=0")?;
                Ok(q.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        let s = self.settings().await;
        let blocked = self
            .store
            .call(false, |c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM proxy_binding WHERE blocked=1)",
                    [],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .await?;
        let binding = s.proxy.v2.binding.read().unwrap().clone();
        for id in ids {
            let rec = self.mcp_record(&id).await?;
            let r = self.store.request(&rec.request).await?;
            if r.state.terminal()
                || rec.expires <= domain::now_ms()
                || blocked
                || binding.as_ref() != Some(&rec.binding)
                || self.recovery.load(Ordering::SeqCst)
            {
                if self.authorized_thread(&r.thread_id).await.is_err() {
                    continue;
                }
                if !blocked
                    && binding.as_ref() == Some(&rec.binding)
                    && !self.recovery.load(Ordering::SeqCst)
                    && self.mcp_compaction_ready(&rec.id).await?
                {
                    self.compact_resolved_mcp(&rec, &r.thread_id).await?;
                    continue;
                }
                self.close_mcp(&rec,&r.thread_id,"この承認・入力は期限切れ、作業終了、または復旧確認中のため操作できません。送信済みの回答は自動再送しません。").await?;
            }
        }
        Ok(())
    }
    pub async fn scan_mcp(&self, id: &str) -> Result<()> {
        let _scan = self.mcp_scan_lock.lock().await;
        let s = self.settings().await;
        let r = self.store.request(id).await?;
        self.authorized_thread(&r.thread_id).await?;
        let binding = s
            .proxy
            .v2
            .binding
            .read()
            .unwrap()
            .clone()
            .context("Proxy binding missing")?;
        let t = r.thread_id.clone();
        let (cv,ws):(String,String)=self.store.call(false,move|c|Ok(c.query_row("SELECT conversation_id,workspace_id FROM proxy_conversations WHERE thread_id=?1",[t],|r|Ok((r.get(0)?,r.get(1)?)))?)).await?;
        let response = r.response_id.as_ref().context("response missing")?;
        let list = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/responses/{}/interactions", path_id(response)?),
                None,
                None,
            )
            .await?;
        ensure!(
            list["response_id"] == *response,
            "interaction response mismatch"
        );
        let modern = self.store.v06_selection(id).await?.is_some();
        let data = list["data"]
            .as_array()
            .context("interaction list invalid")?;
        ensure!(
            data.len()
                <= if modern || s.proxy.v2.mcp_caps.read().unwrap().turn {
                    256
                } else {
                    16
                },
            "interaction list oversized"
        );
        for v in data {
            ensure!(
                v["response_id"] == *response
                    && v["conversation_id"] == cv
                    && v["workspace_id"] == ws,
                "interaction binding mismatch"
            );
            if v["kind"] != "mcp_form" {
                continue;
            }
            let remote = v["interaction_id"]
                .as_str()
                .context("interaction identity missing")?
                .to_owned();
            path_id(&remote)?;
            let rev = v["revision"]
                .as_i64()
                .filter(|x| *x > 0)
                .context("revision invalid")?;
            let expiry = timestamp(&v["expires_at"])?;
            let state = v["state"]
                .as_str()
                .context("interaction state missing")?
                .to_owned();
            let digest = domain::digest(&serde_json::to_vec(&v["request"])?);
            let (b, rid, resp, cv, ws) = (
                binding.clone(),
                r.id.clone(),
                response.clone(),
                cv.clone(),
                ws.clone(),
            );
            let local=self.store.call(true,move|c|{let tx=c.transaction()?;let old:Option<String>=tx.query_row("SELECT id FROM mcp_interactions WHERE instance_id=?1 AND generation=?2 AND interaction_id=?3",params![b.instance_id,b.generation,remote],|r|r.get(0)).optional()?;
        let id=old.unwrap_or_else(domain::id);
        tx.execute("INSERT OR IGNORE INTO mcp_interactions(id,interaction_id,request_id,response_id,conversation_id,workspace_id,instance_id,generation,base_url,revision,request_digest,expires_at,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![id,remote,rid,resp,cv,ws,b.instance_id,b.generation,b.base_url,rev,digest,expiry,state])?;
        let existing=tx.query_row(SELECT,[&id],row)?;ensure!(existing.request==rid&&existing.response==resp&&existing.binding==b,"interaction identity reused");
        if modern && state=="pending" && rev>existing.revision && existing.key.is_none() {
            tx.execute("UPDATE mcp_interactions SET revision=?2,request_digest=?3,expires_at=?4,closed=0 WHERE id=?1",params![id,rev,digest,expiry])?;
            tx.execute("UPDATE mcp_v06_views SET state='STALE',active=0 WHERE interaction_local_id=?1",[&id])?;
        }
        tx.commit()?;Ok(id)
      }).await?;
            self.render_mcp(&local, v).await?;
        }
        // Include records that disappeared from the list or belong to a previous generation.
        let rid = r.id.clone();
        let locals = self
            .store
            .call(false, move |c| {
                let mut q =
                    c.prepare("SELECT id FROM mcp_interactions WHERE request_id=?1 AND closed=0")?;
                Ok(q.query_map([rid], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        for id in locals {
            let rec = self.mcp_record(&id).await?;
            if rec.binding != binding {
                self.close_mcp(&rec,&r.thread_id,"Proxyの世代が変わったため、この承認は操作できません。再送せず照合を待っています。").await?;
                continue;
            }
            if !data.iter().any(|v| v["interaction_id"] == rec.remote) {
                if let Ok(v) = self.mcp_current(&rec).await {
                    self.render_mcp(&id, &v).await?;
                } else if r.state.terminal() || rec.expires <= domain::now_ms() {
                    self.close_mcp(
                        &rec,
                        &r.thread_id,
                        "承認の状態を確認できません。期限切れまたは作業終了のため操作できません。",
                    )
                    .await?;
                }
            }
        }
        if r.state.terminal() {
            let rid = r.id;
            self.store.call(true,move|c|{c.execute("UPDATE requests SET interaction_scan_done=1 WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM mcp_interactions WHERE request_id=?1 AND closed=0)",[rid])?;Ok(())}).await?;
        }
        Ok(())
    }
    async fn close_mcp(&self, r: &Record, thread: &str, text: &str) -> Result<()> {
        self.close_v06_posts(&r.id, thread, false).await?;
        if self
            .delivery
            .text(&r.id, thread, "mcp_action", 0, text, json!([]))
            .await?
        {
            let id = r.id.clone();
            self.store
                .call(true, move |c| {
                    c.execute("UPDATE mcp_interactions SET closed=1 WHERE id=?1", [id])?;
                    Ok(())
                })
                .await?;
            self.mcp_drafts.lock().await.remove(&r.id);
        }
        Ok(())
    }
    async fn mcp_compaction_ready(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store.call(false, move |c| Ok(c.query_row(
            "SELECT coalesce(state='resolved' AND action='accept' AND operation_state='succeeded',0) FROM mcp_interactions WHERE id=?1",
            [id], |r| r.get(0))?)).await
    }

    async fn compact_resolved_mcp(&self, rec: &Record, thread: &str) -> Result<()> {
        self.close_v06_posts(&rec.id, thread, true).await?;
        // A previously closed UNKNOWN card may now have a verified resolution.
        // Keep recovery polling until the entire UI cleanup is confirmed.
        let local_id = rec.id.clone();
        self.store
            .call(true, move |c| {
                c.execute(
                    "UPDATE mcp_interactions SET closed=0 WHERE id=?1",
                    [local_id],
                )?;
                Ok(())
            })
            .await?;
        // A restart may find partially deleted cards; never recreate those cards.
        let local_id = rec.id.clone();
        let deleting: bool = self.store.call(false, move |c| Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM deliveries WHERE target_id=?1 AND kind IN ('mcp_description','mcp_action') AND state IN ('DELETE_PENDING','DELETED'))",
            [local_id], |r| r.get(0))?)).await?;
        // First remove all actionable controls. An uncertain PATCH blocks cleanup.
        if !deleting
            && !self
                .delivery
                .text(
                    &rec.id,
                    thread,
                    "mcp_action",
                    0,
                    "この確認への回答を送りました。作業結果は続く回答で確認してください。",
                    json!([]),
                )
                .await?
        {
            return Ok(());
        }
        let request = rec.request.clone();
        let count: i64 = self.store.call(false, move |c| Ok(c.query_row(
            "SELECT count(*) FROM mcp_interactions WHERE request_id=?1 AND state='resolved' AND action='accept' AND operation_state='succeeded'",
            [request], |r| r.get(0))?)).await?;
        let summary = format!(
            "この作業のMCP確認への回答：{count}件を送信済み。\nこれは確認への回答記録です。ツールの実行結果は続く回答で確認してください。"
        );
        if !self
            .delivery
            .text(&rec.request, thread, "mcp_summary", 0, &summary, json!([]))
            .await?
        {
            return Ok(());
        }
        if !self.delivery.clear_mcp_cards(&rec.id, thread).await? {
            return Ok(());
        }
        let id = rec.id.clone();
        self.store
            .call(true, move |c| {
                c.execute("UPDATE mcp_interactions SET closed=1 WHERE id=?1", [id])?;
                Ok(())
            })
            .await?;
        self.mcp_drafts.lock().await.remove(&rec.id);
        Ok(())
    }

    async fn render_mcp(&self, id: &str, v: &Value) -> Result<()> {
        let rec = self.mcp_record(id).await?;
        let local_id = id.to_owned();
        let closed: bool = self
            .store
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT closed AND EXISTS(SELECT 1 FROM deliveries WHERE target_id=mcp_interactions.id AND kind='mcp_action' AND state='DELETED') FROM mcp_interactions WHERE id=?1",
                    [local_id],
                    |r| r.get(0),
                )?)
            })
            .await?;
        if closed {
            return Ok(());
        }
        let req = self.store.request(&rec.request).await?;
        let s = self.settings().await;
        ensure!(
            s.proxy.v2.binding.read().unwrap().as_ref() == Some(&rec.binding),
            "interaction generation changed"
        );
        let state = v["state"].as_str().context("state missing")?;
        let r = rec.clone();
        let st = state.to_owned();
        self.store
            .call(true, move |c| {
                c.execute(
                    "UPDATE mcp_interactions SET state=?2 WHERE id=?1",
                    params![r.id, st],
                )?;
                Ok(())
            })
            .await?;
        if let Some(key) = &rec.key
            && let Ok(op) = s.proxy.operation(key).await
        {
            ensure!(
                op["resource"]["type"] == "interaction" && op["resource"]["id"] == rec.remote,
                "reply operation target mismatch"
            );
            let (id, st) = (
                id.to_owned(),
                op["state"].as_str().unwrap_or("unknown").to_owned(),
            );
            self.store
                .call(true, move |c| {
                    c.execute(
                        "UPDATE mcp_interactions SET operation_state=?2 WHERE id=?1",
                        params![id, st],
                    )?;
                    c.execute("UPDATE mcp_v06_decisions SET state=?2 WHERE interaction_local_id=?1 AND active=1",params![id,match st.as_str(){"succeeded"=>"RESOLVED","accepted"|"running"=>"ACCEPTED",_=>"UNKNOWN"}])?;
                    Ok(())
                })
                .await?;
        }
        if self.mcp_compaction_ready(id).await? {
            return self.compact_resolved_mcp(&rec, &req.thread_id).await;
        }
        if state != "pending"
            || req.state.terminal()
            || req.stop_requested
            || rec.expires <= domain::now_ms()
            || rec.key.is_some()
        {
            let text = match state {
                "expired" => "MCPの承認・入力期限が切れました。利用者による拒否ではありません。",
                "unknown" => "MCPへの回答の結果が不明です。自動再送はしません。",
                "submitted" => {
                    if rec.action.as_deref() == Some("decline") {
                        "拒否をMCPへ送りました。"
                    } else {
                        "回答をMCPへ送りました。作業結果を待っています。"
                    }
                }
                "resolved" => "このMCP承認・入力は終了しました。",
                "cancelled" => "このMCP承認・入力は操作できなくなりました。",
                _ if req.state.terminal() || req.stop_requested => {
                    "作業が終了または停止要求済みのため、このMCP承認・入力は操作できません。"
                }
                _ if rec.expires <= domain::now_ms() => {
                    "承認・入力の期限を過ぎました。状態を確認しています。"
                }
                _ => "MCPへの回答の送信状況を確認しています。自動再送はしません。",
            };
            if matches!(state, "resolved" | "expired" | "cancelled" | "unknown")
                || req.state.terminal()
            {
                return self.close_mcp(&rec, &req.thread_id, text).await;
            }
            self.delivery
                .text(id, &req.thread_id, "mcp_action", 0, text, json!([]))
                .await?;
            return Ok(());
        }
        ensure!(
            v["revision"] == rec.revision
                && domain::digest(&serde_json::to_vec(&v["request"])?) == rec.digest,
            "interaction changed without new UI"
        );
        let form = &v["request"];
        if form["_meta"]["codex_approval_kind"] == "mcp_tool_call"
            && self.store.v06_selection(&rec.request).await?.is_some()
        {
            return self.render_v06_mcp(&rec, &req.thread_id).await;
        }
        let valid = mcp_form::validate_schema(&form["requestedSchema"]);
        let detailed = s.proxy.v2.mcp_caps.read().unwrap().details
            && form["_meta"]["codex_approval_kind"] == "mcp_tool_call";
        if detailed && self.has_inline_run(&rec.request).await? {
            return self.render_inline_mcp(&rec, &req.thread_id).await;
        }
        let text = if detailed {
            "MCP操作の確認が必要です。操作内容は本人限定画面で確認してください。".to_owned()
        } else {
            self.redact(&s, &confirmation_description(form)?)
        };
        for (n, t) in chunks(&text).iter().enumerate() {
            if !self
                .delivery
                .text(
                    id,
                    &req.thread_id,
                    "mcp_description",
                    n as i64,
                    t,
                    json!([]),
                )
                .await?
            {
                return Ok(());
            }
        }
        if valid.is_err() {
            self.delivery
                .text(
                    id,
                    &req.thread_id,
                    "mcp_action",
                    0,
                    "この入力形式は処理できません。/stop で停止してください。自動で許可しません。",
                    json!([]),
                )
                .await?;
            return Ok(());
        }
        let empty = form["requestedSchema"]["properties"]
            .as_object()
            .unwrap()
            .is_empty();
        let prefix = format!("mcp:{id}:{}", rec.revision);
        let buttons = if detailed {
            let controls = vec![
                button(
                    &format!("mt:details:{id}:{}", rec.revision),
                    "操作内容を確認",
                    1,
                ),
                button(&format!("{prefix}:decline"), "拒否", 4),
            ];
            json!([{"type":1,"components":controls}])
        } else {
            json!([{"type":1,"components":[button(&format!("{prefix}:{}",if empty{"accept"}else{"form"}),if empty{"今回だけ許可"}else{"入力フォームを開く"},if empty{3}else{1}),button(&format!("{prefix}:decline"),"拒否",4)]}])
        };
        self.delivery
            .text(
                id,
                &req.thread_id,
                "mcp_action",
                0,
                "内容を確認してから選んでください。「今回だけ許可」はこの1件のみで、次の呼出しには引き継ぎません。",
                buttons,
            )
            .await?;
        Ok(())
    }
    pub async fn mcp_reply(
        &self,
        id: &str,
        thread: &str,
        revision: i64,
        action: &str,
        answers: Map<String, Value>,
    ) -> Result<()> {
        self.mcp_reply_scoped(id, thread, revision, action, answers, None)
            .await
    }
    pub(crate) async fn mcp_reply_scoped(
        &self,
        id: &str,
        thread: &str,
        revision: i64,
        action: &str,
        answers: Map<String, Value>,
        scope: Option<(String, bool)>,
    ) -> Result<()> {
        self.mcp_reply_with_view(id, thread, revision, action, answers, scope, None)
            .await
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn mcp_reply_with_view(
        &self,
        id: &str,
        thread: &str,
        revision: i64,
        action: &str,
        answers: Map<String, Value>,
        scope: Option<(String, bool)>,
        presentation: Option<crate::mcp_inline::Receipt>,
    ) -> Result<()> {
        ensure!(matches!(action, "accept" | "decline"), "invalid action");
        let rec = self.mcp_record(id).await?;
        let v = self.mcp_pending(&rec, thread, revision).await?;
        ensure!(
            !(self.store.v06_selection(&rec.request).await?.is_some()
                && v["request"]["_meta"]["codex_approval_kind"] == "mcp_tool_call"),
            "0.6の現在の確認画面から操作してください"
        );
        if action == "accept"
            && scope.is_none()
            && self
                .settings()
                .await
                .proxy
                .v2
                .mcp_caps
                .read()
                .unwrap()
                .details
            && v["request"]["_meta"]["codex_approval_kind"] == "mcp_tool_call"
        {
            anyhow::bail!("操作内容を確認する画面から許可してください");
        }
        if action == "accept" {
            mcp_form::validate_answers(&v["request"]["requestedSchema"], &answers)?;
        }
        let mut body = json!({"expected_revision":revision,"response":{"action":action,"content":if action=="accept"{Value::Object(answers)}else{Value::Null}}});
        let grant_scope = if scope.as_ref().is_some_and(|(_, turn)| *turn) {
            Some("turn_tool".to_owned())
        } else {
            None
        };
        if let Some((fp, turn)) = scope {
            body["expected_scope_fingerprint"] = json!(fp);
            if turn {
                body["grant_scope"] = json!("turn_tool");
            }
        }
        if let Some(token) = presentation.as_ref() {
            ensure!(action == "accept", "presentation only for acceptance");
            body["approval_view"] = json!("source_conversation");
            body["expected_presentation_fingerprint"] = json!(token.token);
        }
        ensure!(
            serde_json::to_vec(&body)?.len() <= 65536,
            "回答全体がProxyの容量上限を超えています"
        );
        let key = format!("mcp-{}", domain::id());
        let (r, k, hash, a) = (
            rec.clone(),
            key.clone(),
            domain::digest(&serde_json::to_vec(&body)?),
            action.to_owned(),
        );
        self.store.call(true,move|c|{let tx=c.transaction()?;
      let (state,stop):(String,bool)=tx.query_row("SELECT state,stop_requested FROM requests WHERE id=?1",[&r.request],|r|Ok((r.get(0)?,r.get(1)?)))?;
      ensure!(matches!(state.as_str(),"RUNNING"|"APPROVAL_REQUIRED")&&!stop,"停止または終了済みです");
      let current:(String,String,String,bool)=tx.query_row("SELECT instance_id,generation,base_url,blocked FROM proxy_binding",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
      ensure!(current==(r.binding.instance_id,r.binding.generation,r.binding.base_url,false),"Proxyの世代が変わりました");
      if let Some(receipt)=presentation.as_ref(){let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM mcp_inline_views v JOIN deliveries d ON d.target_id=v.interaction_local_id WHERE v.interaction_local_id=?1 AND v.fingerprint=?2 AND v.active=1 AND v.expires_at>?3 AND d.kind='mcp_action' AND d.part=0 AND d.state='CONFIRMED' AND d.message_id=?4 AND d.confirmed_digest=?5)",params![r.id,receipt.token,domain::now_ms(),receipt.message_id,receipt.card_digest],|r|r.get(0))?;ensure!(valid,"表示が失効しました");}
      ensure!(tx.execute("UPDATE mcp_interactions SET operation_key=?2,operation_state='SENDING',reply_digest=?3,action=?4,grant_scope=?7 WHERE id=?1 AND operation_key IS NULL AND closed=0 AND state='pending' AND revision=?5 AND expires_at>?6",params![r.id,k,hash,a,revision,domain::now_ms(),grant_scope])?==1,"回答済みまたは失効済みです");tx.commit()?;Ok(())
    }).await?;
        self.mcp_drafts.lock().await.remove(id);
        let s = self.settings().await;
        ensure!(
            s.proxy.v2.binding.read().unwrap().as_ref() == Some(&rec.binding),
            "Proxyの世代が変わりました。回答は再送しません"
        );
        let result = s
            .proxy
            .v2_json(
                Method::POST,
                &format!("/v2/codex/interactions/{}/reply", path_id(&rec.remote)?),
                Some(&key),
                Some(&body),
            )
            .await;
        let status = match &result {
            Ok(v)
                if v["resource"]["type"] == "interaction" && v["resource"]["id"] == rec.remote =>
            {
                v["state"].as_str().unwrap_or("unknown")
            }
            _ => "unknown",
        }
        .to_owned();
        let id = id.to_owned();
        self.store
            .call(true, move |c| {
                c.execute(
                    "UPDATE mcp_interactions SET operation_state=?2 WHERE id=?1",
                    params![id, status],
                )?;
                Ok(())
            })
            .await?;
        let op = result?;
        ensure!(
            op["resource"]["type"] == "interaction" && op["resource"]["id"] == rec.remote,
            "MCP回答の照合に失敗しました。再送はしません"
        );
        Ok(())
    }
    pub async fn handle_mcp(&self, v: &Value) -> Result<()> {
        let custom = v["data"]["custom_id"]
            .as_str()
            .context("MCP操作IDがありません")?;
        let parts: Vec<_> = custom.split(':').collect();
        ensure!(
            parts.len() >= 4 && parts[0] == "mcp",
            "invalid MCP operation"
        );
        let (id, revision, action) = (parts[1], parts[2].parse::<i64>()?, parts[3]);
        let (iid, token, app, thread) = (
            v["id"].as_str().context("id missing")?,
            v["token"].as_str().context("token missing")?,
            v["application_id"]
                .as_str()
                .context("application missing")?,
            v["channel_id"].as_str().context("channel missing")?,
        );
        let s = self.settings().await;
        ensure!(
            v["guild_id"] == s.cfg.discord.guild_id
                && v["member"]["user"]["id"] == s.cfg.discord.allowed_user_id,
            "操作権限がありません"
        );
        ensure!(
            v["type"] == if action == "save" { 5 } else { 3 },
            "invalid interaction type"
        );
        if action == "edit" {
            let result=tokio::time::timeout(Duration::from_secs(2),async{
        let rec=self.mcp_record(id).await?;let current=self.mcp_pending(&rec,thread,revision).await?;
        let idx:usize=parts.get(4).context("field missing")?.parse()?;
        let (name,schema)=current["request"]["requestedSchema"]["properties"].as_object().unwrap().iter().nth(idx).context("項目がありません")?;
        let drafts=self.mcp_drafts.lock().await;let draft=drafts.get(id).context("入力画面を開き直してください")?;
        ensure!(draft.revision==revision,"古い入力画面です");
        let long=schema["type"]=="string"&&schema.get("enum").is_none();
        let components:Vec<_>=(0..if long{3}else{1}).map(|n|json!({"type":1,"components":[{"type":4,"custom_id":format!("value{n}"),"label":if n==0{"入力値（選択肢は番号）"}else{"長文の続き（任意・改行は自動追加しません）"},"style":if long{2}else{1},"required":false,"max_length":if long{4000}else{100}}]})).collect();
        Ok::<_,anyhow::Error>(json!({"type":9,"data":{"custom_id":format!("mcp:{id}:{revision}:save:{idx}:{}",draft.version),"title":short(&clean(name),40),"components":components}}))
      }).await;
            let payload = match result {
                Ok(Ok(p)) => p,
                _ => {
                    json!({"type":4,"data":{"flags":64,"content":"この入力画面を開けませんでした。元のカードから開き直してください。","allowed_mentions":{"parse":[]}}})
                }
            };
            return self.discord.interaction_callback(iid, token, payload).await;
        }
        let silent = matches!(action, "accept" | "decline" | "submit");
        if silent {
            self.discord.acknowledge_update(iid, token).await?;
        } else {
            self.discord.acknowledge(iid, token).await?;
        }
        let result=async{
      let rec=self.mcp_record(id).await?;let current=self.mcp_pending(&rec,thread,revision).await?;let schema=&current["request"]["requestedSchema"];let props=schema["properties"].as_object().unwrap();
      if action=="accept"||action=="decline" {
        ensure!(action!="accept"||props.is_empty(),"入力フォームから送信してください");
        self.mcp_reply(id,thread,revision,action,Map::new()).await?;return Ok(());
      }
      {let mut drafts=self.mcp_drafts.lock().await;drafts.retain(|_,d|d.expires>domain::now_ms());ensure!(drafts.contains_key(id)||drafts.len()<32,"入力画面が多すぎます");
        ensure!(action=="form" || drafts.contains_key(id),"入力画面を開き直してください。再起動前の入力は保持していません");
        drafts.entry(id.into()).or_insert(Draft{revision,version:0,expires:rec.expires,answers:Map::new()});}
      if action=="save" {
        let idx:usize=parts.get(4).context("field missing")?.parse()?;let ver:u64=parts.get(5).context("version missing")?.parse()?;
        let (name,scalar)=props.iter().nth(idx).context("項目がありません")?;
        let mut values=HashMap::new();for row in v["data"]["components"].as_array().context("入力がありません")? {for item in row["components"].as_array().context("入力がありません")?{let k=item["custom_id"].as_str().context("入力IDがありません")?;ensure!(matches!(k,"value0"|"value1"|"value2")&&values.insert(k,item["value"].as_str().context("入力がありません")?).is_none(),"入力が重複しています");}}
        let text=format!("{}{}{}",values.get("value0").context("入力がありません")?,values.get("value1").unwrap_or(&""),values.get("value2").unwrap_or(&""));
        let parsed=mcp_form::parse_value(scalar,&text)?;
        let mut drafts=self.mcp_drafts.lock().await;let d=drafts.get_mut(id).context("入力し直してください")?;ensure!(d.revision==revision&&d.version==ver,"古い入力画面です。開き直してください");d.answers.insert(name.clone(),parsed);d.version+=1;
      }
      if action=="clear" {let idx:usize=parts.get(4).context("field missing")?.parse()?;let name=props.keys().nth(idx).context("項目がありません")?;let mut ds=self.mcp_drafts.lock().await;let d=ds.get_mut(id).unwrap();d.answers.remove(name);d.version+=1;}
      if action=="submit" {
        let ver:u64=parts.get(4).context("version missing")?.parse()?;let d=self.mcp_drafts.lock().await.get(id).cloned().context("入力し直してください")?;
        ensure!(d.version==ver&&d.revision==revision,"入力内容が変わりました。最新の画面から送信してください");
        self.mcp_reply(id,thread,revision,"accept",d.answers).await?;
        self.discord.reply_components(app,token,"入力内容を送信しました。元のカードで状態を確認できます。",json!([])).await?;return Ok(());
      }
      if action=="field" {
        let idx:usize=v["data"]["values"][0].as_str().context("項目を選んでください")?.parse()?;
        let(name,scalar)=props.iter().nth(idx).context("項目がありません")?;
        let mut help=mcp_form::help(name,scalar);
        let d=self.mcp_drafts.lock().await.get(id).cloned().unwrap();
        if let Some(value)=d.answers.get(name){help.push_str(&format!("\n現在の入力: {}",mcp_form::display(value)));}
        help.push_str("\n空文字列も入力として扱います。未指定に戻す場合は「入力を消す」を選んでください。");
        let controls=json!([{"type":1,"components":[button(&format!("mcp:{id}:{revision}:edit:{idx}"),"入力する",1),button(&format!("mcp:{id}:{revision}:clear:{idx}"),"入力を消す",2),button(&format!("mcp:{id}:{revision}:form"),"項目一覧へ",2)]}]);
        return self.mcp_private(app,token,&self.redact(&s,&clean(&help)),controls).await;
      }
      let page=if action=="page"{parts.get(4).context("page missing")?.parse::<usize>()?}else{0};ensure!(page<=1,"page invalid");
      let d=self.mcp_drafts.lock().await.get(id).cloned().unwrap();let options:Vec<_>=props.iter().enumerate().skip(page*25).take(25).map(|(idx,(name,_))|{let required=schema["required"].as_array().is_some_and(|a|a.iter().any(|v|v==name));json!({"label":short(&clean(&format!("{}{} {name}",if d.answers.contains_key(name){"✓"}else{"未入力"},if required{"（必須）"}else{""})),90),"value":idx.to_string()})}).collect();
      let mut controls=vec![];if !options.is_empty(){controls.push(json!({"type":1,"components":[{"type":3,"custom_id":format!("mcp:{id}:{revision}:field"),"placeholder":"入力する項目を選択","options":options}]}));}
      controls.push(json!({"type":1,"components":[button(&format!("mcp:{id}:{revision}:submit:{}",d.version),"この内容で送信",3),button(&format!("mcp:{id}:{revision}:decline"),"拒否",4)]}));
      if props.len()>25{controls.push(json!({"type":1,"components":[button(&format!("mcp:{id}:{revision}:page:{}",1-page),"別のページ",2)]}));}
      self.discord.reply_components(app,token,&format!("MCP入力フォーム（入力済み {}/{}項目）\n項目を選んで入力し、最後に「この内容で送信」を押してください。既定値は自動採用しません。Bot再起動時は入力し直してください。",d.answers.len(),props.len()),Value::Array(controls)).await
    }.await;
        if let Err(e) = result {
            // Errors may include transport details. Only locally authored validation messages are shown.
            let message = if e
                .to_string()
                .chars()
                .any(|c| ('\u{3040}'..='\u{9fff}').contains(&c))
            {
                e.to_string()
            } else {
                "操作を確認できませんでした。元のカードを確認してください。自動再送はしません。"
                    .into()
            };
            if silent {
                self.discord.followup_error(app, token, &message).await?;
            } else {
                self.discord.reply(app, token, &message).await?;
            }
        }
        Ok(())
    }
    pub(crate) async fn mcp_private(
        &self,
        app: &str,
        token: &str,
        text: &str,
        controls: Value,
    ) -> Result<()> {
        let parts = chunks(text);
        for (i, t) in parts.iter().enumerate() {
            if i == 0 {
                self.discord.reply(app, token, t).await?;
            } else {
                self.discord.followup_error(app, token, t).await?;
            }
        }
        // Controls only after every description fragment has been delivered.
        self.discord
            .followup_components(app, token, "内容を確認して操作してください。", controls)
            .await
    }
}
