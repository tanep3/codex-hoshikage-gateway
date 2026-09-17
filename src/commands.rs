use crate::{
    application::App,
    discord::{Incoming, snowflake},
    domain::{self, RequestState},
    proxy::path_id,
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Duration};
use tokio::{sync::mpsc, task::JoinSet};
impl App {
    pub async fn control_loop(&self, mut rx: mpsc::Receiver<Incoming>) -> Result<()> {
        let mut jobs = JoinSet::new();
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>{jobs.shutdown().await;return Ok(())},
                result=jobs.join_next(),if !jobs.is_empty()=>{if result.is_some_and(|r|r.is_err()){tracing::warn!(event="control_worker_lost");}},
                incoming=rx.recv()=>{
                    match incoming.context("control event channel closed")?{
                        Incoming::Connected(app)=>{let s=self.settings().await;self.connected.store(false,Ordering::SeqCst);self.discord.register(&app,&s.cfg.discord.guild_id).await?;self.connected.store(true,Ordering::SeqCst);},
                        Incoming::Reconnected=>{let s=self.settings().await;let _=s.proxy.check().await;self.connected.store(true,Ordering::SeqCst);},
                        Incoming::Interaction(v)=>{
                            let s=self.settings().await;
                            let user=v["member"]["user"]["id"].as_str().or(v["user"]["id"].as_str());
                            if v["guild_id"]!=s.cfg.discord.guild_id||user!=Some(s.cfg.discord.allowed_user_id.as_str()){continue;}
                            if let Some(id)=v["id"].as_str() && !self.store.admissible_event(id.into()).await?{continue;}
                        let app=self.clone();
                            // Stop can save pause even when other control workers are occupied.
                            let stop=v["data"]["name"]=="stop"||v["data"]["custom_id"].as_str().is_some_and(|x|x.starts_with("stop:"));
                            let cancel=v["data"]["name"]=="cancel";
                            let urgent=stop||cancel||v["data"]["custom_id"].as_str().is_some_and(|x|x.starts_with("ma6:decline:")||x.starts_with("mt:revoke")||x.ends_with(":decline"));
                            if jobs.len()>=8&&!urgent{continue;}
                            if stop{
                                let Some(thread)=v["channel_id"].as_str() else{continue};let Some(id)=v["id"].as_str() else{continue};
                                if app.store.conversation(thread).await.is_err(){continue;}
                                if let Some(custom)=v["data"]["custom_id"].as_str(){let target=custom.trim_start_matches("stop:");if !app.store.active(thread).await?.is_some_and(|r|r.id==target){continue;}}
                                // Guild/user plus persisted conversation association authorize durable pause.
                                let expected=v["data"]["custom_id"].as_str().and_then(|s|s.strip_prefix("stop:")).map(str::to_owned);
                            if app.store.stop_target(id.into(),thread.into(),expected).await.is_err(){continue;}
                            }
                            if cancel {
                                let Some(thread)=v["channel_id"].as_str() else{continue};
                                let Some(id)=v["id"].as_str() else{continue};
                                if app.store.conversation(thread).await.is_ok() && app.store.cancel_latest(id.into(),thread.into()).await.is_err(){continue;}
                            }
                            if jobs.len()>=10{continue;}
                            jobs.spawn(async move{
                                let Some(id)=v["id"].as_str() else{return};let Some(token)=v["token"].as_str() else{return};let Some(application)=v["application_id"].as_str() else{return};
                                if v["data"]["custom_id"].as_str().is_some_and(|x|x.starts_with("ma6:")) {let _=app.handle_v06_mcp(&v).await;return;}
                                if v["data"]["custom_id"].as_str().is_some_and(|x|x.starts_with("mi:")) {let _=app.handle_inline_mcp(&v).await;return;}
                                if v["data"]["custom_id"].as_str().is_some_and(|x|x.starts_with("mt:")) {let _=app.handle_mcp_turn(&v).await;return;}
                                if v["data"]["custom_id"].as_str().is_some_and(|x|x.starts_with("mcp:")) {
                                    if app.handle_mcp(&v).await.is_err(){tracing::warn!(event="mcp_ui_operation_failed");}
                                    return;
                                }
                                let approval=v["type"]==3&&v["data"]["custom_id"].as_str().is_some_and(|x|x.starts_with("approval:"));
                                if approval {
                                    let _=tokio::time::timeout(Duration::from_millis(2500),app.discord.acknowledge_update(id,token)).await;
                                } else {
                                    let _=tokio::time::timeout(Duration::from_millis(2500),app.discord.acknowledge(id,token)).await;
                                }
                                let result=tokio::time::timeout(Duration::from_secs(45),app.command(&v,stop)).await;
                                if approval {
                                    // The approval UI worker owns the original card. Do not create a second success message.
                                    if !matches!(result,Ok(Ok(_))) {
                                        let _=app.discord.followup_error(application,token,"操作を完了確認できませんでした。元の承認カードで状態を確認してください。承認の自動再送はしません。").await;
                                    }
                                    return;
                                }
                                let text=match result{Ok(Ok(text))=>text,_=>"操作を完了確認できませんでした。/status で確認してください。実行要求の自動再送はしません。".into()};
                                if text.is_empty(){return;}
                                let components=app.project_menu(&v).await.unwrap_or(json!([]));
                                let _=app.discord.reply_components(application,token,&text,components).await;
                            });
                        },_=>{}
                    }
                }
            }
        }
    }
    pub(crate) async fn text_control_command(&self, v: &Value) -> Result<String> {
        let id = v["id"].as_str().context("message ID missing")?;
        ensure!(
            self.store.admissible_event(id.into()).await?,
            "old command message"
        );
        let parts = v["content"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>();
        let name = parts.first().copied().unwrap_or("");
        if name == "/project" {
            return Ok("作業先の登録は不要になりました。Proxyが会話ごとのワークを自動で用意します。そのまま話しかけてください。".into());
        }
        if !matches!(name, "/model" | "/models") {
            return Ok("この操作はDiscordのコマンド選択から実行してください。AIへの作業依頼としては送っていません。".into());
        }
        if v["attachments"].as_array().is_some_and(|a| !a.is_empty()) {
            return Ok(
                "モデル操作にはファイルを添付せず、/model モデルID と送ってください。".into(),
            );
        }
        let argument=match parts.as_slice() {
            [_]=>None,
            ["/model",value]=>Some(value.strip_prefix("id:").unwrap_or(value).trim()),
            ["/model","id:",value]=>Some(*value),
            _=>return Ok("一覧は /models、確認は /model、変更は /model chatgpt/gpt-5.6-terra の形式で送ってください。".into()),
        };
        if argument.is_some_and(str::is_empty) {
            return Ok("変更先のモデルIDを指定してください。/models で一覧を確認できます。".into());
        }
        let options = argument
            .map(|value| json!([{"name":"id","value":value}]))
            .unwrap_or(json!([]));
        self.command(&json!({"id":id,"channel_id":v["channel_id"],"data":{"name":name.trim_start_matches('/'),"options":options}}),false).await
    }
    async fn command(&self, v: &Value, stopped: bool) -> Result<String> {
        let s = self.settings().await;
        let thread = v["channel_id"].as_str().context("missing channel")?;
        let iid = v["id"].as_str().context("missing interaction")?;
        let name = v["data"]["name"].as_str().unwrap_or("");
        if !matches!(name, "status" | "stop" | "cancel") && !stopped {
            ensure!(
                !self.recovery.load(Ordering::SeqCst) && !self.reloading.load(Ordering::SeqCst),
                "recovery or reload pending"
            );
        }
        if v["data"]["custom_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("model-"))
        {
            return self.project_selection(v).await;
        }
        if name == "project" {
            return Ok(
                "作業先の登録は不要になりました。Proxyが会話ごとのワークを自動で用意します。"
                    .into(),
            );
        }
        if name == "models" {
            return self.choose_model(iid, thread, None).await;
        }
        if name == "new" {
            return self
                .new_conversation(iid, thread, option(v, "title").context("title required")?)
                .await;
        }
        if !self.ensure_channel_conversation(thread).await? {
            return Ok(
                "この場所では会話できません。Botの閲覧権限とチャンネルの種類を確認してください。"
                    .into(),
            );
        }
        let cv = self.authorized_thread(thread).await?;
        if name == "model" && option(v, "id").is_none() {
            return self.start_model_menu(iid, thread).await;
        }
        if stopped {
            if let Some(r) = self.store.active(thread).await? {
                self.interrupt(&r).await?;
                return Ok("待機列を一時停止しました。実行中の依頼へ中断を要求しています。停止完了は /status で確認してください。".into());
            }
            return Ok("待機列を一時停止しました。/resume で再開できます。".into());
        }
        if let Some(custom) = v["data"]["custom_id"]
            .as_str()
            .filter(|s| s.starts_with("pick:") || s.starts_with("page:"))
        {
            return self
                .selection_action(iid, thread, custom, v["data"]["values"][0].as_str())
                .await;
        }
        if let Some(custom) = v["data"]["custom_id"]
            .as_str()
            .filter(|x| x.starts_with("retry:") || x.starts_with("resend:"))
        {
            return self
                .retry_action(iid, thread, custom, v["data"]["values"][0].as_str())
                .await;
        }
        if let Some(custom) = v["data"]["custom_id"].as_str() {
            let mut parts = custom.split(':');
            ensure!(parts.next() == Some("approval"), "unknown component");
            let aid = parts.next().context("approval ID required")?;
            let decision = parts.next().context("decision required")?;
            ensure!(parts.next().is_none(), "invalid component");
            return self.approve(iid, thread, aid, decision).await;
        }
        match name {
            "status" => {
                let active = self.store.active(thread).await?;
                let state = active
                    .as_ref()
                    .map(|r| r.state.as_str())
                    .unwrap_or("実行中なし");
                let location = thread.to_owned();
                let latest=self.store.call(false,move|c|{use rusqlite::OptionalExtension;Ok(c.query_row("SELECT state,error_code,stop_requested,EXISTS(SELECT 1 FROM mcp_interactions m WHERE m.request_id=requests.id AND m.action='decline' AND m.operation_state='succeeded') FROM requests WHERE thread_id=?1 ORDER BY sequence DESC LIMIT 1",[location],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,bool>(2)?,r.get::<_,bool>(3)?))).optional()?)}).await?;
                let rejection = latest
                    .filter(|(state, _, _, _)| matches!(state.as_str(), "FAILED" | "CANCELLED"))
                    .map(|(state, code, stopped, declined)| {
                        format!(
                            "直前の依頼: {}\n",
                            if state == "CANCELLED" {
                                crate::application::interruption_with_evidence(
                                    code.as_deref(),
                                    stopped,
                                    declined,
                                )
                            } else {
                                crate::application::failure_message(code.as_deref())
                            }
                        )
                    })
                    .unwrap_or_default();
                let t = thread.to_owned();
                let deliveries:Vec<(String,i64)>=self.store.call(false,move|c|{let mut st=c.prepare("SELECT state,count(*) FROM resource_deliveries WHERE thread_id=?1 AND state NOT IN ('DELIVERED','SUPERSEDED') GROUP BY state")?;Ok(st.query_map([t],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
                let delivery_status = if deliveries.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n配信: {}\n再送が必要なら /retry（同じ保存版を送ります）。",
                        deliveries
                            .iter()
                            .map(|(s, n)| format!("{s}: {n}"))
                            .collect::<Vec<_>>()
                            .join(" / ")
                    )
                };
                let t = thread.to_owned();
                let images:Vec<(String,i64)>=self.store.call(false,move|c|{let mut q=c.prepare("SELECT state,count(*) FROM generated_image_watches WHERE thread_id=?1 GROUP BY state")?;Ok(q.query_map([t],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
                let t = thread.to_owned();
                let mcp:Vec<(String,i64)>=self.store.call(false,move|c|{let mut q=c.prepare("SELECT m.state,count(*) FROM mcp_interactions m JOIN requests r ON r.id=m.request_id WHERE r.thread_id=?1 AND (m.closed=0 OR m.state='unknown' OR m.operation_state IN ('SENDING','unknown')) GROUP BY m.state")?;Ok(q.query_map([t],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
                let mcp_status = if mcp.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\nMCP承認・入力: {}",
                        mcp.iter()
                            .map(|(s, n)| format!(
                                "{} {n}件",
                                match s.as_str() {
                                    "pending" => "回答待ち",
                                    "sending" => "送信確認中",
                                    "submitted" => "回答送信済み",
                                    "unknown" => "結果不明",
                                    _ => "照合中",
                                }
                            ))
                            .collect::<Vec<_>>()
                            .join(" / ")
                    )
                };
                let image_status = if images.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n画像: {}",
                        images
                            .iter()
                            .map(|(state, n)| format!(
                                "{} {n}件",
                                match state.as_str() {
                                    "DONE" => "確認済み",
                                    "WATCHING" => "登録・配信を確認中",
                                    "UNSUPPORTED" => "追跡対象外",
                                    "EXPIRED" => "照会期限切れ",
                                    _ => "未完了・確認が必要",
                                }
                            ))
                            .collect::<Vec<_>>()
                            .join(" / ")
                    )
                };
                let preparation_status = if let Some(r) = &active {
                    self.store
                        .v06_status(&r.id)
                        .await?
                        .map(|s| format!("\n{s}"))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let recovery_help = if cv.continuation == "NEW_CONVERSATION_REQUIRED" {
                    "\n会話の接続状態を確認する必要があります。実行中・結果不明の依頼がある場合は、その確認が終わるまで新しい実行を保留します。"
                } else {
                    ""
                };
                let text = format!(
                    "{rejection}状態: {state}\n待機列: {}\n会話継続: {}\n選択モデル: {}\n実行モデル: {}\nProxy: {}\n確定回答と成果物はProxyの保存期限内に再取得します。{delivery_status}{image_status}{mcp_status}{preparation_status}{recovery_help}",
                    if cv.paused {
                        "停止中"
                    } else {
                        "再開済み"
                    },
                    cv.continuation,
                    cv.selected_model,
                    cv.effective_model.as_deref().unwrap_or("未実行"),
                    if s.proxy.gate.is_ready() {
                        "利用可能"
                    } else {
                        "確認中／利用不可"
                    }
                );
                Ok(self.redact(&s, &text))
            }
            "cancel" => {
                let (decision, target) =
                    self.store.cancel_latest(iid.into(), thread.into()).await?;
                let Some(target) = target else {
                    return Ok("取り消せる依頼はありません。".into());
                };
                let id = target.clone();
                let message: String = self
                    .store
                    .call(true, move |c| {
                        Ok(c.query_row(
                            "SELECT message_id FROM admissions WHERE request_id=?1",
                            [id],
                            |r| r.get(0),
                        )?)
                    })
                    .await?;
                let link = format!(
                    "https://discord.com/channels/{}/{}/{}",
                    s.cfg.discord.guild_id, thread, message
                );
                let result = if decision == "waiting" {
                    "待機中の依頼を1件取り消しました。Codexには実行させません。".to_owned()
                } else {
                    let request = self.store.request(&target).await?;
                    if !request.state.terminal() {
                        let _ = self.interrupt(&request).await;
                    }
                    match self.store.request(&target).await?.state {
                        RequestState::Cancelled => "依頼の停止を確認しました。".into(),
                        RequestState::Completed | RequestState::Failed => "依頼はすでに終了していました。実行済みの変更は元に戻りません。".into(),
                        _ => "取消要求を保存しました。停止の確認が取れるまで後続を保留します。/status で確認してください。".into(),
                    }
                };
                let paused = if cv.paused {
                    "\n待機列は一時停止中です。残った依頼を進めるには /resume を使ってください。"
                } else {
                    ""
                };
                Ok(format!("{result}\n対象: {link}{paused}"))
            }
            "resume" => {
                let Some((op, _)) = self.store.reserve_resume(iid.into(), thread.into()).await?
                else {
                    return Ok("この操作は受付済みです。".into());
                };
                let changed = self.store.apply_resume(op).await?;
                Ok(if changed{"待機列の自動開始を再開しました。UNKNOWNや権限・接続の問題は別途解消が必要です。"}else{"後続の停止操作などにより再開しませんでした。"}.into())
            }
            "models" => self.choose_model(iid, thread, None).await,
            "model" => match option(v, "id") {
                Some(id) => self.choose_model(iid, thread, Some(id.trim())).await,
                None => Ok(self.redact(
                    &s,
                    &format!(
                        "選択中のモデル: {}\n/models で一覧、/model の id 欄で変更できます。",
                        cv.selected_model
                    ),
                )),
            },
            "mcp" => {
                self.latest_mcp_grants(
                    thread,
                    v["application_id"]
                        .as_str()
                        .context("application missing")?,
                    v["token"].as_str().context("token missing")?,
                )
                .await
            }
            "steer" => {
                self.steer(iid, thread, option(v, "text").context("text required")?)
                    .await
            }
            "workspace" => {
                self.selection_page(iid, thread, "workspace", "selectable", None)
                    .await
            }
            "retry" => self.retry_menu(iid, thread).await,
            "get" if option(v, "scope") == Some("shared") => {
                ensure!(
                    option(v, "path").is_none(),
                    "shared listing does not take path"
                );
                self.shared_artifact_menu(iid, thread).await
            }
            "get" => self.artifact_command(iid, thread, option(v, "path")).await,
            _ => anyhow::bail!("unknown command"),
        }
    }
    async fn new_conversation(&self, iid: &str, channel: &str, title: &str) -> Result<String> {
        let s = self.settings().await;
        ensure!(
            title.chars().count() >= 1 && title.chars().count() <= 100,
            "invalid thread title"
        );
        let mut ch = self
            .discord
            .get(&format!("/channels/{}", snowflake(channel)?))
            .await?;
        ensure!(
            ch["guild_id"] == s.cfg.discord.guild_id,
            "channel guild mismatch"
        );
        if matches!(ch["type"].as_u64(), Some(11 | 12)) {
            let parent = ch["parent_id"].as_str().context("parent missing")?;
            ch = self
                .discord
                .get(&format!("/channels/{}", snowflake(parent)?))
                .await?;
        }
        ensure!(
            ch["guild_id"] == s.cfg.discord.guild_id && matches!(ch["type"].as_u64(), Some(0 | 15)),
            "unsupported conversation parent"
        );
        let channel = ch["id"].as_str().context("channel identity missing")?;
        if ch["type"] == 15 && ch["flags"].as_u64().unwrap_or(0) & 16 != 0 {
            return Ok("このフォーラムはタグの指定が必要です。Discordの「投稿を作成」からタグを選んで会話を作ってください。作成した投稿ではそのままBotと話せます。".into());
        }
        let body = new_thread_body(&ch, title)?;
        let (id, project) = (iid.to_owned(), crate::storage::PROXY_SCOPE.to_owned());
        let send=self.store.call(true,move|c|{Ok(c.execute("INSERT OR IGNORE INTO conversation_creations(interaction_id,project_id,state) VALUES(?1,?2,'SENDING')",params![id,project])?==1)}).await?;
        ensure!(send, "thread creation already attempted");
        let v = self
            .discord
            .api(
                reqwest::Method::POST,
                &format!("/channels/{channel}/threads"),
                Some(body),
            )
            .await?;
        let thread = v["id"].as_str().context("thread receipt missing")?;
        ensure!(
            v["parent_id"] == channel && v["guild_id"] == s.cfg.discord.guild_id,
            "thread receipt mismatch"
        );
        let (id, project, t) = (
            iid.to_owned(),
            crate::storage::PROXY_SCOPE.to_owned(),
            thread.to_owned(),
        );
        self.store.call(true,move|c|{let tx=c.transaction()?;tx.execute("INSERT INTO conversations(thread_id,project_id,selected_model) SELECT ?1,id,default_model FROM projects WHERE id=?2 AND lifecycle='ACTIVE'",params![t,project])?;tx.execute("UPDATE conversation_creations SET state='CONFIRMED',thread_id=?2 WHERE interaction_id=?1",params![id,t])?;tx.commit()?;Ok(())}).await?;
        Ok(format!(
            "会話を作成しました: <#{thread}>\nこのスレッドへの通常投稿が作業依頼になります。"
        ))
    }
    pub(crate) async fn choose_model(
        &self,
        iid: &str,
        thread: &str,
        desired: Option<&str>,
    ) -> Result<String> {
        let s = self.settings().await;
        if desired.is_none() {
            let models = s.proxy.get("/v1/models").await?;
            let names = models["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["id"].as_str())
                .take(25)
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(self.redact(
                &s,
                &format!("モデル一覧（最大25件）\n{names}\n/model id:モデルID で選択できます。"),
            ));
        }
        let desired = desired.unwrap().to_owned();
        ensure!(desired.len() <= 256, "model ID too long");
        let op = domain::id();
        let (i, t, o, d) = (
            iid.to_owned(),
            thread.to_owned(),
            op.clone(),
            desired.clone(),
        );
        let sequence=self.store.call(true,move|c|{let tx=c.transaction()?;ensure!(!tx.prepare("SELECT 1 FROM operations WHERE interaction_id=?1")?.exists([&i])?,"duplicate operation");tx.execute("UPDATE conversations SET next_control_sequence=next_control_sequence+1,latest_model_sequence=next_control_sequence+1 WHERE thread_id=?1",[&t])?;let seq:i64=tx.query_row("SELECT latest_model_sequence FROM conversations WHERE thread_id=?1",[&t],|r|r.get(0))?;tx.execute("INSERT INTO operations(id,interaction_id,thread_id,sequence,kind,desired_model,created_at) VALUES(?1,?2,?3,?4,'model',?5,?6)",params![o,i,t,seq,d,domain::now_ms()])?;tx.commit()?;Ok(seq)}).await?;
        let result=async{
            let cv=self.store.conversation(thread).await?;let models=s.proxy.get("/v1/models").await?;
            let entries=models["data"].as_array().context("model list unavailable")?;let next=entries.iter().find(|v|v["id"]==desired).context("model unavailable")?;
            if cv.proxy_thread_id.is_some(){let previous=entries.iter().find(|v|v["id"]==cv.effective_model.as_deref().unwrap_or(&cv.selected_model)).context("current provider unknown")?;ensure!(next["owned_by"].is_string()&&next["owned_by"]==previous["owned_by"],"provider change requires new conversation");}
            let(t,o,d)=(thread.to_owned(),op.clone(),desired.clone());
            self.store.call(true,move|c|{let tx=c.transaction()?;let n=tx.execute("UPDATE conversations SET selected_model=?2,selection_revision=selection_revision+1 WHERE thread_id=?1 AND latest_model_sequence=?3 AND EXISTS(SELECT 1 FROM operations WHERE id=?4 AND state='VALIDATING')",params![t,d,sequence,o])?;tx.execute("UPDATE operations SET state=?2 WHERE id=?1",params![o,if n==1{"APPLIED"}else{"SUPERSEDED"}])?;tx.commit()?;Ok(n==1)}).await
        }.await;
        match result{Ok(true)=>Ok(self.redact(&s,&format!("次に開始する依頼のモデルを {desired} にしました。実行中のTurnには適用しません。"))),Ok(false)=>Ok("より新しいモデル選択があるため、この操作は適用しませんでした。".into()),Err(e)=>{self.store.call(true,move|c|{c.execute("UPDATE operations SET state='FAILED' WHERE id=?1 AND state='VALIDATING'",[op])?;Ok(())}).await?;
            let message=match e.to_string().as_str() {
                "model unavailable"=>"そのモデルIDは利用できません。/models の一覧から id 欄に入力してください。",
                "provider change requires new conversation"=>"別Providerへの変更には /new で新しい会話を作成してください。",
                _=>"モデルを変更できませんでした。Proxyの接続と /models を確認してください。",
            };Ok(message.into())}}
    }
    async fn reserve_control(
        &self,
        iid: &str,
        r: &crate::domain::Request,
        kind: &str,
        input_hash: Option<String>,
        approval: Option<String>,
        decision: Option<String>,
    ) -> Result<String> {
        let (id, iid, r, kind) = (domain::id(), iid.to_owned(), r.clone(), kind.to_owned());
        let result = id.clone();
        self.store.call(true,move|c|{let tx=c.transaction()?;ensure!(!tx.prepare("SELECT 1 FROM operations WHERE interaction_id=?1")?.exists([&iid])?,"duplicate operation");
            let valid:bool=tx.query_row("SELECT turn_id=?2 AND proxy_thread_id=?3 AND state IN ('RUNNING','APPROVAL_REQUIRED') AND stop_requested=0 FROM requests WHERE id=?1",params![r.id,r.turn_id,r.proxy_thread_id],|r|r.get(0))?;ensure!(valid,"control target no longer active");
            if let Some(aid)=&approval{let n=tx.execute("UPDATE approvals SET operation_id=?2,state='DECISION_PENDING' WHERE id=?1 AND request_id=?3 AND operation_id IS NULL AND state='PENDING' AND (expires_at IS NULL OR expires_at>?4)",params![aid,id,r.id,domain::now_ms()])?;ensure!(n==1,"approval no longer available");}
            tx.execute("INSERT INTO operations(id,interaction_id,thread_id,kind,target_request_id,target_turn_id,target_thread_id,approval_id,decision,input_digest,state,send_state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'SENDING','SENDING',?11)",params![id,iid,r.thread_id,kind,r.id,r.turn_id,r.proxy_thread_id,approval,decision,input_hash,domain::now_ms()])?;tx.commit()?;Ok(())}).await?;
        Ok(result)
    }
    async fn steer(&self, iid: &str, thread: &str, text: &str) -> Result<String> {
        let s = self.settings().await;
        ensure!(
            !text.trim().is_empty() && text.len() <= s.cfg.limits.text_bytes,
            "steer text invalid"
        );
        let r = self.store.active(thread).await?.context("no active turn")?;
        ensure!(
            s.proxy.reconcile(&self.store, &r.id).await? == RequestState::Running,
            "steer unavailable in current state"
        );
        let r = self.store.request(&r.id).await?;
        let turn = r.turn_id.as_deref().context("Turn unknown")?;
        let op = self
            .reserve_control(
                iid,
                &r,
                "steer",
                Some(domain::digest(text.as_bytes())),
                None,
                None,
            )
            .await?;
        let result = s
            .proxy
            .control(
                &format!("/v1/codex/turns/{}/steer", path_id(turn)?),
                Some(json!({"expected_turn_id":turn,"input":text})),
            )
            .await;
        self.finish_operation(op, result.is_ok()).await?;
        result?;
        Ok(
            "対象Turnへの追加指示が受け付けられました。停止が必要な場合は /stop を使ってください。"
                .into(),
        )
    }
    async fn approve(&self, iid: &str, thread: &str, aid: &str, decision: &str) -> Result<String> {
        ensure!(
            matches!(decision, "accept" | "decline" | "cancel"),
            "decision prohibited"
        );
        let s = self.settings().await;
        let a = aid.to_owned();
        let rid = self
            .store
            .call(true, move |c| {
                Ok(
                    c.query_row("SELECT request_id FROM approvals WHERE id=?1", [a], |r| {
                        r.get::<_, String>(0)
                    })?,
                )
            })
            .await?;
        let r = self.store.request(&rid).await?;
        ensure!(r.thread_id == thread, "approval conversation mismatch");
        let current = s
            .proxy
            .get(&format!("/v1/codex/approvals/{}", path_id(aid)?))
            .await?;
        ensure!(
            current["details"]["threadId"].as_str() == r.proxy_thread_id.as_deref()
                && current["details"]["turnId"].as_str() == r.turn_id.as_deref(),
            "approval identity mismatch"
        );
        if current["state"] != "pending" {
            return Ok(
                "この承認は回答済み、または期限切れです。元の作業は再実行していません。".into(),
            );
        }
        ensure!(
            current["available_decisions"]
                .as_array()
                .is_some_and(|a| a.iter().any(|d| d == decision)),
            "decision unavailable"
        );
        let op = self
            .reserve_control(
                iid,
                &r,
                "approval",
                None,
                Some(aid.into()),
                Some(decision.into()),
            )
            .await?;
        let result=s.proxy.control(&format!("/v1/codex/approvals/{}",path_id(aid)?),Some(json!({"decision":decision,"expected_thread_id":r.proxy_thread_id,"expected_turn_id":r.turn_id}))).await;
        self.finish_operation(op, result.is_ok()).await?;
        result?;
        Ok(match decision {
            "accept" => "今回の操作を承認しました。続きの回答はこの会話に届きます。",
            "decline" => "この操作を拒否しました。",
            _ => "この承認を取り消しました。",
        }
        .into())
    }
}
fn option<'a>(v: &'a Value, name: &str) -> Option<&'a str> {
    v["data"]["options"]
        .as_array()?
        .iter()
        .find(|o| o["name"] == name)?["value"]
        .as_str()
}

/// Only exact Gateway command tokens are intercepted; ordinary prose and paths are untouched.
pub(crate) fn is_text_control(text: &str) -> bool {
    matches!(
        text.split_whitespace().next(),
        Some(
            "/project"
                | "/model"
                | "/models"
                | "/new"
                | "/status"
                | "/stop"
                | "/cancel"
                | "/resume"
                | "/steer"
                | "/get"
                | "/workspace"
                | "/retry"
                | "/mcp"
        )
    )
}

/// Forum threads require an initial message; regular text threads do not.
pub fn new_thread_body(channel: &Value, title: &str) -> Result<Value> {
    ensure!(
        !title.trim().is_empty() && title.chars().count() <= 100,
        "invalid title"
    );
    match channel["type"].as_u64() {
        Some(0) => Ok(json!({"name":title,"type":11,"auto_archive_duration":1440})),
        Some(15) => Ok(
            json!({"name":title,"auto_archive_duration":1440,"message":{"content":"ここで新しい会話を始められます。Botに話しかけてください。","allowed_mentions":{"parse":[]}}}),
        ),
        _ => anyhow::bail!("unsupported thread parent"),
    }
}
