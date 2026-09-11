use crate::{
    config::{Config, Workspace},
    delivery::{Delivery, chunks},
    discord::{Discord, Incoming, input_message, snowflake},
    domain::{self, Redactor, Request, RequestState},
    files::Files,
    proxy::{Proxy, SseDecoder, path_id},
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
#[derive(Clone)]
pub struct Settings {
    pub cfg: Config,
    pub revision: i64,
    pub workspaces: Vec<Workspace>,
    pub proxy: Proxy,
}
#[derive(Clone)]
pub struct App {
    pub settings: Arc<RwLock<Settings>>,
    pub store: Store,
    pub discord: Discord,
    pub files: Files,
    pub delivery: Delivery,
    pub cancel: CancellationToken,
    pub connected: Arc<AtomicBool>,
    pub recovery: Arc<AtomicBool>,
    pub reloading: Arc<AtomicBool>,
    pub retired_secrets: Arc<std::sync::RwLock<Vec<String>>>,
    pub output: Arc<Mutex<HashMap<String, Output>>>,
}
pub struct Output {
    pub thread: String,
    pub text: String,
    pub done: bool,
    pub lost: bool,
    pub created: Instant,
    pub last_progress: Instant,
    pub retention: Duration,
}
impl App {
    pub fn new(cfg: Config, store: Store, discord: Discord, proxy: Proxy) -> Result<Self> {
        let workspaces = cfg.validate()?;
        Ok(Self {
            settings: Arc::new(RwLock::new(Settings {
                cfg,
                revision: 1,
                workspaces,
                proxy,
            })),
            delivery: Delivery {
                store: store.clone(),
                discord: discord.clone(),
            },
            store,
            discord,
            files: Files::new()?,
            cancel: CancellationToken::new(),
            connected: Arc::new(AtomicBool::new(false)),
            recovery: Arc::new(AtomicBool::new(false)),
            reloading: Arc::new(AtomicBool::new(false)),
            retired_secrets: Arc::new(std::sync::RwLock::new(vec![])),
            output: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub async fn settings(&self) -> Settings {
        self.settings.read().await.clone()
    }
    pub async fn authorized_thread(&self, thread: &str) -> Result<crate::domain::Conversation> {
        let s = self.settings().await;
        let cv = self.store.conversation(thread).await?;
        let p = s
            .cfg
            .projects
            .iter()
            .find(|p| p.id == cv.project_id && p.lifecycle == "ACTIVE")
            .context("project inactive")?;
        let v = self
            .discord
            .get(&format!("/channels/{}", snowflake(thread)?))
            .await?;
        ensure!(
            v["id"] == thread
                && v["guild_id"] == s.cfg.discord.guild_id
                && ((v["id"] == p.channel_id && v["type"] == 0)
                    || (v["parent_id"] == p.channel_id
                        && matches!(v["type"].as_u64(), Some(11 | 12))))
                && v["thread_metadata"]["archived"] != true
                && v["thread_metadata"]["locked"] != true,
            "conversation unavailable or identity changed"
        );
        Ok(cv)
    }
    pub async fn ensure_channel_conversation(&self, channel: &str) -> Result<bool> {
        let s = self.settings().await;
        if let Some(p) = s
            .cfg
            .projects
            .iter()
            .find(|p| p.channel_id == channel && p.lifecycle == "ACTIVE")
        {
            self.store
                .add_conversation(channel.into(), p.id.clone())
                .await?;
            return Ok(true);
        }
        Ok(self.store.conversation(channel).await.is_ok())
    }
    pub async fn admit_loop(&self, mut rx: mpsc::Receiver<Incoming>) -> Result<()> {
        let mut workers = JoinSet::new();
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>{workers.shutdown().await;return Ok(())},
                result=workers.join_next(),if !workers.is_empty()=>{if result.is_some_and(|r|r.is_err()){tracing::warn!(event="validation_worker_lost");}},
                event=rx.recv()=>{
                    let Some(Incoming::Message(v))=event else{ensure!(!rx.is_closed(),"Discord admission channel closed");continue};
                    let s=self.settings().await;
                    if v["guild_id"]!=s.cfg.discord.guild_id||v["author"]["id"]!=s.cfg.discord.allowed_user_id||v["author"]["bot"]==true||!v["webhook_id"].is_null(){continue;}
                    if !self.discord.should_respond(&v,s.cfg.discord.response_mode){continue;}
                    let (Some(channel),Some(message))=(v["channel_id"].as_str(),v["id"].as_str()) else {continue};
                    if !self.ensure_channel_conversation(channel).await? {
                        self.notice(message.into(),channel.into(),"このチャンネルの作業フォルダーは未登録です。設定の projects に channel_id と cwd を登録し、設定を再読み込みしてください。").await?;
                        continue;
                    }
                    if self.reloading.load(Ordering::SeqCst)||self.recovery.load(Ordering::SeqCst)||!s.proxy.gate.is_ready()||!self.connected.load(Ordering::SeqCst){if let (Some(t),Some(id))=(v["channel_id"].as_str(),v["id"].as_str()) && self.store.conversation(t).await.is_ok(){self.notice(id.into(),t.into(),"受付停止中です。/status で状態を確認してください。").await?;}continue;}
                    let m=match input_message(&v,&s.cfg.discord.guild_id){Ok(m)=>m,Err(_)=>continue};
                    if workers.len()>=s.cfg.limits.queue_global{continue;}
                    // The Store transaction is the acceptance ordering boundary, before network validation.
                    let id=match self.store.reserve(m.id.clone(),m.thread_id.clone(),m.metadata_digest(),s.cfg.limits.clone()).await{Ok(Some(id))=>id,Ok(None)=>continue,Err(_)=>{if self.store.conversation(&m.thread_id).await.is_ok(){self.notice(m.id.clone(),m.thread_id.clone(),"受付できませんでした。会話が継続不可の場合は /new で新しい会話を作成してください。詳しくは /status で確認できます。").await?;}continue}};
                    let app=self.clone();workers.spawn(async move{
                        let result=async{
                            let cv=app.authorized_thread(&m.thread_id).await?;
                            let prepared=app.files.prepare(&m,&s.cfg.limits).await?;
                            let waiting=cv.paused || app.store.active(&m.thread_id).await?.is_some();
                            app.store.finalize(id.clone(),prepared.metadata_digest,prepared.digest,prepared.attachments).await?;
                            if waiting {app.notice(format!("queued-{id}"),m.thread_id.clone(),if cv.paused {"受け付けました。一時停止中のため待機します。/resume で再開できます。"}else{"受け付けました。先の作業が終わるまで待機します。"}).await?;}
                            Ok::<(),anyhow::Error>(())
                        }.await;
                        if result.is_err(){let _=app.store.reject_admission(id,"input_validation_failed").await;}
                    });
                }
            }
        }
    }
    pub async fn sweep_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{self.store.sweep().await?;}}
        }
    }
    pub async fn capability_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{let s=self.settings().await;let _=s.proxy.check().await;}}
        }
    }
    pub async fn scheduler_loop(&self) -> Result<()> {
        let mut jobs = JoinSet::new();
        let mut busy = HashSet::new();
        let mut ids: HashMap<tokio::task::Id, String> = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                            _=self.cancel.cancelled()=>{jobs.shutdown().await;return Ok(())},
                            result=jobs.join_next_with_id(),if !jobs.is_empty()=>{
                                let result=result.context("request task vanished")?;
                                let task=match &result{Ok((id,_))=>*id,Err(e)=>e.id()};
                                if let Some(id)=ids.remove(&task){busy.remove(&id);let s=self.settings().await;let r=self.store.request(&id).await?;if !matches!(r.state,RequestState::Queued|RequestState::Received)&&!r.state.terminal(){s.proxy.reconcile(&self.store,&id).await?;}
            if let Some(o)=self.output.lock().await.get_mut(&id){o.done=true;if !matches!(result,Ok((_,Ok(())))){o.lost=true;}}}
                            },
                            _=tick.tick()=>{
                                let s=self.settings().await;if self.reloading.load(Ordering::SeqCst)||self.recovery.load(Ordering::SeqCst)||!s.proxy.gate.is_ready()||!self.connected.load(Ordering::SeqCst)||jobs.len()>=2{continue;}
                                for r in self.store.candidates().await?{
                                    if jobs.len()>=2{break;}
            if !busy.insert(r.id.clone()){continue;}
                                    let app=self.clone();let id=r.id.clone();let handle=jobs.spawn(async move{app.execute(r).await});ids.insert(handle.id(),id);
                                }
                            }
                        }
        }
    }
    async fn execute(&self, r: Request) -> Result<()> {
        let mut s = self.settings().await;
        s.cfg.limits = self.store.input_limits(r.id.clone()).await?;
        let prepared = async {
            self.authorized_thread(&r.thread_id).await?;
            let m = self
                .discord
                .message(&r.thread_id, &r.message_id, &s.cfg.discord.guild_id)
                .await?;
            ensure!(
                m.user_id == s.cfg.discord.allowed_user_id,
                "input author changed"
            );
            let p = self.files.prepare(&m, &s.cfg.limits).await?;
            ensure!(p.digest == r.input_digest, "input changed after admission");
            Ok::<_, anyhow::Error>(p)
        }
        .await;
        let p = match prepared {
            Ok(p) => p,
            Err(_) => {
                self.store
                    .observe(
                        r.id,
                        RequestState::Failed,
                        "input_unrecoverable_before_send",
                        false,
                    )
                    .await?;
                return Ok(());
            }
        };
        let workspace = s
            .workspaces
            .iter()
            .find(|w| w.project.id == r.project_id)
            .context("workspace inactive")?;
        workspace.verify()?;
        ensure!(
            !self.reloading.load(Ordering::SeqCst),
            "configuration reload in progress"
        );
        let epoch = s.proxy.gate.epoch.load(Ordering::SeqCst);
        let permit = s.proxy.authorize(r.id.clone(), s.revision).await?;
        {
            let mut cache = self.output.lock().await;
            let total: usize = cache.values().map(|o| o.text.len()).sum();
            ensure!(
                total.saturating_add(s.cfg.limits.output_bytes) <= s.cfg.limits.output_total_bytes,
                "output memory capacity unavailable"
            );
            cache.insert(
                r.id.clone(),
                Output {
                    thread: r.thread_id.clone(),
                    text: String::new(),
                    done: false,
                    lost: false,
                    created: Instant::now(),
                    last_progress: Instant::now(),
                    retention: Duration::from_secs(s.cfg.limits.delivery_retention_secs),
                },
            );
        }
        let r = match self.store.begin_send_authorized(r.id.clone(), permit).await {
            Ok(r) => r,
            Err(e) => {
                self.output.lock().await.remove(&r.id);
                return Err(e);
            }
        };
        // If stop/shutdown wins after the durable boundary, do not create a new remote turn.
        if self.cancel.is_cancelled()
            || !s.proxy.gate.is_ready()
            || epoch != s.proxy.gate.epoch.load(Ordering::SeqCst)
            || self.store.request(&r.id).await?.stop_requested
        {
            self.store
                .observe(
                    r.id,
                    RequestState::Unknown,
                    "dispatch_cancelled_after_commit",
                    false,
                )
                .await?;
            return Ok(());
        }
        let response = s
            .proxy
            .start(
                &r,
                p.input,
                workspace.path.to_str().context("workspace encoding")?,
            )
            .await?;
        drop(p.reservation);
        let headers = response.headers();
        if let (Some(resp), Some(thread), Some(turn)) = (
            headers.get("x-response-id").and_then(|h| h.to_str().ok()),
            headers
                .get("x-codex-thread-id")
                .and_then(|h| h.to_str().ok()),
            headers.get("x-codex-turn-id").and_then(|h| h.to_str().ok()),
        ) {
            self.store
                .identify(r.id.clone(), resp.into(), thread.into(), turn.into())
                .await?;
        }
        let expected_response = self.store.request(&r.id).await?.response_id;
        let mut redactor = Redactor::new(self.secrets(&s));
        let mut decoder = SseDecoder::default();
        let mut stream = response.bytes_stream();
        let mut raw_lost = false;
        loop {
            let chunk = tokio::select! {_=self.cancel.cancelled()=>break,chunk=tokio::time::timeout(Duration::from_secs(90),stream.next())=>chunk.context("Responses stream silent")?};
            let Some(chunk) = chunk else { break };
            for event in decoder.feed(&chunk.context("Responses stream lost")?)? {
                let value: Value = match serde_json::from_str(&event.data) {
                    Ok(v) => v,
                    Err(_) => {
                        raw_lost = true;
                        continue;
                    }
                };
                if event.event == "response.output_text.delta" && !raw_lost {
                    if expected_response
                        .as_deref()
                        .is_none_or(|id| value["id"].as_str() != Some(id))
                    {
                        raw_lost = true;
                        continue;
                    }
                    if let Some(delta) = value["delta"].as_str() {
                        let clean = redactor.push(delta);
                        let mut cache = self.output.lock().await;
                        let total: usize = cache.values().map(|o| o.text.len()).sum();
                        if let Some(o) = cache.get_mut(&r.id) {
                            if o.text.len() + clean.len() <= s.cfg.limits.output_bytes
                                && total + clean.len() <= s.cfg.limits.output_total_bytes
                            {
                                o.text.push_str(&clean);
                                o.last_progress = Instant::now();
                            } else {
                                o.lost = true;
                                raw_lost = true;
                            }
                        }
                    }
                }
            }
        }
        if let Some(o) = self.output.lock().await.get_mut(&r.id) {
            if !raw_lost {
                o.text.push_str(&redactor.finish());
            }
            o.done = true;
            o.lost |= raw_lost;
        }
        s.proxy.reconcile(&self.store, &r.id).await?;
        Ok(())
    }
    pub fn secrets(&self, s: &Settings) -> Vec<String> {
        let mut v = vec![self.discord.secret(), s.proxy.secret()];
        for w in &s.workspaces {
            v.push(w.path.to_string_lossy().into_owned());
        }
        v.push(s.cfg.storage.state_dir.to_string_lossy().into_owned());
        v.push(s.cfg.storage.temp_dir.to_string_lossy().into_owned());
        v.extend(self.retired_secrets.read().unwrap().clone());
        v
    }
    pub fn redact(&self, s: &Settings, text: &str) -> String {
        let mut r = Redactor::new(self.secrets(s));
        let mut t = r.push(text);
        t.push_str(&r.finish());
        t
    }
    pub async fn event_loop(&self) -> Result<()> {
        let mut jobs = JoinSet::new();
        let mut ids: HashMap<tokio::task::Id, String> = HashMap::new();
        let mut active = HashSet::new();
        let mut attempts: HashMap<String, Instant> = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>{jobs.shutdown().await;return Ok(())},
                result=jobs.join_next_with_id(),if !jobs.is_empty()=>{
                    let result=result.context("event monitor disappeared")?;let task=match &result{Ok((id,_))=>*id,Err(e)=>e.id()};
                    if let Some(id)=ids.remove(&task){active.remove(&id);self.settings().await.proxy.reconcile(&self.store,&id).await?;}
                },
                _=tick.tick()=>{
                    for r in self.store.pending().await?{
                        if jobs.len()>=2{break;}
                        if r.turn_id.is_none()||r.state.terminal()||active.contains(&r.id)||attempts.get(&r.id).is_some_and(|t|t.elapsed()<Duration::from_secs(5)){continue;}
                        active.insert(r.id.clone());attempts.insert(r.id.clone(),Instant::now());let app=self.clone();let id=r.id.clone();let task=jobs.spawn(async move{app.watch_events(r).await});ids.insert(task.id(),id);
                    }
                    attempts.retain(|id,t|active.contains(id)||t.elapsed()<Duration::from_secs(60));
                }
            }
        }
    }
    async fn watch_events(&self, r: Request) -> Result<()> {
        let s = self.settings().await;
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            s.proxy
                .monitor(r.turn_id.as_deref().context("Turn missing")?),
        )
        .await??;
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        let mut last_byte = Instant::now();
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>return Ok(()),
                _=tick.tick()=>{if self.store.request(&r.id).await?.state.terminal(){return Ok(())}ensure!(last_byte.elapsed()<Duration::from_secs(90),"Turn monitor silent");},
                chunk=stream.next()=>{let Some(chunk)=chunk else{return Ok(())};last_byte=Instant::now();for event in decoder.feed(&chunk?)?{
                    match event.event.as_str(){
                        "codex.events.reset"|"codex.events.gap"|"codex.turn.snapshot"=>{s.proxy.reconcile(&self.store,&r.id).await?;},
                        "approval_requested"=>{let v:Value=serde_json::from_str(&event.data)?;ensure!(v["threadId"].as_str()==r.proxy_thread_id.as_deref()&&v["turnId"].as_str()==r.turn_id.as_deref(),"event target mismatch");self.discover_approvals(&r).await?;},
                        _=>{}
                    }
                }}
            }
        }
    }
    pub async fn monitor_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{
                let s=self.settings().await;
                for r in self.store.pending().await?{
                    let state=s.proxy.reconcile(&self.store,&r.id).await?;
                    let current=self.store.request(&r.id).await?;
                    if current.stop_requested&&!state.terminal(){let _=self.interrupt(&current).await;}
                    if state==RequestState::ApprovalRequired{let _=self.discover_approvals(&current).await;}
                }
            }}
        }
    }
    pub async fn delivery_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{
                if self.recovery.load(Ordering::SeqCst)||!self.connected.load(Ordering::SeqCst){continue;}
                let _s=self.settings().await;
                let silent:Vec<_>=self.output.lock().await.iter().filter(|(_,o)|!o.done&&o.last_progress.elapsed()>Duration::from_secs(120)).map(|(id,o)|(format!("progress-{id}"),o.thread.clone())).collect();
                for(id,t)in silent{self.notice(id,t,"この案内の時点で120秒以上、新しい回答テキストを受信していません。作業停止とは断定できません。/status と /stop を利用できます。").await?;}
                let snapshots:Vec<_>=self.output.lock().await.iter().map(|(id,o)|(id.clone(),o.thread.clone(),o.text.clone(),o.done,o.lost)).collect();
                for(id,thread,text,done,lost)in snapshots{
                    let mut confirmed=true;
                    for(i,part)in chunks(&text).iter().enumerate(){if !self.delivery.text(&id,&thread,"answer",i as i64,part,json!([])).await?{confirmed=false;break;}}
                    if done&&confirmed&& (!lost || self.delivery.text(&id,&thread,"delivery",0,"回答表示に欠落があります。再実行はしていません。",json!([])).await?){self.output.lock().await.remove(&id);let rid=id.clone();self.store.call(false,move|c|{c.execute("UPDATE output_state SET state='DELIVERED' WHERE request_id=?1",[rid])?;Ok(())}).await?;}
                }
                // State cards remain recoverable without retaining answer text.
                let rows=self.store.call(false,|c|{let mut st=c.prepare("SELECT id,thread_id,state FROM requests ORDER BY updated_at DESC LIMIT 20")?;Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
                for(id,thread,state)in rows {
                    let message=match state.as_str(){
                        "FAILED"=>"作業を完了できませんでした。/status で確認してください。",
                        "CANCELLED"=>"作業を中断しました。",
                        "CANCEL_REQUESTED"=>"中断を要求しました。停止の確認を待っています。",
                        "UNKNOWN"=>"作業の状態を確認できません。再実行せず保留しています。/status で確認してください。",
                        _=>continue,
                    };
                    let _=self.delivery.text(&id,&thread,"status",0,message,json!([])).await?;
                }
                let expired:Vec<_>=self.output.lock().await.iter().filter(|(_,o)|o.done&&o.created.elapsed()>=o.retention).map(|(id,_)|id.clone()).collect();
                for id in expired{self.output.lock().await.remove(&id);self.store.call(false,move|c|{c.execute("UPDATE output_state SET state='UNAVAILABLE' WHERE request_id=?1",[id])?;Ok(())}).await?;}
                let lost=self.store.call(false,|c|{let mut st=c.prepare("SELECT o.request_id,r.thread_id FROM output_state o JOIN requests r ON r.id=o.request_id WHERE o.state='UNAVAILABLE' ORDER BY r.updated_at DESC LIMIT 20")?;Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
                for(id,t)in lost{let _=self.delivery.text(&id,&t,"output_unavailable",0,"再起動または配信失敗により、回答全文を再取得できません。実行状態は別途確認します。自動再実行はしません。",json!([])).await?;}
                let notices=self.store.call(false,|c|{let mut st=c.prepare("SELECT id,thread_id,code FROM notices UNION ALL SELECT request_id,thread_id,'入力を確認できなかったため、実行していません。' FROM admissions WHERE status='REJECTED' LIMIT 50")?;Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
                for(id,t,text)in notices{let _=self.delivery.text(&id,&t,"notice",0,&text,json!([])).await?;}
                self.delivery.recover().await?;
            }}
        }
    }
    pub async fn discover_approvals(&self, r: &Request) -> Result<()> {
        let s = self.settings().await;
        let turn = r.turn_id.as_deref().context("Turn unknown")?;
        let list = s
            .proxy
            .get(&format!("/v1/codex/turns/{}/approvals", path_id(turn)?))
            .await?;
        for a in list["data"].as_array().into_iter().flatten() {
            let aid = a["approval_id"].as_str().context("approval ID missing")?;
            let v = s
                .proxy
                .get(&format!("/v1/codex/approvals/{}", path_id(aid)?))
                .await?;
            ensure!(
                v["details"]["threadId"].as_str() == r.proxy_thread_id.as_deref()
                    && v["details"]["turnId"].as_str() == Some(turn),
                "approval target mismatch"
            );
            let (rid, aid_owned, thread, turn_owned, expires) = (
                r.id.clone(),
                aid.to_owned(),
                r.proxy_thread_id.clone().unwrap(),
                turn.to_owned(),
                v["expires_at_ms"].as_i64(),
            );
            self.store.call(true,move|c|{c.execute("INSERT OR IGNORE INTO approvals(id,request_id,thread_id,turn_id,state,expires_at) VALUES(?1,?2,?3,?4,'PENDING',?5)",params![aid_owned,rid,thread,turn_owned,expires])?;Ok(())}).await?;
            let mut buttons = vec![];
            for (decision, label, style) in [
                ("accept", "今回のみ承認", 3),
                ("decline", "拒否", 4),
                ("cancel", "取消", 2),
            ] {
                if v["available_decisions"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|d| d == decision))
                {
                    let custom = format!("approval:{aid}:{decision}");
                    ensure!(custom.len() <= 100, "approval component ID exceeds limit");
                    buttons.push(json!({"type":2,"style":style,"label":label,"custom_id":custom}));
                }
            }
            let details = self.redact(&s, &serde_json::to_string(&v["details"])?);
            let snippet = details.chars().take(1200).collect::<String>();
            let text = format!(
                "依頼 {} の承認待ちです。\n{}\n期限切れ・対象変更後の操作は拒否します。",
                &r.id[..8],
                snippet
            );
            let components = if buttons.is_empty() {
                json!([])
            } else {
                json!([{"type":1,"components":buttons}])
            };
            self.delivery
                .text(aid, &r.thread_id, "approval", 0, &text, components)
                .await?;
        }
        Ok(())
    }
    pub async fn notice(&self, id: String, thread: String, text: &'static str) -> Result<()> {
        self.store
            .call(false, move |c| {
                c.execute(
                    "INSERT OR IGNORE INTO notices VALUES(?1,?2,?3,?4)",
                    params![id, thread, text, domain::now_ms()],
                )?;
                Ok(())
            })
            .await
    }
    pub async fn interrupt(&self, r: &Request) -> Result<()> {
        if self.reloading.load(Ordering::SeqCst) || self.recovery.load(Ordering::SeqCst) {
            return Ok(());
        }
        let Some(turn) = &r.turn_id else {
            return Ok(());
        };
        let s = self.settings().await;
        let op = format!("interrupt-{}", r.id);
        let (op2, rid, turn2, thread) = (
            op.clone(),
            r.id.clone(),
            turn.clone(),
            r.proxy_thread_id.clone(),
        );
        let send=self.store.call(true,move|c|{let tx=c.transaction()?;let active:bool=tx.query_row("SELECT stop_requested=1 AND state NOT IN ('COMPLETED','FAILED','CANCELLED') AND turn_id=?2 FROM requests WHERE id=?1",params![rid,turn2],|r|r.get(0))?;if !active{return Ok(false)};let changed=tx.execute("INSERT OR IGNORE INTO operations(id,kind,target_request_id,target_turn_id,target_thread_id,state,send_state,created_at) VALUES(?1,'interrupt',?2,?3,?4,'SENDING','SENDING',?5)",params![op2,rid,turn2,thread,domain::now_ms()])?;tx.commit()?;Ok(changed==1)}).await?;
        if !send {
            return Ok(());
        }
        let result = s
            .proxy
            .control(
                &format!("/v1/codex/turns/{}/interrupt", path_id(turn)?),
                None,
            )
            .await;
        self.finish_operation(op, result.is_ok()).await?;
        if result.is_ok() {
            self.store
                .observe(
                    r.id.clone(),
                    RequestState::CancelRequested,
                    "interrupt_accepted_not_confirmed",
                    false,
                )
                .await?;
        }
        Ok(())
    }
    pub async fn finish_operation(&self, id: String, accepted: bool) -> Result<()> {
        self.store
            .call(true, move |c| {
                c.execute(
                    "UPDATE operations SET state=?2,send_state=?3 WHERE id=?1",
                    params![
                        id,
                        if accepted { "ACCEPTED" } else { "UNKNOWN" },
                        if accepted { "WRITTEN" } else { "UNKNOWN" }
                    ],
                )?;
                Ok(())
            })
            .await
    }
}
