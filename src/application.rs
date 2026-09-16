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
    pub config_mutation: Arc<Mutex<()>>,
    pub(crate) resource_mutation: Arc<RwLock<()>>,
    pub(crate) pending_projects: Arc<Mutex<HashMap<String, crate::projects::PendingProject>>>,
    pub(crate) mcp_scan_lock: Arc<Mutex<()>>,
    pub(crate) mcp_drafts: Arc<Mutex<HashMap<String, crate::mcp_ui::Draft>>>,
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
    dispatching: Arc<std::sync::Mutex<HashSet<String>>>,
}
struct DispatchGuard {
    ids: Arc<std::sync::Mutex<HashSet<String>>>,
    id: String,
}
impl Drop for DispatchGuard {
    fn drop(&mut self) {
        self.ids.lock().unwrap().remove(&self.id);
    }
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
        let cfg = cfg.with_registered_projects(&store.path)?;
        let workspaces = cfg.validate()?;
        Ok(Self {
            mcp_scan_lock: Arc::new(Mutex::new(())),
            mcp_drafts: Arc::new(Mutex::new(HashMap::new())),
            config_mutation: Arc::new(Mutex::new(())),
            resource_mutation: Arc::new(RwLock::new(())),
            pending_projects: Arc::new(Mutex::new(HashMap::new())),
            settings: Arc::new(RwLock::new(Settings {
                cfg,
                revision: 1,
                workspaces,
                proxy: proxy.with_store(store.clone()),
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
            dispatching: Arc::new(std::sync::Mutex::new(HashSet::new())),
        })
    }
    pub async fn settings(&self) -> Settings {
        self.settings.read().await.clone()
    }
    async fn verify_location(&self, channel: &str) -> Result<()> {
        let s = self.settings().await;
        let v = self
            .discord
            .get(&format!("/channels/{}", snowflake(channel)?))
            .await?;
        ensure!(
            v["id"] == channel && v["guild_id"] == s.cfg.discord.guild_id,
            "channel identity mismatch"
        );
        match v["type"].as_u64() {
            Some(0) => {}
            Some(11 | 12) => {
                ensure!(
                    v["thread_metadata"]["archived"] != true
                        && v["thread_metadata"]["locked"] != true,
                    "conversation closed"
                );
                let parent = v["parent_id"].as_str().context("parent missing")?;
                let p = self
                    .discord
                    .get(&format!("/channels/{}", snowflake(parent)?))
                    .await?;
                ensure!(
                    p["id"] == parent
                        && p["guild_id"] == s.cfg.discord.guild_id
                        && matches!(p["type"].as_u64(), Some(0 | 15)),
                    "parent mismatch"
                );
            }
            _ => anyhow::bail!("unsupported conversation location"),
        }
        Ok(())
    }
    pub async fn authorized_thread(&self, thread: &str) -> Result<crate::domain::Conversation> {
        self.verify_location(thread).await?;
        self.store.conversation(thread).await
    }
    pub async fn ensure_channel_conversation(&self, channel: &str) -> Result<bool> {
        self.verify_location(channel).await?;
        self.store
            .add_conversation(channel.into(), crate::storage::PROXY_SCOPE.into())
            .await?;
        Ok(true)
    }
    pub async fn admit_loop(&self, mut rx: mpsc::Receiver<Incoming>) -> Result<()> {
        let mut workers = JoinSet::new();
        let mut controls = JoinSet::new();
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>{workers.shutdown().await;controls.shutdown().await;return Ok(())},
                result=controls.join_next(),if !controls.is_empty()=>{if result.is_some_and(|r|r.is_err()){tracing::warn!(event="text_control_worker_lost");}},
                result=workers.join_next(),if !workers.is_empty()=>{if result.is_some_and(|r|r.is_err()){tracing::warn!(event="validation_worker_lost");}},
                event=rx.recv()=>{
                    let Some(Incoming::Message(v))=event else{ensure!(!rx.is_closed(),"Discord admission channel closed");continue};
                    let s=self.settings().await;
                    if v["guild_id"]!=s.cfg.discord.guild_id||v["author"]["id"]!=s.cfg.discord.allowed_user_id||v["author"]["bot"]==true||!v["webhook_id"].is_null(){continue;}
                    if crate::commands::is_text_control(v["content"].as_str().unwrap_or("")) {
                        let (Some(channel),Some(id))=(v["channel_id"].as_str(),v["id"].as_str()) else {continue};
                        if !self.store.admissible_event(id.into()).await? {continue;}
                        if controls.len()>=8 {
                            self.notice(id.into(),channel.into(),"操作が混み合っています。少し待ってから操作してください。AIには送っていません。").await?;
                            continue;
                        }
                        let app=self.clone();let channel=channel.to_owned();let id=id.to_owned();
                        controls.spawn(async move {
                            let result=tokio::time::timeout(Duration::from_secs(45),app.text_control_command(&v)).await;
                            let text=match result {Ok(Ok(text))=>text,_=>"操作の適用を確認できませんでした。/model または /status で確認してください。AIには送っていません。".into()};
                            if let Some(components) = app.project_menu(&v).await {
                                app.discord.api(reqwest::Method::POST,&format!("/channels/{}/messages",snowflake(&channel)?),Some(serde_json::json!({"content":text,"components":components,"allowed_mentions":{"parse":[]}}))).await?;
                                Ok(())
                            } else { app.notice(id,channel,&text).await }
                        });
                        continue;
                    }
                    if !self.discord.should_respond(&v,s.cfg.discord.response_mode){continue;}
                    let (Some(channel),Some(message))=(v["channel_id"].as_str(),v["id"].as_str()) else {continue};
                    if !self.ensure_channel_conversation(channel).await.unwrap_or(false) {
                        self.notice(message.into(),channel.into(),"この場所では会話を開始できません。通常チャンネルか開いているスレッドで、Botの閲覧権限を確認してください。").await?;
                        continue;
                    }
                    if self.store.conversation(channel).await?.selected_model.is_empty() {
                        self.notice(message.into(),channel.into(),"初期モデルを選んでください。/model を開くと選択メニューが表示されます。cwd登録は不要です。選択後にもう一度話しかけてください。").await?;
                        continue;
                    }
                    if self.reloading.load(Ordering::SeqCst)||self.recovery.load(Ordering::SeqCst)||!s.proxy.gate.is_ready()||!self.connected.load(Ordering::SeqCst){if let (Some(t),Some(id))=(v["channel_id"].as_str(),v["id"].as_str()) && self.store.conversation(t).await.is_ok(){self.notice(id.into(),t.into(),"受付停止中です。/status で状態を確認してください。").await?;}continue;}
                    let m=match input_message(&v,&s.cfg.discord.guild_id){Ok(m)=>m,Err(_)=>continue};
                    if workers.len()>=s.cfg.limits.queue_global{continue;}
                    // The Store transaction is the acceptance ordering boundary, before network validation.
                    let id=match self.store.reserve(m.id.clone(),m.thread_id.clone(),m.metadata_digest(),s.cfg.limits.clone()).await{Ok(Some(id))=>id,Ok(None)=>continue,Err(_)=>{if self.store.conversation(&m.thread_id).await.is_ok(){self.notice(m.id.clone(),m.thread_id.clone(),"受付できませんでした。待機上限または会話の安全確認が必要な状態です。/status で確認できます。この投稿はAIへ送信していません。").await?;}continue}};
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
            }
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
        let conversation = s
            .proxy
            .ensure_conversation_v2(&self.store, &r.thread_id)
            .await?;
        let permit = s.proxy.authorize(r.id.clone(), s.revision).await?;
        self.prepare_mcp_context(&r, &s).await?;
        self.dispatching.lock().unwrap().insert(r.id.clone());
        let _dispatch = DispatchGuard {
            ids: self.dispatching.clone(),
            id: r.id.clone(),
        };
        let r = self
            .store
            .begin_send_authorized(r.id.clone(), permit)
            .await?;
        if self.store.request(&r.id).await?.stop_requested {
            s.proxy.stop_v2(&self.store, &r).await?;
            s.proxy.reconcile(&self.store, &r.id).await?;
            return Ok(());
        }
        let result = s.proxy.start_v2(&r, &conversation, p.input).await;
        drop(p.reservation);
        if let Err(error) = &result
            && error
                .downcast_ref::<crate::proxy_v2::ApiError>()
                .is_some_and(|e| {
                    e.status == 409
                        && matches!(
                            e.code.as_str(),
                            "provider_busy" | "workspace_busy" | "conversation_busy"
                        )
                })
        {
            self.store
                .observe(r.id.clone(), RequestState::Failed, "proxy_busy", true)
                .await?;
            return Ok(());
        }
        if let Ok(value) = &result {
            ensure!(
                value["resource"]["type"] == "response",
                "execution receipt mismatch"
            );
            let response = crate::proxy::field(&value["resource"], "id")?;
            let id = r.id.clone();
            self.store
                .call(true, move |c| {
                    c.execute(
                        "UPDATE requests SET response_id=?2 WHERE id=?1 AND response_id IS NULL",
                        params![id, response],
                    )?;
                    Ok(())
                })
                .await?;
        }
        s.proxy.reconcile(&self.store, &r.id).await?;
        result?;
        Ok(())
    }
    pub fn secrets(&self, s: &Settings) -> Vec<String> {
        let mut v = vec![self.discord.secret(), s.proxy.secret()];
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
                    if self.dispatching.lock().unwrap().contains(&r.id) { continue; }
                        if jobs.len()>=2{break;}
                        if r.response_id.is_none()||r.state.terminal()||active.contains(&r.id)||attempts.get(&r.id).is_some_and(|t|t.elapsed()<Duration::from_secs(5)){continue;}
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
                .monitor_v2(r.response_id.as_deref().context("Response missing")?),
        )
        .await??;
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        let mut last_byte = Instant::now();
        let mut redactor = Redactor::new(self.secrets(&s));
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>return Ok(()),
                _=tick.tick()=>{if self.store.request(&r.id).await?.state.terminal(){return Ok(())}ensure!(last_byte.elapsed()<Duration::from_secs(90),"Turn monitor silent");},
                chunk=stream.next()=>{let Some(chunk)=chunk else{return Ok(())};last_byte=Instant::now();for event in decoder.feed(&chunk?)?{
                    match event.event.as_str(){
                        "snapshot"|"gap"|"response.execution_terminal"|"response.output_ready"|"response.output_failed"=>{s.proxy.reconcile(&self.store,&r.id).await?;},
                        "response.generated_images_changed"=>{let id=r.id.clone();self.store.call(false,move|c|{c.execute("UPDATE generated_image_watches SET next_poll_ms=0 WHERE request_id=?1 AND state='WATCHING'",[id])?;Ok(())}).await?;},
                        "response.delta"=>{
                            let v:Value=serde_json::from_str(&event.data)?;
                            ensure!(v["response_id"].as_str()==r.response_id.as_deref(),"delta target mismatch");
                            if let Some(delta)=v["delta"].as_str(){
                                let delta=redactor.push(delta);
                                let mut cache=self.output.lock().await;
                                let total:usize=cache.values().map(|o|o.text.len()).sum();
                                if total+delta.len()<=s.cfg.limits.output_total_bytes {
                                    let o=cache.entry(r.id.clone()).or_insert_with(||Output{thread:r.thread_id.clone(),text:String::new(),done:false,lost:false,created:Instant::now(),last_progress:Instant::now(),retention:Duration::from_secs(s.cfg.limits.delivery_retention_secs)});
                                    if !o.done&&o.text.len()+delta.len()<=s.cfg.limits.output_bytes {o.text.push_str(&delta);o.last_progress=Instant::now();}
                                }
                            }
                        },
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
                    if self.dispatching.lock().unwrap().contains(&r.id) { continue; }
                    let state=s.proxy.reconcile(&self.store,&r.id).await?;
                    let current=self.store.request(&r.id).await?;
                    if current.stop_requested&&!state.terminal(){let _=self.interrupt(&current).await;}
                    if matches!(state,RequestState::Running|RequestState::ApprovalRequired)&&current.proxy_thread_id.is_some(){let _=self.discover_approvals(&current).await;}
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
                    let _guard=self.resource_mutation.read().await;
                    if !self.output.lock().await.contains_key(&id){continue;}
                    let mut confirmed=true;
                    for(i,part)in chunks(&text).iter().enumerate(){if !self.delivery.text(&id,&thread,if done {"answer"} else {"draft"},i as i64,part,json!([])).await?{confirmed=false;break;}}
                    if done&&confirmed {confirmed=self.delivery.trim_answer(&id,&thread,chunks(&text).len()).await?; if confirmed {confirmed=self.delivery.clear_draft(&id,&thread).await?;}}
                    if done&&confirmed&& (!lost || self.delivery.text(&id,&thread,"delivery",0,"回答表示に欠落があります。再実行はしていません。",json!([])).await?){self.output.lock().await.remove(&id);let rid=id.clone();self.store.call(false,move|c|{c.execute("UPDATE output_state SET state='DELIVERED' WHERE request_id=?1",[&rid])?;c.execute("UPDATE resource_deliveries SET state='RELEASE_PENDING' WHERE id=?1",[rid])?;Ok(())}).await?;}
                }
                // State cards remain recoverable without retaining answer text.
                let rows=self.store.call(false,|c|{let mut st=c.prepare("SELECT id,thread_id,state,error_code,EXISTS(SELECT 1 FROM deliveries d WHERE d.target_id=requests.id AND d.kind='status'),stop_requested,EXISTS(SELECT 1 FROM mcp_interactions m WHERE m.request_id=requests.id AND m.action='decline' AND m.operation_state='succeeded') FROM requests ORDER BY updated_at DESC LIMIT 20")?;Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,bool>(4)?,r.get::<_,bool>(5)?,r.get::<_,bool>(6)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
                for(id,thread,state,error,had_status,stopped,declined)in rows {
                    if had_status && matches!(state.as_str(), "COMPLETED"|"RUNNING"|"APPROVAL_REQUIRED") {
                        let _=self.delivery.clear_status(&id,&thread).await?;
                        continue;
                    }
                    let message=match state.as_str(){
                        "FAILED"=>failure_message(error.as_deref()),
                        "CANCELLED"=>interruption_with_evidence(error.as_deref(),stopped,declined),
                        "CANCEL_REQUESTED"=>"中断を要求しました。停止の確認を待っています。",
                        "UNKNOWN"=>"作業の状態を確認できません。再実行せず保留しています。/status で確認してください。",
                        _=>continue,
                    };
                    let _=self.delivery.text(&id,&thread,"status",0,message,json!([])).await?;
                }
                let expired:Vec<_>=self.output.lock().await.iter().filter(|(_,o)|o.done&&o.created.elapsed()>=o.retention).map(|(id,_)|id.clone()).collect();
                for id in expired{self.output.lock().await.remove(&id);self.store.call(false,move|c|{c.execute("UPDATE output_state SET state='UNAVAILABLE' WHERE request_id=?1 AND NOT EXISTS(SELECT 1 FROM resource_deliveries WHERE id=?1 AND resource_type='response_output')",[id])?;Ok(())}).await?;}
                let lost=self.store.call(false,|c|{let mut st=c.prepare("SELECT o.request_id,r.thread_id FROM output_state o JOIN requests r ON r.id=o.request_id WHERE o.state='UNAVAILABLE' ORDER BY r.updated_at DESC LIMIT 20")?;Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
                for(id,t)in lost{let _=self.delivery.text(&id,&t,"output_unavailable",0,"再起動または配信失敗により、回答全文を再取得できません。実行状態は別途確認します。自動再実行はしません。",json!([])).await?;}
                let notices=self.store.call(false,|c|{let mut st=c.prepare("SELECT id,thread_id,code FROM notices UNION ALL SELECT request_id,thread_id,'入力を確認できなかったため、実行していません。' FROM admissions WHERE status='REJECTED' LIMIT 50")?;Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
                for(id,t,text)in notices{if id.starts_with("resource-error-"){self.resource_notice(&id,&t).await?;}else{let _=self.delivery.text(&id,&t,"notice",0,&text,json!([])).await?;}}
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
        }
        Ok(())
    }
    pub async fn notice(&self, id: String, thread: String, text: &str) -> Result<()> {
        let text = text.to_owned();
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
        let value = self.settings().await.proxy.stop_v2(&self.store, r).await?;
        let next = match value["stop_status"].as_str() {
            Some("cancelled_before_start" | "interrupted") => RequestState::Cancelled,
            Some("waiting_for_start" | "interrupt_pending") => RequestState::CancelRequested,
            _ => return Ok(()),
        };
        self.store
            .observe(r.id.clone(), next, "v2_stop_observed", true)
            .await
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

pub fn failure_message(code: Option<&str>) -> &'static str {
    match code {
        Some(
            "unsupported_interaction"
            | "unsupported_interaction_schema"
            | "interaction_expired"
            | "interaction_events_lost"
            | "interaction_connection_lost",
        ) => interruption_message(code),
        Some("proxy_busy") => {
            "Proxyが混み合っていたため開始できませんでした。少し待って、新しいメッセージとして依頼してください。この依頼は自動再送しません。"
        }
        Some("proxy_capacity") => {
            "Proxyの保存容量が不足しており開始できませんでした。空き容量を確認してください。"
        }
        Some("workspace_access_revoked") => {
            "Proxyが作業先へのアクセスを拒否しました。作業先の権限を確認してください。"
        }
        Some("proxy_invalid_cwd") => {
            "Proxyが作業フォルダーを拒否したため、実行を開始していません。Proxy側の作業先の許可設定を確認してください。自動再実行はしません。"
        }
        Some("proxy_start_rejected") => {
            "Proxyが依頼を開始前に拒否しました。接続・モデル・作業先の設定を確認してください。自動再実行はしません。"
        }
        _ => "作業を完了できませんでした。/status で確認してください。",
    }
}

pub fn interruption_message(code: Option<&str>) -> &'static str {
    match code {
        Some("unsupported_interaction" | "unsupported_interaction_schema") => {
            "必要な承認・入力画面に未対応のため実行できませんでした。利用者による拒否ではありません。"
        }
        Some("interaction_expired") => {
            "承認・入力の期限が切れたため停止しました。利用者による拒否ではありません。"
        }
        Some("interaction_events_lost" | "interaction_connection_lost") => {
            "実行側の接続・対話情報を失ったため停止しました。利用者による拒否ではありません。"
        }
        Some("user_stop" | "v2_stop_observed") => "停止操作により作業を中断しました。",
        Some("mcp_user_declined") => {
            "作業は中断しています。この依頼では、利用者によるMCP要求の拒否を送信済みです。"
        }
        _ => {
            "作業は中断されていますが、理由を確定できません。利用者の拒否・停止とは断定していません。/status で確認してください。"
        }
    }
}

pub fn interruption_with_evidence(
    code: Option<&str>,
    stopped: bool,
    declined: bool,
) -> &'static str {
    if matches!(
        code,
        Some(
            "unsupported_interaction"
                | "unsupported_interaction_schema"
                | "interaction_expired"
                | "interaction_events_lost"
                | "interaction_connection_lost"
        )
    ) {
        return interruption_message(code);
    }
    if stopped {
        return interruption_message(Some("user_stop"));
    }
    if declined {
        return interruption_message(Some("mcp_user_declined"));
    }
    interruption_message(code)
}
