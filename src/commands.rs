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
                            if jobs.len()>=8&&!stop{continue;}
                            if stop{
                                let Some(thread)=v["channel_id"].as_str() else{continue};let Some(id)=v["id"].as_str() else{continue};
                                if app.store.conversation(thread).await.is_err(){continue;}
                                if let Some(custom)=v["data"]["custom_id"].as_str(){let target=custom.trim_start_matches("stop:");if !app.store.active(thread).await?.is_some_and(|r|r.id==target){continue;}}
                                // Guild/user plus persisted conversation association authorize durable pause.
                                let expected=v["data"]["custom_id"].as_str().and_then(|s|s.strip_prefix("stop:")).map(str::to_owned);
                            if app.store.stop_target(id.into(),thread.into(),expected).await.is_err(){continue;}
                            }
                            if jobs.len()>=10{continue;}
                            jobs.spawn(async move{
                                let Some(id)=v["id"].as_str() else{return};let Some(token)=v["token"].as_str() else{return};let Some(application)=v["application_id"].as_str() else{return};
                                let _=tokio::time::timeout(Duration::from_millis(2500),app.discord.acknowledge(id,token)).await;
                                let result=tokio::time::timeout(Duration::from_secs(45),app.command(&v,stop)).await;
                                let text=match result{Ok(Ok(text))=>text,_=>"操作を完了確認できませんでした。/status で確認してください。実行要求の自動再送はしません。".into()};
                                let _=app.discord.reply(application,token,&text).await;
                            });
                        },_=>{}
                    }
                }
            }
        }
    }
    async fn command(&self, v: &Value, stopped: bool) -> Result<String> {
        let s = self.settings().await;
        let thread = v["channel_id"].as_str().context("missing channel")?;
        let iid = v["id"].as_str().context("missing interaction")?;
        let name = v["data"]["name"].as_str().unwrap_or("");
        if !matches!(name, "status" | "stop") && !stopped {
            ensure!(
                !self.recovery.load(Ordering::SeqCst) && !self.reloading.load(Ordering::SeqCst),
                "recovery or reload pending"
            );
        }
        if name == "new" {
            return self
                .new_conversation(iid, thread, option(v, "title").context("title required")?)
                .await;
        }
        let cv = self.authorized_thread(thread).await?;
        if stopped {
            if let Some(r) = self.store.active(thread).await? {
                self.interrupt(&r).await?;
                return Ok("待機列を一時停止しました。実行中の依頼へ中断を要求しています。停止完了は /status で確認してください。".into());
            }
            return Ok("待機列を一時停止しました。/resume で再開できます。".into());
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
                let text = format!(
                    "状態: {state}\n待機列: {}\n会話継続: {}\n選択モデル: {}\n実行モデル: {}\nProxy: {}\n回答本文はGatewayに保存しません。再起動・配信失敗後は全文を取り戻せない場合があります。",
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
            "resume" => {
                let Some((op, _)) = self.store.reserve_resume(iid.into(), thread.into()).await?
                else {
                    return Ok("この操作は受付済みです。".into());
                };
                let changed = self.store.apply_resume(op).await?;
                Ok(if changed{"待機列の自動開始を再開しました。UNKNOWNや権限・接続の問題は別途解消が必要です。"}else{"後続の停止操作などにより再開しませんでした。"}.into())
            }
            "model" => self.choose_model(iid, thread, option(v, "id")).await,
            "steer" => {
                self.steer(iid, thread, option(v, "text").context("text required")?)
                    .await
            }
            "get" => {
                self.get_artifact(iid, thread, option(v, "path").context("path required")?)
                    .await
            }
            _ => anyhow::bail!("unknown command"),
        }
    }
    async fn new_conversation(&self, iid: &str, channel: &str, title: &str) -> Result<String> {
        let s = self.settings().await;
        ensure!(
            title.chars().count() >= 1 && title.chars().count() <= 100,
            "invalid thread title"
        );
        let p = s
            .cfg
            .projects
            .iter()
            .find(|p| p.channel_id == channel && p.lifecycle == "ACTIVE")
            .context("use project channel")?;
        let ch = self
            .discord
            .get(&format!("/channels/{}", snowflake(channel)?))
            .await?;
        ensure!(
            ch["guild_id"] == s.cfg.discord.guild_id && ch["type"] == 0,
            "project channel mismatch"
        );
        let (id, project) = (iid.to_owned(), p.id.clone());
        let send=self.store.call(true,move|c|{Ok(c.execute("INSERT OR IGNORE INTO conversation_creations(interaction_id,project_id,state) VALUES(?1,?2,'SENDING')",params![id,project])?==1)}).await?;
        ensure!(send, "thread creation already attempted");
        let v = self
            .discord
            .api(
                reqwest::Method::POST,
                &format!("/channels/{channel}/threads"),
                Some(json!({"name":title,"type":11,"auto_archive_duration":1440})),
            )
            .await?;
        let thread = v["id"].as_str().context("thread receipt missing")?;
        ensure!(
            v["parent_id"] == channel && v["guild_id"] == s.cfg.discord.guild_id,
            "thread receipt mismatch"
        );
        let (id, project, t) = (iid.to_owned(), p.id.clone(), thread.to_owned());
        self.store.call(true,move|c|{let tx=c.transaction()?;tx.execute("INSERT INTO conversations(thread_id,project_id,selected_model) SELECT ?1,id,default_model FROM projects WHERE id=?2 AND lifecycle='ACTIVE'",params![t,project])?;tx.execute("UPDATE conversation_creations SET state='CONFIRMED',thread_id=?2 WHERE interaction_id=?1",params![id,t])?;tx.commit()?;Ok(())}).await?;
        Ok(format!(
            "会話を作成しました: <#{thread}>\nこのスレッドへの通常投稿が作業依頼になります。"
        ))
    }
    async fn choose_model(&self, iid: &str, thread: &str, desired: Option<&str>) -> Result<String> {
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
        match result{Ok(true)=>Ok(self.redact(&s,&format!("次に開始する依頼のモデルを {desired} にしました。実行中のTurnには適用しません。"))),Ok(false)=>Ok("より新しいモデル選択があるため、この操作は適用しませんでした。".into()),Err(e)=>{self.store.call(true,move|c|{c.execute("UPDATE operations SET state='FAILED' WHERE id=?1 AND state='VALIDATING'",[op])?;Ok(())}).await?;Err(e)}}
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
        Ok("承認への回答を受け付けました。実行結果は別途確認します。".into())
    }
    async fn get_artifact(&self, iid: &str, thread: &str, relative: &str) -> Result<String> {
        let s = self.settings().await;
        let cv = self.store.conversation(thread).await?;
        let w = s
            .workspaces
            .iter()
            .find(|w| w.project.id == cv.project_id)
            .context("workspace inactive")?
            .clone();
        let path = relative.to_owned();
        let limit = s.cfg.limits.artifact_bytes;
        let _reservation = self
            .files
            .reserve(limit as u64, s.cfg.limits.temp_bytes)
            .await?;
        let bytes =
            tokio::task::spawn_blocking(move || crate::files::artifact(&w, &path, limit)).await??;
        let filename = std::path::Path::new(relative)
            .file_name()
            .and_then(|s| s.to_str())
            .context("file name invalid")?
            .to_owned();
        ensure!(!filename.contains(['\r', '\n']), "invalid filename");
        let (i, t, hash) = (iid.to_owned(), thread.to_owned(), domain::digest(&bytes));
        self.store.call(true,move|c|{c.execute("INSERT INTO deliveries(id,target_id,thread_id,kind,part,state,pending_digest,pending_revision,created_at) VALUES(?1,?1,?2,'artifact',0,'POST_PENDING',?3,1,?4)",params![i,t,hash,domain::now_ms()])?;Ok(())}).await?;
        let v = self
            .discord
            .upload(thread, filename, bytes, &crate::delivery::nonce(iid))
            .await?;
        let mid = v["id"]
            .as_str()
            .context("artifact receipt unknown")?
            .to_owned();
        let i = iid.to_owned();
        self.store.call(true,move|c|{c.execute("UPDATE deliveries SET state='CONFIRMED',message_id=?2,confirmed_digest=pending_digest,confirmed_revision=1,pending_digest=NULL,pending_revision=NULL WHERE id=?1",params![i,mid])?;Ok(())}).await?;
        Ok("指定された成果物を返送しました。".into())
    }
}
fn option<'a>(v: &'a Value, name: &str) -> Option<&'a str> {
    v["data"]["options"]
        .as_array()?
        .iter()
        .find(|o| o["name"] == name)?["value"]
        .as_str()
}
