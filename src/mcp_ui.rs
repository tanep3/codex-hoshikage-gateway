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
struct Record {
    id: String,
    remote: String,
    request: String,
    response: String,
    conversation: String,
    workspace: String,
    binding: Binding,
    revision: i64,
    digest: String,
    expires: i64,
    key: Option<String>,
    action: Option<String>,
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
impl App {
    async fn mcp_record(&self, id: &str) -> Result<Record> {
        let id = id.to_owned();
        self.store
            .call(false, move |c| Ok(c.query_row(SELECT, [id], row)?))
            .await
    }
    async fn mcp_current(&self, r: &Record) -> Result<Value> {
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
    async fn mcp_pending(&self, r: &Record, thread: &str, revision: i64) -> Result<Value> {
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
                self.close_mcp(&rec,&r.thread_id,"この承認・入力は期限切れ、作業終了、または復旧確認中のため操作できません。送信済みの回答は自動再送しません。").await?;
            }
        }
        Ok(())
    }
    pub async fn scan_mcp(&self, id: &str) -> Result<()> {
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
        let data = list["data"]
            .as_array()
            .context("interaction list invalid")?;
        ensure!(
            data.len() <= 16 && serde_json::to_vec(&list)?.len() <= 65536,
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
        let existing=tx.query_row(SELECT,[&id],row)?;ensure!(existing.request==rid&&existing.response==resp&&existing.binding==b,"interaction identity reused");tx.commit()?;Ok(id)
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
    async fn render_mcp(&self, id: &str, v: &Value) -> Result<()> {
        let rec = self.mcp_record(id).await?;
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
                    Ok(())
                })
                .await?;
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
        let valid = mcp_form::validate_schema(&form["requestedSchema"]);
        let text = self.redact(
            &s,
            &clean(&format!(
                "MCPから確認が届きました\nサーバー: {}\n\n{}",
                form["serverName"].as_str().context("server missing")?,
                form["message"].as_str().context("message missing")?
            )),
        );
        let text = format!(
            "{text}{}{}",
            form["requestedSchema"]
                .get("title")
                .map(|v| format!("\n{}", clean(&mcp_form::display(v))))
                .unwrap_or_default(),
            form["requestedSchema"]
                .get("description")
                .map(|v| format!("\n{}", clean(&mcp_form::display(v))))
                .unwrap_or_default()
        );
        let text = self.redact(&s, &text);
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
        let buttons = json!([{"type":1,"components":[button(&format!("{prefix}:{}",if empty{"accept"}else{"form"}),if empty{"今回許可"}else{"入力フォームを開く"},if empty{3}else{1}),button(&format!("{prefix}:decline"),"拒否",4)]}]);
        self.delivery
            .text(
                id,
                &req.thread_id,
                "mcp_action",
                0,
                "今回の要求だけが対象です。自動許可・包括許可はしません。",
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
        ensure!(matches!(action, "accept" | "decline"), "invalid action");
        let rec = self.mcp_record(id).await?;
        let v = self.mcp_pending(&rec, thread, revision).await?;
        if action == "accept" {
            mcp_form::validate_answers(&v["request"]["requestedSchema"], &answers)?;
        }
        let body = json!({"expected_revision":revision,"response":{"action":action,"content":if action=="accept"{Value::Object(answers)}else{Value::Null}}});
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
      ensure!(tx.execute("UPDATE mcp_interactions SET operation_key=?2,operation_state='SENDING',reply_digest=?3,action=?4 WHERE id=?1 AND operation_key IS NULL AND closed=0 AND state='pending' AND revision=?5 AND expires_at>?6",params![r.id,k,hash,a,revision,domain::now_ms()])?==1,"回答済みまたは失効済みです");tx.commit()?;Ok(())
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
    async fn mcp_private(&self, app: &str, token: &str, text: &str, controls: Value) -> Result<()> {
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
