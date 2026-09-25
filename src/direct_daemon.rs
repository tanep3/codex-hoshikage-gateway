//! Discord-facing owner for the Gateway's direct Codex runtime.
//! The legacy Proxy daemon remains a separate, explicit CLI mode during cutover.
use crate::{
    codex_transport::CodexRuntimePool,
    config::secret,
    delivery::Delivery,
    direct_application::{DirectAdmission, DirectApplication, DirectControlOutcome},
    direct_approval::{DirectInteraction, InteractionKind, ManualDecision},
    direct_config::DirectConfig,
    direct_content::DirectContent,
    direct_models::{DirectModel, DirectModelCatalog, DirectReasoningEffort},
    direct_run::DirectRunService,
    direct_run_actor::{self, RunCommand, RunEvent},
    discord::{Discord, Handler, Health, Incoming, snowflake},
    domain::RequestState,
    files::Files,
    storage::{self, Store},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

fn input_rejection_message(error: &anyhow::Error, limits: &crate::config::Limits) -> String {
    let reason = error.to_string();
    let mib = |bytes: usize| format!("{:.1} MiB", bytes as f64 / 1024.0 / 1024.0);
    if reason.contains("too many attachments") {
        return format!(
            "添付ファイルは1回につき{}件までです。件数を減らして新しい投稿で送り直してください。AIへは送信していません。",
            limits.attachments
        );
    }
    if reason.contains("attachment too large") || reason.contains("attachment byte limit") {
        return format!(
            "添付ファイル1件の上限は{}です。ファイルを小さくするか分割して、新しい投稿で送り直してください。AIへは送信していません。",
            mib(limits.attachment_bytes)
        );
    }
    if reason.contains("total input limit") || reason.contains("encoded request size exceeded") {
        return format!(
            "本文と添付の合計が1回の上限{}を超えています。添付を減らすか分割して、新しい投稿で送り直してください。AIへは送信していません。",
            mib(limits.input_bytes)
        );
    }
    if reason.contains("text too large") {
        return format!(
            "本文が上限（{} KiB）を超えています。短くするか複数の投稿に分けてください。AIへは送信していません。",
            limits.text_bytes / 1024
        );
    }
    if reason.contains("attachment fetch")
        || reason.contains("attachment unavailable")
        || reason.contains("attachment read")
    {
        return "添付ファイルをDiscordから取得できませんでした。ファイルを添付し直し、新しい投稿で送り直してください。AIへは送信していません。".into();
    }
    if reason.contains("image pixel limit")
        || reason.contains("unsupported image")
        || reason.contains("animated image")
    {
        return "この画像形式または画像サイズには対応できません。PNG・JPEG・WebPの静止画像へ変換して、新しい投稿で送り直してください。AIへは送信していません。".into();
    }
    "受付できませんでした。本文や添付を確認して、新しい投稿で送り直してください。解決しない場合は /status の内容を運用者へ伝えてください。AIへは送信していません。".into()
}

#[derive(Clone)]
struct LiveRun {
    request_id: String,
    commands: mpsc::Sender<RunCommand>,
}
#[derive(Clone)]
struct ApprovalCard {
    request_id: String,
    thread_id: String,
    fingerprint: String,
    operation: DirectInteraction,
    input_generation: u64,
    highest_viewed_page: Option<usize>,
}
#[derive(Clone)]
pub struct DirectCoordinator {
    app: DirectApplication,
    active: Arc<Mutex<HashMap<String, LiveRun>>>,
    approvals: Arc<Mutex<HashMap<String, ApprovalCard>>>,
    cancel: CancellationToken,
    connected: Arc<AtomicBool>,
}

fn model_menu(models: &[DirectModel], current: &str, interaction: &str) -> (String, Value) {
    let choices: Vec<Value> = models
        .iter()
        .filter(|model| model.id.len() <= 100)
        .take(25)
        .map(|model| {
            json!({
                "label": model.id,
                "value": model.id,
                "description": model.display_name.chars().take(100).collect::<String>(),
                "default": model.id == current,
            })
        })
        .collect();
    let omitted = models.len().saturating_sub(choices.len());
    let note = if omitted == 0 {
        String::new()
    } else {
        format!(
            "\n一覧に収まらない残り{omitted}件は /models でIDを確認し、/model の id 欄に入力できます。"
        )
    };
    let text = format!("選択中のモデル: {current}\n次に使うモデルを選んでください。{note}");
    if choices.is_empty() {
        return (
            format!(
                "{text}\nこの画面に表示できる候補はありません。/models でIDを確認し、/model の id 欄に入力してください。"
            ),
            json!([]),
        );
    }
    (
        text,
        json!([{"type":1,"components":[{
            "type":3,
            "custom_id":format!("direct:model:{interaction}"),
            "placeholder":"モデルを選択",
            "min_values":1,
            "max_values":1,
            "options":choices,
        }]}]),
    )
}

fn effort_menu(
    efforts: &[DirectReasoningEffort],
    current: &str,
    interaction: &str,
) -> (String, Value) {
    let choices: Vec<Value> = efforts
        .iter()
        .filter(|effort| effort.id.len() <= 100)
        .take(25)
        .map(|effort| {
            json!({
                "label": effort.id,
                "value": effort.id,
                "description": effort.description.chars().take(100).collect::<String>(),
                "default": effort.id == current,
            })
        })
        .collect();
    let text = format!("選択中の推論レベル: {current}\n次に使う推論レベルを選んでください。");
    if choices.is_empty() {
        return (
            format!("{text}\n選択中のモデルで利用可能な候補を取得できませんでした。"),
            json!([]),
        );
    }
    (
        text,
        json!([{"type":1,"components":[{
            "type":3,
            "custom_id":format!("direct:effort:{interaction}"),
            "placeholder":"推論レベルを選択",
            "min_values":1,
            "max_values":1,
            "options":choices,
        }]}]),
    )
}

fn approval_detail(operation: &DirectInteraction) -> Result<Vec<String>> {
    let detail = if operation.kind == InteractionKind::McpElicitation
        && operation
            .mcp_tool_decision(ManualDecision::AcceptOnce)
            .is_ok()
    {
        let args = serde_json::to_string_pretty(&operation.params["_meta"]["tool_params"])?;
        let tool = operation
            .mcp_evidence
            .as_ref()
            .and_then(|item| item["tool"].as_str())
            .or_else(|| operation.params["message"].as_str())
            .unwrap_or("ツール名を確認できません");
        format!(
            "MCPツール実行の確認（本人限定）\nサーバー: {}\nツール: {tool}\n実引数（Codexからの原文）:\n{args}\n「今回だけ許可」は、この操作1件だけです。",
            operation.params["serverName"].as_str().unwrap_or("不明"),
        )
    } else {
        let rendered = serde_json::to_string_pretty(&json!({
            "upstream_request":operation.params,
            "matched_mcp_call":operation.mcp_evidence
        }))?;
        format!(
            "Codexが求めた操作の実引数（本人限定）:\n{rendered}\n内容を確認して選んでください。"
        )
    };
    let detail = if operation.run_grant_tool().is_some() {
        format!(
            "{detail}\n「この依頼中、このツールを許可」は、同じ依頼内の同じサーバー／ツールに限り、引数が変わる後続呼出しも許可します。追加指示・停止・依頼終了で失効します。"
        )
    } else {
        detail
    };
    // Discord's message content is limited to 2000 characters. Preserve the
    // entire bounded upstream request across private pages; count UTF-16 units
    // so astral characters cannot make a page exceed the client limit.
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for character in detail.chars() {
        let escaped = match character {
            '\n' => "\n".to_owned(),
            value
                if value.is_control()
                    || matches!(value, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
            {
                format!("\\u{{{:x}}}", value as u32)
            }
            value if "\\`*_{}[]()#+-.!|>~<@".contains(value) => format!("\\{value}"),
            value => value.to_string(),
        };
        let width = escaped.encode_utf16().count();
        if units + width > 1750 {
            chunks.push(std::mem::take(&mut chunk));
            units = 0;
        }
        chunk.push_str(&escaped);
        units += width;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    ensure!(!chunks.is_empty(), "approval detail empty");
    let total = chunks.len();
    let pages = chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            format!(
                "操作詳細 {}/{}ページ（本人限定）\n{chunk}",
                index + 1,
                total
            )
        })
        .collect::<Vec<_>>();
    ensure!(
        pages.iter().all(|page| page.encode_utf16().count() <= 1900),
        "approval page too long"
    );
    Ok(pages)
}

impl DirectCoordinator {
    fn answerable(operation: &DirectInteraction) -> bool {
        if matches!(
            operation.kind,
            InteractionKind::CommandApproval | InteractionKind::FileChangeApproval
        ) {
            operation
                .manual_decision(ManualDecision::AcceptOnce)
                .is_ok()
        } else {
            operation
                .mcp_tool_decision(ManualDecision::AcceptOnce)
                .is_ok()
        }
    }
    fn declineable(operation: &DirectInteraction) -> bool {
        if matches!(
            operation.kind,
            InteractionKind::CommandApproval | InteractionKind::FileChangeApproval
        ) {
            operation.manual_decision(ManualDecision::Decline).is_ok()
        } else {
            operation.mcp_tool_decision(ManualDecision::Decline).is_ok()
        }
    }
    fn cancelable(operation: &DirectInteraction) -> bool {
        matches!(
            operation.kind,
            InteractionKind::CommandApproval | InteractionKind::FileChangeApproval
        ) && operation.manual_decision(ManualDecision::Cancel).is_ok()
    }
    fn rejection_button(operation: &DirectInteraction, id: &str) -> Value {
        if Self::declineable(operation) {
            json!({"type":2,"style":4,"label":"拒否","custom_id":format!("direct:decline:{id}")})
        } else if Self::cancelable(operation) {
            json!({"type":2,"style":4,"label":"取り消し","custom_id":format!("direct:cancel:{id}")})
        } else {
            json!({"type":2,"style":4,"label":"未対応の確認を終了","custom_id":format!("direct:reject:{id}")})
        }
    }
    fn new(app: DirectApplication) -> Self {
        Self {
            app,
            active: Arc::new(Mutex::new(HashMap::new())),
            approvals: Arc::new(Mutex::new(HashMap::new())),
            cancel: CancellationToken::new(),
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn notice(&self, target: &str, thread: &str, text: &str) -> Result<()> {
        self.app
            .delivery
            .text(target, thread, "direct-notice", 0, text, json!([]))
            .await?;
        Ok(())
    }

    async fn admit_loop(&self, mut input: mpsc::Receiver<Incoming>) -> Result<()> {
        let mut workers = JoinSet::new();
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => { workers.shutdown().await; return Ok(()); }
                joined = workers.join_next(), if !workers.is_empty() => {
                    match joined {
                        Some(Ok(Ok(()))) => {}
                        Some(Ok(Err(_))) | Some(Err(_)) =>
                            anyhow::bail!("direct admission worker exited unexpectedly"),
                        None => {}
                    }
                }
                event = input.recv() => {
                    let Some(Incoming::Message(message)) = event else {
                        ensure!(!input.is_closed(), "Discord admission channel closed");
                        continue;
                    };
                    if !self.connected.load(Ordering::Acquire) { continue; }
                    if workers.len() >= self.app.cfg.limits.queue_global {
                        if message["guild_id"] == self.app.cfg.discord.guild_id
                            && message["author"]["id"] == self.app.cfg.discord.allowed_user_id
                            && let (Some(thread), Some(id)) =
                                (message["channel_id"].as_str(), message["id"].as_str())
                        {
                            let _ = self
                                .notice(
                                    id,
                                    thread,
                                    "受付処理が混み合っています。この投稿はAIへ送っていません。少し待ってから新しい投稿で依頼してください。",
                                )
                                .await;
                        }
                        continue;
                    }
                    let this = self.clone();
                    workers.spawn(async move {
                        let thread = message["channel_id"].as_str().map(str::to_owned);
                        let message_id = message["id"].as_str().map(str::to_owned);
                        match this.app.admit_message(&message).await {
                            Ok(DirectAdmission::Accepted(id)) => {
                                let request = this.app.store.request(&id).await?;
                                let conversation = this.app.store.conversation(&request.thread_id).await?;
                                if this.app.store.direct_unknown_blocker(&request.thread_id).await? {
                                    this.notice(&id, &request.thread_id, "前の作業の結果を確認できず、この依頼はまだCodexへ送っていません。/recover で内容を確認して会話を復旧できます。復旧時に未送信依頼は取り消されるため、必要な指示を改めて投稿してください。").await?;
                                } else if conversation.paused {
                                    this.notice(&id, &request.thread_id, "受け付けました。待機列は停止中です。/resume で再開、/cancel でこの待機依頼を取り消せます。").await?;
                                }
                            }
                            Ok(DirectAdmission::Ignored | DirectAdmission::Duplicate) => {}
                            Err(error) => {
                                tracing::warn!(event="direct_admission_failed", category=%error.to_string().split(':').next().unwrap_or("unknown"));
                                if message["guild_id"] == this.app.cfg.discord.guild_id
                                    && message["author"]["id"] == this.app.cfg.discord.allowed_user_id
                                    && let (Some(thread),Some(message_id))=(thread,message_id) {
                                    let notice=input_rejection_message(&error,&this.app.cfg.limits);
                                    let _=this.notice(&message_id,&thread,&notice).await;
                                }
                            }
                        }
                        Ok::<(),anyhow::Error>(())
                    });
                }
            }
        }
    }

    async fn scheduler_loop(&self) -> Result<()> {
        let mut jobs: JoinSet<(String, Result<()>)> = JoinSet::new();
        let mut busy = HashSet::new();
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=> { jobs.shutdown().await; return Ok(()); }
                joined=jobs.join_next(), if !jobs.is_empty()=> {
                    let Some(joined)=joined else {continue};
                    match joined {
                        Ok((id, result)) => {
                            busy.remove(&id);
                            if let Err(error)=result {
                                tracing::warn!(event="direct_request_task_failed", request_id=%id, category=%error.to_string().split(':').next().unwrap_or("unknown"));
                            }
                        }
                        Err(_) => anyhow::bail!("direct request actor panicked"),
                    }
                }
                _=tick.tick()=>{
                    if !self.connected.load(Ordering::Acquire) || jobs.len()>=2 {continue;}
                    for request in self.app.store.candidates().await? {
                        if jobs.len()>=2 {break;}
                        if !busy.insert(request.id.clone()) {continue;}
                        let this=self.clone();
                        jobs.spawn(async move {
                            let id=request.id;
                            let outcome=this.drive(&id).await;
                            (id,outcome)
                        });
                    }
                }
            }
        }
    }

    async fn typing_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(7));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{}}
            if !self.connected.load(Ordering::Acquire) {
                continue;
            }
            let approving = self
                .approvals
                .lock()
                .await
                .values()
                .map(|card| card.thread_id.clone())
                .collect::<HashSet<_>>();
            let threads = self
                .active
                .lock()
                .await
                .keys()
                .filter(|thread| !approving.contains(*thread))
                .cloned()
                .collect::<Vec<_>>();
            for thread in threads {
                if self.app.verify_location(&thread).await.is_err() {
                    continue;
                }
                let _ = tokio::time::timeout(
                    Duration::from_secs(3),
                    self.app.discord.api(
                        reqwest::Method::POST,
                        &format!("/channels/{}/typing", crate::discord::snowflake(&thread)?),
                        None,
                    ),
                )
                .await;
            }
        }
    }

    async fn drive(&self, request_id: &str) -> Result<()> {
        let run = match self.app.start_queued(request_id).await {
            Ok(run) => run,
            Err(error) => {
                let request = self.app.store.request(request_id).await?;
                if request.state == RequestState::Queued {
                    self.app
                        .store
                        .fail_direct_before_send(request.id.clone(), "direct_pre_send_failed")
                        .await?;
                    let _=self.notice(&request.id,&request.thread_id,"依頼の入力確認またはCodexの開始準備に失敗しました。この依頼をAIへ送信していません。内容と接続を確認して、新しい投稿で依頼してください。").await;
                }
                return Err(error);
            }
        };
        let thread = self.app.store.request(request_id).await?.thread_id;
        let mut actor = direct_run_actor::spawn(self.app.clone(), run);
        self.active.lock().await.insert(
            thread.clone(),
            LiveRun {
                request_id: request_id.into(),
                commands: actor.commands.clone(),
            },
        );
        let event_result=async {
        while let Some(event) = actor.events.recv().await {
            match event {
                RunEvent::Approval { interaction_id, operation, input_generation } => {
                    self.show_approval(request_id, &thread, interaction_id, *operation, input_generation).await?;
                }
                RunEvent::ApprovalResolved { interaction_id } => {
                    if let Some(card)=self.approvals.lock().await.remove(&interaction_id) {
                        // A stale Discord button must not remain usable after Codex resolves
                        // the request. Delivery failure affects only the display, not the Run.
                        let _=self.app.delivery.text(&interaction_id,&card.thread_id,"direct-approval",0,
                            "このMCP承認・入力は終了しました。",json!([])).await;
                    }
                }
                RunEvent::ApprovalInvalidated { input_generation } => {
                    let expired = {
                        let mut cards = self.approvals.lock().await;
                        let ids = cards.iter()
                            .filter(|(_, card)| card.request_id == request_id && card.input_generation < input_generation)
                            .map(|(id, card)| (id.clone(), card.thread_id.clone()))
                            .collect::<Vec<_>>();
                        for (id, _) in &ids { cards.remove(id); }
                        ids
                    };
                    for (id, card_thread) in expired {
                        let _ = self.app.delivery.text(&id, &card_thread, "direct-approval", 0,
                            "追加指示を受けたため、この確認は終了しました。新しい確認が届いた場合は、そちらを開いてください。", json!([])).await;
                    }
                }
                RunEvent::UnsupportedApproval => {
                    self.notice(request_id,&thread,"Codexから未対応の確認形式が届いたため、その要求には形式未対応を返しました。続く回答を待ってください。/status で作業状態を確認できます。AIは再実行していません。").await?;
                }
                RunEvent::Terminal(_) => break,
                RunEvent::DeliveryPending => {
                    self.notice(request_id,&thread,"回答は保存されましたが、Discordへの配信を確認できません。/status で状態を確認してください。AIは再実行しません。").await?;
                    break;
                }
                RunEvent::ResultUnknown(reason) => {
                    let message = if reason == "terminal_content_unavailable" {
                        "Codexの作業終了は確認できましたが、回答や成果物の取得を確定できません。新しい依頼は保留します。/status で状態を確認し、運用者に復旧を依頼してください。AIは再実行していません。"
                    } else {
                        "作業の状態を確認できず、新しい依頼は保留します。/status で確認し、必要なら /stop で待機列を止めてください。運用者による復旧が必要です。AIは再実行していません。"
                    };
                    self.notice(request_id,&thread,message).await?;
                    break;
                }
            }
        }
        Ok::<(),anyhow::Error>(())
        }.await;
        if event_result.is_err() {
            actor.task.abort();
        }
        let task = actor.task.await;
        self.active.lock().await.remove(&thread);
        let expired = {
            let mut cards = self.approvals.lock().await;
            let ids = cards
                .iter()
                .filter(|(_, card)| card.request_id == request_id)
                .map(|(id, card)| (id.clone(), card.thread_id.clone()))
                .collect::<Vec<_>>();
            for (id, _) in &ids {
                cards.remove(id);
            }
            ids
        };
        for (id, card_thread) in expired {
            let _ = self
                .app
                .delivery
                .text(
                    &id,
                    &card_thread,
                    "direct-approval",
                    0,
                    "このMCP承認・入力は終了しました。",
                    json!([]),
                )
                .await;
        }
        let actor_result = task
            .context("direct run actor panicked")
            .and_then(|result| result);
        if event_result.is_err() || actor_result.is_err() {
            let state = self.app.store.request(request_id).await?.state;
            if matches!(
                state,
                RequestState::Sending
                    | RequestState::Running
                    | RequestState::ApprovalRequired
                    | RequestState::CancelRequested
            ) {
                self.app
                    .store
                    .mark_direct_unknown(request_id.into(), "direct_actor_failed".into())
                    .await?;
            }
        }
        event_result?;
        actor_result
    }

    async fn show_approval(
        &self,
        request_id: &str,
        thread: &str,
        id: String,
        operation: DirectInteraction,
        input_generation: u64,
    ) -> Result<()> {
        let kind = match operation.kind {
            InteractionKind::CommandApproval => "コマンド実行",
            InteractionKind::FileChangeApproval => "ファイル変更",
            InteractionKind::UserInput => "追加入力",
            InteractionKind::PermissionsApproval => "権限変更",
            InteractionKind::McpElicitation => "MCP入力",
            InteractionKind::DynamicTool => "ツール操作",
        };
        let subject = if let Some(evidence) = &operation.mcp_evidence {
            format!(
                "MCP操作 {} / {}",
                evidence["server"].as_str().unwrap_or("不明"),
                evidence["tool"].as_str().unwrap_or("不明")
            )
        } else if operation.kind == InteractionKind::McpElicitation
            && operation
                .mcp_tool_decision(ManualDecision::AcceptOnce)
                .is_ok()
        {
            "MCP操作".into()
        } else {
            kind.into()
        };
        let rejection = Self::rejection_button(&operation, &id);
        self.approvals.lock().await.insert(
            id.clone(),
            ApprovalCard {
                request_id: request_id.into(),
                thread_id: thread.into(),
                fingerprint: operation.fingerprint.clone(),
                operation,
                input_generation,
                highest_viewed_page: None,
            },
        );
        // Arbitrary upstream arguments can contain credentials or code. The
        // public message intentionally contains only the operation category.
        let components = json!([{"type":1,"components":[
            {"type":2,"style":1,"label":"自分だけに表示して確認","custom_id":format!("direct:detail:{id}")},
            rejection
        ]}]);
        self.app
            .delivery
            .text(
                &id,
                thread,
                "direct-approval",
                0,
                &format!("Codexが{subject}の確認を求めています。内容を確認して選んでください。"),
                components,
            )
            .await?;
        Ok(())
    }

    async fn control_loop(&self, mut input: mpsc::Receiver<Incoming>) -> Result<()> {
        let mut workers = JoinSet::new();
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>{workers.shutdown().await;return Ok(());}
                joined=workers.join_next(),if !workers.is_empty()=>{
                    match joined {
                        Some(Ok(())) => {}
                        Some(Err(_)) => anyhow::bail!("direct control worker panicked"),
                        None => {}
                    }
                }
                event=input.recv()=>{
                    let Some(event)=event else {anyhow::bail!("Discord control channel closed")};
                    match event {
                        Incoming::Connected(app_id)=>{
                            self.app.discord.register_direct(&app_id,&self.app.cfg.discord.guild_id).await?;
                            self.connected.store(true,Ordering::Release);
                        }
                        Incoming::Reconnected=>{
                            self.app.discord.identify_bot().await?;
                            self.connected.store(true,Ordering::Release);
                        }
                        Incoming::Interaction(value)=>{
                            if workers.len()>=32 {continue;}
                            let this=self.clone();
                            workers.spawn(async move {
                                if let Err(error)=this.handle_interaction(value).await {
                                    tracing::warn!(event="direct_control_failed", category=%error.to_string().split(':').next().unwrap_or("unknown"));
                                }
                            });
                        }
                        Incoming::Message(_)=>{}
                    }
                }
            }
        }
    }

    async fn handle_interaction(&self, value: Value) -> Result<()> {
        let id = value["id"]
            .as_str()
            .context("Discord interaction ID missing")?;
        let token = value["token"]
            .as_str()
            .context("Discord interaction token missing")?;
        if value["guild_id"] != self.app.cfg.discord.guild_id {
            self.app.discord.acknowledge(id, token).await?;
            return Ok(());
        }
        let user = value["member"]["user"]["id"]
            .as_str()
            .or_else(|| value["user"]["id"].as_str());
        if user != Some(self.app.cfg.discord.allowed_user_id.as_str()) {
            self.app.discord.acknowledge(id, token).await?;
            self.app
                .discord
                .reply(
                    value["application_id"]
                        .as_str()
                        .context("application ID missing")?,
                    token,
                    "この操作は許可された利用者だけが実行できます。",
                )
                .await?;
            return Ok(());
        }
        let thread = value["channel_id"]
            .as_str()
            .context("Discord channel missing")?;
        let app_id = value["application_id"]
            .as_str()
            .context("application ID missing")?;
        if let Some(custom) = value["data"]["custom_id"].as_str() {
            if custom.starts_with("direct:model:") {
                return self
                    .model_selection(id, token, app_id, thread, custom, &value)
                    .await;
            }
            if custom.starts_with("direct:effort:") {
                return self
                    .effort_selection(id, token, app_id, thread, custom, &value)
                    .await;
            }
            if custom.starts_with("direct:recover:") {
                return self
                    .recovery_button(id, token, app_id, thread, custom)
                    .await;
            }
            return self
                .approval_button(id, token, app_id, thread, custom)
                .await;
        }
        if value["data"]["name"] == "model"
            && !value["data"]["options"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["name"] == "id"))
        {
            self.app.discord.acknowledge(id, token).await?;
            let result = self.model_selection_menu(thread, id).await;
            let (text, components) = result.unwrap_or_else(|_| {
                (
                    "モデル候補を取得できませんでした。少し待って /model を開き直してください。"
                        .into(),
                    json!([]),
                )
            });
            return self
                .app
                .discord
                .reply_components(app_id, token, &text, components)
                .await;
        }
        if value["data"]["name"] == "effort"
            && !value["data"]["options"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["name"] == "level"))
        {
            self.app.discord.acknowledge(id, token).await?;
            let result = self.effort_selection_menu(thread, id).await;
            let (text, components) = result.unwrap_or_else(|_| {
                (
                    "推論レベル候補を取得できませんでした。少し待って /effort を開き直してください。"
                        .into(),
                    json!([]),
                )
            });
            return self
                .app
                .discord
                .reply_components(app_id, token, &text, components)
                .await;
        }
        if value["data"]["name"] == "recover" {
            self.app.discord.acknowledge(id, token).await?;
            let (text, components) = self.recovery_menu(thread).await.unwrap_or_else(|_| {
                (
                    "復旧対象を確認できませんでした。少し待って /recover を開き直してください。"
                        .into(),
                    json!([]),
                )
            });
            return self
                .app
                .discord
                .reply_components(app_id, token, &text, components)
                .await;
        }
        self.app.discord.acknowledge(id, token).await?;
        let result = self.command(id, thread, &value).await;
        let text = match result {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(event="direct_command_failed",category=%error.to_string().split(':').next().unwrap_or("unknown"));
                "操作を確認できませんでした。/status で状態を確認し、必要なら /stop してください。AIへ同じ指示を自動再送しません。".into()
            }
        };
        self.app.discord.reply(app_id, token, &text).await
    }

    async fn model_selection_menu(
        &self,
        thread: &str,
        interaction: &str,
    ) -> Result<(String, Value)> {
        self.app.verify_location(thread).await?;
        self.app
            .store
            .add_conversation(thread.into(), storage::PROXY_SCOPE.into())
            .await?;
        let current = self.app.store.conversation(thread).await?.selected_model;
        let catalog = DirectModelCatalog {
            launch: self.app.cfg.launch(),
        }
        .list()
        .await?;
        Ok(model_menu(&catalog, &current, interaction))
    }

    async fn effort_selection_menu(
        &self,
        thread: &str,
        interaction: &str,
    ) -> Result<(String, Value)> {
        self.app.verify_location(thread).await?;
        self.app
            .store
            .add_conversation(thread.into(), storage::PROXY_SCOPE.into())
            .await?;
        let conversation = self.app.store.conversation(thread).await?;
        let model = DirectModelCatalog {
            launch: self.app.cfg.launch(),
        }
        .find(&conversation.selected_model)
        .await?;
        Ok(effort_menu(
            &model.supported_reasoning_efforts,
            &conversation.selected_reasoning_effort,
            interaction,
        ))
    }

    async fn recovery_menu(&self, thread: &str) -> Result<(String, Value)> {
        self.app.verify_location(thread).await?;
        if self.active.lock().await.contains_key(thread) {
            return Ok(("この会話ではまだ作業が動いています。/status で確認し、必要なら /stop を使ってください。".into(),json!([])));
        }
        let Some(offer) = self.app.store.direct_recovery_offer(thread).await? else {
            return Ok(("この会話に安全に解除できる結果不明の依頼はありません。/status で状態を確認してください。".into(),json!([])));
        };
        let text = format!(
            "この会話の前の作業は結果を確認できず、次の依頼が止まっています。\n復旧すると、前の作業は再実行せず記録を残し、Codexの会話文脈を新しくします。未送信の待機依頼{}件は取り消します。Discordの履歴と作業ファイルは残ります。\n続ける場合は確認ボタンを押してください。次の新しい投稿から作業できます。",
            offer.waiting
        );
        let components = json!([{"type":1,"components":[
            {"type":2,"style":4,"label":"新しい文脈で会話を再開","custom_id":format!("direct:recover:{}:{}:{}:{}",offer.request_id,offer.generation,offer.pause_revision,offer.next_sequence)}
        ]}]);
        Ok((text, components))
    }

    async fn recovery_button(
        &self,
        id: &str,
        token: &str,
        app_id: &str,
        thread: &str,
        custom: &str,
    ) -> Result<()> {
        self.app.discord.acknowledge_update(id, token).await?;
        let outcome = async {
            self.app.verify_location(thread).await?;
            let identity = custom
                .strip_prefix("direct:recover:")
                .context("recovery identity missing")?;
            let fields = identity.split(':').collect::<Vec<_>>();
            ensure!(fields.len() == 4, "recovery identity invalid");
            let request_id = fields[0];
            let generation = fields[1].parse::<i64>()?;
            let pause_revision = fields[2].parse::<i64>()?;
            let next_sequence = fields[3].parse::<i64>()?;
            ensure!(
                !request_id.is_empty() && generation >= 0,
                "recovery identity invalid"
            );
            ensure!(
                !self.active.lock().await.contains_key(thread),
                "direct run still active"
            );
            let offer = self
                .app
                .store
                .direct_recovery_offer(thread)
                .await?
                .context("recovery no longer available")?;
            ensure!(
                offer.request_id == request_id
                    && offer.generation == generation
                    && offer.pause_revision == pause_revision
                    && offer.next_sequence == next_sequence,
                "recovery offer changed"
            );
            let backup_dir = self.app.cfg.storage.state_dir.join("recovery-backups");
            crate::storage::private_dir(&backup_dir)?;
            let source = self.app.cfg.storage.state_dir.join("gateway.sqlite3");
            let target = backup_dir.join(format!("discord-{id}"));
            let backup =
                tokio::task::spawn_blocking(move || crate::backup::create(&source, &target))
                    .await??;
            ensure!(
                !self.active.lock().await.contains_key(thread),
                "direct run started during recovery"
            );
            self.app
                .store
                .recover_direct_unknown(thread.into(), offer, id.into(), backup.backup_id)
                .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        let text = match outcome {
            Ok(()) => {
                "この会話を再開しました。前の作業は再実行していません。次の新しい投稿からCodexの文脈で作業できます。Discordの履歴と作業ファイルは残っています。"
            }
            Err(error) => {
                tracing::warn!(event="direct_user_recovery_failed",category=%error.to_string().split(':').next().unwrap_or("unknown"));
                "復旧を完了できませんでした。前の作業は再実行していません。/status を確認し、/recover を開き直してください。繰り返し失敗する場合はGatewayの運用者に連絡してください。"
            }
        };
        self.app
            .discord
            .reply_components(app_id, token, text, json!([]))
            .await
    }

    async fn model_selection(
        &self,
        id: &str,
        token: &str,
        app_id: &str,
        thread: &str,
        custom: &str,
        value: &Value,
    ) -> Result<()> {
        self.app.discord.acknowledge_update(id, token).await?;
        let result = async {
            ensure!(
                value["data"]["component_type"] == 3,
                "model choice component type is invalid"
            );
            let source = custom
                .strip_prefix("direct:model:")
                .context("model menu identity missing")?;
            snowflake(source)?;
            let values = value["data"]["values"]
                .as_array()
                .context("model choice missing")?;
            ensure!(values.len() == 1, "exactly one model must be selected");
            let model = values[0].as_str().context("model choice is not text")?;
            self.app.verify_location(thread).await?;
            self.app
                .store
                .add_conversation(thread.into(), storage::PROXY_SCOPE.into())
                .await?;
            let (applied, adjusted_effort) = self.app.choose_model(thread, id, model).await?;
            let current = self.app.store.conversation(thread).await?.selected_model;
            Ok::<String, anyhow::Error>(if applied {
                match adjusted_effort {
                    Some(effort) => format!("選択中のモデルを {current} に変更しました。このモデルで使えるよう、推論レベルは {effort} に合わせました。次の依頼から使います。"),
                    None => format!("選択中のモデルを {current} に変更しました。次の依頼から使います。"),
                }
            } else {
                format!("より新しいモデル選択が優先されています。現在のモデル: {current}")
            })
        }
        .await;
        let text = result.unwrap_or_else(|_| {
            "モデルを変更できませんでした。/model を開き直して選択してください。".into()
        });
        self.app
            .discord
            .reply_components(app_id, token, &text, json!([]))
            .await
    }

    async fn effort_selection(
        &self,
        id: &str,
        token: &str,
        app_id: &str,
        thread: &str,
        custom: &str,
        value: &Value,
    ) -> Result<()> {
        self.app.discord.acknowledge_update(id, token).await?;
        let result = async {
            ensure!(
                value["data"]["component_type"] == 3,
                "reasoning effort component type is invalid"
            );
            let source = custom
                .strip_prefix("direct:effort:")
                .context("reasoning effort menu identity missing")?;
            snowflake(source)?;
            let values = value["data"]["values"]
                .as_array()
                .context("reasoning effort choice missing")?;
            ensure!(
                values.len() == 1,
                "exactly one reasoning effort must be selected"
            );
            let effort = values[0]
                .as_str()
                .context("reasoning effort choice is not text")?;
            let applied = self.app.choose_effort(thread, id, effort).await?;
            let current = self
                .app
                .store
                .conversation(thread)
                .await?
                .selected_reasoning_effort;
            Ok::<String, anyhow::Error>(if applied {
                format!("選択中の推論レベルを {current} に変更しました。次の依頼から使います。")
            } else {
                format!("より新しい推論レベル選択が優先されています。現在: {current}")
            })
        }
        .await;
        let text = result.unwrap_or_else(|_| {
            "推論レベルを変更できませんでした。/effort を開き直して選択してください。".into()
        });
        self.app
            .discord
            .reply_components(app_id, token, &text, json!([]))
            .await
    }

    async fn command(&self, id: &str, thread: &str, value: &Value) -> Result<String> {
        self.app.verify_location(thread).await?;
        self.app
            .store
            .add_conversation(thread.into(), storage::PROXY_SCOPE.into())
            .await?;
        let name = value["data"]["name"]
            .as_str()
            .context("command name missing")?;
        let option = |key: &str| -> Option<&str> {
            value["data"]["options"]
                .as_array()?
                .iter()
                .find(|item| item["name"] == key)?
                .get("value")?
                .as_str()
        };
        match name {
            "status" => {
                let cv = self.app.store.conversation(thread).await?;
                let active = self.app.store.active(thread).await?;
                let active = active.as_ref().map(|r| r.state.as_str()).unwrap_or("なし");
                let recovery = if self.app.store.direct_unknown_blocker(thread).await? {
                    "\n前の作業が結果不明です。/recover で復旧内容を確認できます。/resume だけでは解除されません。"
                } else {
                    ""
                };
                Ok(format!(
                    "実行中: {active}\n待機列: {}\n選択モデル: {}\n推論レベル: {}\n会話状態: {}{recovery}",
                    if cv.paused {
                        "停止中"
                    } else {
                        "再開済み"
                    },
                    cv.selected_model,
                    cv.selected_reasoning_effort,
                    cv.continuation
                ))
            }
            "models" => {
                let list = DirectModelCatalog {
                    launch: self.app.cfg.launch(),
                }
                .list()
                .await?;
                let page = value["data"]["options"]
                    .as_array()
                    .and_then(|items| items.iter().find(|item| item["name"] == "page"))
                    .and_then(|item| item["value"].as_u64())
                    .unwrap_or(1);
                let start = usize::try_from(page.saturating_sub(1))?.saturating_mul(8);
                ensure!(
                    start < list.len() || (list.is_empty() && page == 1),
                    "model catalog page is empty"
                );
                let names = list
                    .iter()
                    .skip(start)
                    .take(8)
                    .map(|m| {
                        format!(
                            "{} — {}",
                            m.id,
                            m.display_name.chars().take(80).collect::<String>()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let next = if start.saturating_add(8) < list.len() {
                    format!("\n続き: /models page:{}", page + 1)
                } else {
                    String::new()
                };
                Ok(format!(
                    "利用可能なモデル（全{}件、ページ{}）:\n{}{}",
                    list.len(),
                    page,
                    names,
                    next
                ))
            }
            "model" => {
                if let Some(model) = option("id") {
                    let (_, adjusted_effort) = self.app.choose_model(thread, id, model).await?;
                    Ok(match adjusted_effort {
                        Some(effort) => format!(
                            "次の依頼からモデルを {model} に変更しました。このモデルで使えるよう、推論レベルは {effort} に合わせました。"
                        ),
                        None => format!("次の依頼からモデルを {model} に変更しました。"),
                    })
                } else {
                    let cv = self.app.store.conversation(thread).await?;
                    Ok(format!(
                        "選択中のモデル: {}\n/models で一覧を確認できます。",
                        cv.selected_model
                    ))
                }
            }
            "effort" => {
                if let Some(effort) = option("level") {
                    self.app.choose_effort(thread, id, effort).await?;
                    Ok(format!(
                        "次の依頼から推論レベルを {effort} に変更しました。"
                    ))
                } else {
                    let cv = self.app.store.conversation(thread).await?;
                    Ok(format!(
                        "選択中の推論レベル: {}\n/effort の level 欄へ文字列を直接入力することもできます。",
                        cv.selected_reasoning_effort
                    ))
                }
            }
            "workspace" => {
                if let Some(path) = self.app.store.bound_direct_workspace(thread).await? {
                    Ok(format!(
                        "この会話の作業フォルダー: {}\n保存先設定を変えても、この会話は同じ場所を使います。",
                        path.display()
                    ))
                } else {
                    Ok(format!(
                        "この会話の最初の依頼から使う作業フォルダー: {}",
                        self.app.cfg.workspace_root().join(thread).display()
                    ))
                }
            }
            "get" => {
                if let Some(path) = option("path") {
                    let artifact = match crate::direct_artifacts::capture(
                        &self.app.store,
                        &self.app.runs.content,
                        crate::direct_artifacts::CaptureTarget {
                            thread_id: thread,
                            request_id: None,
                            call_id: id,
                            relative: path,
                            display_name: None,
                        },
                        self.app.cfg.limits.artifact_bytes,
                    )
                    .await
                    {
                        Ok(artifact) => artifact,
                        Err(error) if error.to_string()=="previous artifact delivery is still unconfirmed" =>
                            return Ok("同じファイルの前回の送信結果を確認できません。会話内の添付を確認してください。重複を避けるため、新たな送信はしていません。".into()),
                        Err(_) => return Ok("このファイルを取得できませんでした。この会話で作成したファイルの相対パス、容量、ファイルの更新状態を確認してください。AIは再実行していません。".into()),
                    };
                    let sent = self
                        .app
                        .delivery
                        .direct_artifact(
                            &artifact,
                            thread,
                            &self.app.cfg.discord.guild_id,
                            self.app.cfg.limits.artifact_bytes,
                        )
                        .await?;
                    Ok(if sent {
                        format!(
                            "{} の保存版をこの会話に送信しました。",
                            artifact.display_name
                        )
                    } else {
                        "成果物の送信結果を確認できません。会話内の添付を確認してください。重複を避けるため自動再送はしません。".into()
                    })
                } else {
                    let artifacts = crate::direct_artifacts::list(&self.app.store, thread).await?;
                    if artifacts.is_empty() {
                        return Ok("この会話に登録された成果物はまだありません。作成したファイルは /get の path 欄に相対パスを指定して取得できます。".into());
                    }
                    let lines = artifacts
                        .iter()
                        .take(20)
                        .map(|artifact| {
                            format!("{} — {}", artifact.display_name, artifact.source_path)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(format!(
                        "この会話の成果物:\n{}",
                        lines.chars().take(1800).collect::<String>()
                    ))
                }
            }
            "resume" => {
                let changed = self.app.resume(id, thread).await?;
                Ok(if changed {
                    "待機列を再開しました。"
                } else {
                    "待機列はすでに再開済みか、再開できない状態です。/status で確認してください。"
                }
                .into())
            }
            "cancel" | "stop" => {
                let active = self.active.lock().await.get(thread).cloned();
                let mut actor_closed = false;
                let outcome = if let Some(active) = active {
                    let (reply, receiver) = oneshot::channel();
                    let command = if name == "cancel" {
                        RunCommand::Cancel {
                            interaction_id: id.into(),
                            discord_thread_id: thread.into(),
                            reply,
                        }
                    } else {
                        RunCommand::Stop {
                            interaction_id: id.into(),
                            discord_thread_id: thread.into(),
                            reply,
                        }
                    };
                    match tokio::time::timeout(
                        Duration::from_secs(5),
                        active.commands.send(command),
                    )
                    .await
                    {
                        Ok(Ok(())) => {
                            match tokio::time::timeout(Duration::from_secs(20), receiver).await {
                                Ok(Ok(result)) => result?,
                                Ok(Err(_)) => {
                                    actor_closed = true;
                                    if name == "cancel" {
                                        self.app.cancel(id, thread, None).await?
                                    } else {
                                        self.app.stop(id, thread, None).await?
                                    }
                                }
                                Err(_) => anyhow::bail!("direct control response timed out"),
                            }
                        }
                        Ok(Err(_)) => {
                            actor_closed = true;
                            if name == "cancel" {
                                self.app.cancel(id, thread, None).await?
                            } else {
                                self.app.stop(id, thread, None).await?
                            }
                        }
                        Err(_) => anyhow::bail!("direct control send timed out"),
                    }
                } else if name == "cancel" {
                    self.app.cancel(id, thread, None).await?
                } else {
                    self.app.stop(id, thread, None).await?
                };
                if actor_closed && name == "stop" {
                    return Ok("待機列を停止しました。前の作業への中断は確認できません。Gatewayの運用者に状態確認を依頼してください。/status でも状態を確認できます。".into());
                }
                Ok(match outcome {
                    DirectControlOutcome::WaitingCancelled => {
                        "直近の待機依頼を取り消しました。実行中の依頼は継続します。"
                    }
                    DirectControlOutcome::InterruptAccepted => {
                        "中断をCodexへ要求しました。完了確認は /status で行ってください。"
                    }
                    DirectControlOutcome::Paused => {
                        "待機列を一時停止しました。/resume で再開できます。"
                    }
                    DirectControlOutcome::NothingActive => {
                        "取り消せる待機依頼も実行中の依頼もありません。"
                    }
                    DirectControlOutcome::ActiveRunUnavailable => {
                        "実行中の操作へ接続できません。/status で状態を確認してください。"
                    }
                }
                .into())
            }
            "steer" => {
                let text = option("text").context("steer text missing")?;
                ensure!(
                    !text.trim().is_empty() && text.len() <= self.app.cfg.limits.text_bytes,
                    "steer text invalid"
                );
                let active = self
                    .active
                    .lock()
                    .await
                    .get(thread)
                    .cloned()
                    .context("実行中の依頼がありません")?;
                let (reply, receiver) = oneshot::channel();
                tokio::time::timeout(
                    Duration::from_secs(5),
                    active.commands.send(RunCommand::Steer {
                        interaction_id: id.into(),
                        discord_thread_id: thread.into(),
                        input: vec![json!({"type":"text","text":text})],
                        reply,
                    }),
                )
                .await??;
                tokio::time::timeout(Duration::from_secs(20), receiver).await???;
                Ok("実行中の依頼へ追加指示を送信しました。".into())
            }
            _ => Ok("このコマンドは直接接続版ではまだ利用できません。".into()),
        }
    }

    async fn approval_button(
        &self,
        id: &str,
        token: &str,
        app_id: &str,
        thread: &str,
        custom: &str,
    ) -> Result<()> {
        let mut parts = custom.split(':');
        let (Some("direct"), Some(action), Some(card_id), extra, None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            self.app.discord.acknowledge(id, token).await?;
            self.app
                .discord
                .reply(
                    app_id,
                    token,
                    "この確認画面は使用できません。/status で作業状態を確認してください。",
                )
                .await?;
            return Ok(());
        };
        if (action == "page") != extra.is_some() {
            self.app.discord.acknowledge(id, token).await?;
            self.app
                .discord
                .reply(
                    app_id,
                    token,
                    "この確認画面のページ指定は使用できません。元の確認を開き直してください。",
                )
                .await?;
            return Ok(());
        }
        let card = self.approvals.lock().await.get(card_id).cloned();
        let Some(card) = card.filter(|card| card.thread_id == thread) else {
            self.app.discord.acknowledge(id, token).await?;
            self.app
                .discord
                .reply(
                    app_id,
                    token,
                    "この確認は終了しました。/status で作業状態を確認してください。",
                )
                .await?;
            return Ok(());
        };
        if action == "detail" || action == "page" {
            let pages = approval_detail(&card.operation)?;
            let page = if action == "detail" {
                0
            } else {
                extra
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(usize::MAX)
            };
            let allowed = page < pages.len()
                && (action == "detail"
                    || card
                        .highest_viewed_page
                        .is_some_and(|highest| page <= highest.saturating_add(1)));
            if !allowed {
                self.app.discord.acknowledge(id, token).await?;
                self.app
                    .discord
                    .reply(
                        app_id,
                        token,
                        "このページはまだ開けません。元の確認画面から順番に確認してください。",
                    )
                    .await?;
                return Ok(());
            }
            let mut buttons = Vec::new();
            if page > 0 {
                buttons.push(json!({"type":2,"style":2,"label":"前へ","custom_id":format!("direct:page:{card_id}:{}",page-1)}));
            }
            if page + 1 < pages.len() {
                buttons.push(json!({"type":2,"style":1,"label":"次へ","custom_id":format!("direct:page:{card_id}:{}",page+1)}));
            }
            if page + 1 == pages.len() && Self::answerable(&card.operation) {
                buttons.push(json!({"type":2,"style":3,"label":"今回だけ許可","custom_id":format!("direct:accept:{card_id}")}));
                if card.operation.run_grant_tool().is_some() {
                    buttons.push(json!({"type":2,"style":3,"label":"この依頼中、このツールを許可","custom_id":format!("direct:run:{card_id}")}));
                }
            }
            buttons.push(Self::rejection_button(&card.operation, card_id));
            let callback_type = if action == "detail" { 4 } else { 7 };
            let mut data = json!({"content":pages[page],"components":[{"type":1,"components":buttons}],"allowed_mentions":{"parse":[]}});
            if action == "detail" {
                data["flags"] = json!(64);
            }
            self.app
                .discord
                .interaction_callback(id, token, json!({"type":callback_type,"data":data}))
                .await?;
            if let Some(current) = self.approvals.lock().await.get_mut(card_id)
                && current.fingerprint == card.fingerprint
            {
                current.highest_viewed_page = Some(
                    current
                        .highest_viewed_page
                        .map_or(page, |highest| highest.max(page)),
                );
            }
            return Ok(());
        }
        self.app.discord.acknowledge(id, token).await?;
        let decision = match action {
            "accept" => ManualDecision::AcceptOnce,
            "run" => ManualDecision::AcceptOnce,
            "decline" => ManualDecision::Decline,
            "cancel" => ManualDecision::Cancel,
            "reject" => ManualDecision::Decline,
            _ => {
                self.app
                    .discord
                    .reply(app_id, token, "このボタンは使用できません。")
                    .await?;
                return Ok(());
            }
        };
        if matches!(action, "accept" | "run") && !Self::answerable(&card.operation) {
            self.app.discord.reply(app_id, token, "この確認形式には許可を返せません。取り消すか、/stop で作業を中断してください。").await?;
            return Ok(());
        }
        if matches!(action, "accept" | "run")
            && card.highest_viewed_page != Some(approval_detail(&card.operation)?.len() - 1)
        {
            self.app
                .discord
                .reply(
                    app_id,
                    token,
                    "操作内容の全ページを確認してから許可してください。元の確認画面で「自分だけに表示して確認」を開き、最後のページまで進んでください。",
                )
                .await?;
            return Ok(());
        }
        if (action == "decline" && !Self::declineable(&card.operation))
            || (action == "run" && card.operation.run_grant_tool().is_none())
            || (action == "cancel" && !Self::cancelable(&card.operation))
            || (action == "reject"
                && (Self::declineable(&card.operation) || Self::cancelable(&card.operation)))
        {
            self.app.discord.reply(app_id,token,"この確認画面の選択肢は現在の要求と一致しません。/status で状態を確認してください。").await?;
            return Ok(());
        }
        let active = self.active.lock().await.get(thread).cloned();
        let Some(active) = active.filter(|active| active.request_id == card.request_id) else {
            self.app
                .discord
                .reply(
                    app_id,
                    token,
                    "元の実行は終了しました。この確認は適用していません。",
                )
                .await?;
            return Ok(());
        };
        let (reply, receiver) = oneshot::channel();
        let command = if action == "reject" {
            RunCommand::RejectUnsupported {
                interaction_id: card_id.into(),
                fingerprint: card.fingerprint,
                input_generation: card.input_generation,
                reply,
            }
        } else if matches!(
            card.operation.kind,
            InteractionKind::UserInput | InteractionKind::McpElicitation
        ) {
            RunCommand::McpToolApproval {
                interaction_id: card_id.into(),
                operation: card.operation,
                decision,
                run_grant: action == "run",
                input_generation: card.input_generation,
                reply,
            }
        } else {
            RunCommand::Approval {
                interaction_id: card_id.into(),
                fingerprint: card.fingerprint,
                decision,
                input_generation: card.input_generation,
                reply,
            }
        };
        tokio::time::timeout(Duration::from_secs(5), active.commands.send(command)).await??;
        let result = tokio::time::timeout(Duration::from_secs(20), receiver).await??;
        match result {
            Ok(()) => {
                let feedback = match action {
                    "reject" => {
                        "この確認形式には対応できないため、Codexへ形式未対応エラーを返しました。/status で作業結果を確認してください。"
                    }
                    "decline" => "この操作を拒否しました。続きの回答はこの会話に届きます。",
                    "cancel" => "この確認を取り消しました。続きの回答はこの会話に届きます。",
                    "run" => {
                        "この依頼中、同じMCPツールの後続呼出しを許可しました。引数が変わる操作も対象です。追加指示・停止・依頼終了で失効します。"
                    }
                    _ => "この操作だけを許可しました。続きの回答はこの会話に届きます。",
                };
                self.app.discord.reply(app_id, token, feedback).await?
            }
            Err(error) => {
                let message = if error.to_string().contains("earlier input generation") {
                    "追加指示の前の確認画面です。このボタンは使えません。新しい確認が届いた場合は、そちらを開いてください。"
                } else if self
                    .app
                    .store
                    .direct_interaction_state(card_id.into())
                    .await
                    .is_ok_and(|state| state == "PENDING")
                {
                    "この選択はCodexへ送信されませんでした。元の確認画面はまだ有効です。少し待って開き直すか、拒否または /stop を使ってください。"
                } else {
                    "確認の結果を確定できません。再度押さず、/status で作業状態を確認してください。"
                };
                self.app.discord.reply(app_id, token, message).await?
            }
        }
        Ok(())
    }
}

pub async fn run(cfg: DirectConfig) -> Result<()> {
    cfg.validate()?;
    let _lock = storage::StateLock::acquire(&cfg.storage.state_dir)?;
    let (store, done) = Store::open_direct(&cfg)?;
    store.startup_recover().await?;
    store.fence_direct_after_restart().await?;
    store.fence_direct_approvals_after_restart().await?;
    let discord = Discord::new(secret(&cfg.discord.token_file)?)?;
    discord.identify_bot().await?;
    let delivery = Delivery {
        store: store.clone(),
        discord: discord.clone(),
    };
    let app = DirectApplication {
        cfg: cfg.clone(),
        store: store.clone(),
        discord: discord.clone(),
        files: Files::new()?,
        runs: DirectRunService {
            store: store.clone(),
            pool: CodexRuntimePool::new(cfg.launch()),
            content: DirectContent::new(&cfg.storage.state_dir)?,
            state_dir: cfg.storage.state_dir.clone(),
            workspace_root: cfg.workspace_root(),
            output_limit: cfg.limits.output_bytes,
            image_max_count: 16,
            image_max_bytes: cfg.limits.artifact_bytes,
        },
        delivery: delivery.clone(),
    };
    delivery.recover().await?;
    delivery
        .recover_direct_images(&cfg.discord.guild_id, cfg.limits.artifact_bytes)
        .await?;
    delivery
        .recover_direct_artifacts(&cfg.discord.guild_id, cfg.limits.artifact_bytes)
        .await?;
    let coordinator = DirectCoordinator::new(app);
    let (messages, messages_rx) = mpsc::channel(128);
    let (controls, controls_rx) = mpsc::channel(64);
    let intents = serenity::all::GatewayIntents::GUILDS
        | serenity::all::GatewayIntents::GUILD_MESSAGES
        | serenity::all::GatewayIntents::MESSAGE_CONTENT;
    let mut client = serenity::Client::builder(discord.secret(), intents)
        .event_handler(Health {
            connected: coordinator.connected.clone(),
        })
        .raw_event_handler(Handler {
            messages,
            controls,
            cancel: coordinator.cancel.clone(),
        })
        .await?;
    let shards = client.shard_manager.clone();
    let mut tasks = JoinSet::new();
    let a = coordinator.clone();
    tasks.spawn(async move { ("admission", a.admit_loop(messages_rx).await) });
    let a = coordinator.clone();
    tasks.spawn(async move { ("control", a.control_loop(controls_rx).await) });
    let a = coordinator.clone();
    tasks.spawn(async move { ("scheduler", a.scheduler_loop().await) });
    let a = coordinator.clone();
    tasks.spawn(async move { ("typing", a.typing_loop().await) });
    let a = coordinator.clone();
    tasks.spawn(async move {("sweeper",async {
        let mut tick=tokio::time::interval(Duration::from_secs(1));
        loop {tokio::select!{_=a.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{a.app.store.sweep().await?;}}}
    }.await)});
    tasks.spawn(async move { ("discord", client.start().await.map_err(anyhow::Error::from)) });
    tasks.spawn(async move {
        (
            "store",
            async {
                let _ = done.await;
                anyhow::bail!("direct store worker exited")
            }
            .await,
        )
    });
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let failed = tokio::select! {
        _=tokio::signal::ctrl_c()=>false,
        _=term.recv()=>false,
        _=coordinator.cancel.cancelled()=>true,
        result=tasks.join_next()=>{
            if let Some(Ok((role,result)))=result {tracing::error!(role,error=%result.is_err(),event="direct_critical_task_exited");}
            true
        }
    };
    coordinator.connected.store(false, Ordering::Release);
    coordinator.cancel.cancel();
    coordinator.app.runs.pool.close();
    let _ = tokio::time::timeout(Duration::from_secs(10), shards.shutdown_all()).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    tasks.shutdown().await;
    ensure!(
        !failed,
        "direct Gateway stopped after critical task failure"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{codex_transport::LaunchConfig, direct_config::Codex};
    use axum::{
        Json, Router,
        routing::{get, patch, post},
    };

    #[test]
    fn model_menu_exposes_selectable_catalog_and_reports_overflow() {
        let models = (0..27)
            .map(|index| DirectModel {
                id: format!("model-{index}"),
                display_name: format!("Model {index}"),
                default_reasoning_effort: "medium".into(),
                supported_reasoning_efforts: vec![DirectReasoningEffort {
                    id: "high".into(),
                    description: "Deep reasoning".into(),
                }],
            })
            .collect::<Vec<_>>();
        let (text, components) = model_menu(&models, "model-0", "123");
        assert!(text.contains("残り2件"));
        let select = &components[0]["components"][0];
        assert_eq!(select["type"], 3);
        assert_eq!(select["custom_id"], "direct:model:123");
        assert_eq!(select["options"].as_array().unwrap().len(), 25);
        assert_eq!(select["options"][0]["value"], "model-0");
        assert_eq!(select["options"][0]["default"], true);
    }

    #[test]
    fn effort_menu_uses_the_selected_models_catalog() {
        let efforts = vec![
            DirectReasoningEffort {
                id: "medium".into(),
                description: "Balanced reasoning".into(),
            },
            DirectReasoningEffort {
                id: "high".into(),
                description: "Deep reasoning".into(),
            },
        ];
        let (_, components) = effort_menu(&efforts, "high", "456");
        let select = &components[0]["components"][0];
        assert_eq!(select["custom_id"], "direct:effort:456");
        assert_eq!(select["options"][1]["value"], "high");
        assert_eq!(select["options"][1]["default"], true);
    }

    #[test]
    fn admission_errors_tell_the_user_what_to_do_next() {
        let limits = crate::config::Limits {
            attachments: 4,
            attachment_bytes: 8 * 1024 * 1024,
            input_bytes: 16 * 1024 * 1024,
            text_bytes: 256 * 1024,
            image_pixels: 20_000_000,
            artifact_bytes: 8 * 1024 * 1024,
            temp_bytes: 32 * 1024 * 1024,
            output_bytes: 1024 * 1024,
            output_total_bytes: 8 * 1024 * 1024,
            delivery_retention_secs: 60,
            queue_conversation: 5,
            queue_global: 20,
            validation_secs: 120,
        };
        let too_large = input_rejection_message(&anyhow::anyhow!("attachment too large"), &limits);
        assert!(too_large.contains("8.0 MiB"));
        assert!(too_large.contains("小さくするか分割"));
        assert!(too_large.contains("AIへは送信していません"));

        let unavailable =
            input_rejection_message(&anyhow::anyhow!("attachment fetch failed"), &limits);
        assert!(unavailable.contains("添付し直し"));
        assert!(unavailable.contains("AIへは送信していません"));
    }

    #[test]
    fn approval_buttons_follow_the_upstream_decisions() {
        let active = crate::codex_execution::TurnIdentity {
            thread_id: "thread-one".into(),
            turn_id: "turn-one".into(),
        };
        let request = crate::codex_transport::Event::ServerRequest {
            id: json!("approval"),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({"threadId":"thread-one","turnId":"turn-one","itemId":"item-one","availableDecisions":["decline"]}),
        };
        let operation = DirectInteraction::from_event(&request, &active)
            .unwrap()
            .unwrap();
        assert!(!DirectCoordinator::answerable(&operation));
        assert_eq!(
            DirectCoordinator::rejection_button(&operation, "card")["label"],
            "拒否"
        );
        let unsupported = crate::codex_transport::Event::ServerRequest {
            id: json!("unsupported"),
            method: "item/permissions/requestApproval".into(),
            params: json!({"threadId":"thread-one","turnId":"turn-one","itemId":"item-one"}),
        };
        let operation = DirectInteraction::from_event(&unsupported, &active)
            .unwrap()
            .unwrap();
        assert_eq!(
            DirectCoordinator::rejection_button(&operation, "card")["label"],
            "未対応の確認を終了"
        );
    }

    #[test]
    fn approval_detail_pages_are_utf16_bounded_and_retain_emoji_content() {
        let active = crate::codex_execution::TurnIdentity {
            thread_id: "thread-one".into(),
            turn_id: "turn-one".into(),
        };
        let command = format!("EMOJI_START {} EMOJI_END", "😀".repeat(1200));
        let request = crate::codex_transport::Event::ServerRequest {
            id: json!("approval"),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({"threadId":"thread-one","turnId":"turn-one","itemId":"item-one","command":command,"availableDecisions":["accept","cancel"]}),
        };
        let operation = DirectInteraction::from_event(&request, &active)
            .unwrap()
            .unwrap();
        let pages = approval_detail(&operation).unwrap();
        assert!(pages.len() > 1);
        assert!(pages.iter().all(|page| page.encode_utf16().count() <= 1900));
        let combined = pages.join("\n");
        assert!(combined.contains("EMOJI\\_START"));
        assert!(combined.contains("EMOJI\\_END"));
        assert_eq!(combined.matches('😀').count(), 1200);
    }
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Mutex as StdMutex};

    #[tokio::test]
    async fn raw_message_reaches_direct_child_and_saved_answer_is_delivered_once() {
        direct_flow(false, false, false, false, None, false, false).await;
    }

    #[tokio::test]
    async fn mcp_approval_button_returns_to_exact_direct_run() {
        direct_flow(true, false, false, false, None, false, false).await;
    }

    #[tokio::test]
    async fn mcp_empty_form_shows_arguments_and_accepts_only_this_call() {
        direct_flow(false, false, false, false, Some(true), false, false).await;
    }

    #[tokio::test]
    async fn mcp_empty_form_decline_uses_the_elicitation_reply() {
        direct_flow(false, false, false, false, Some(false), false, false).await;
    }

    #[tokio::test]
    async fn mcp_run_grant_button_skips_second_verified_call() {
        direct_flow(false, false, false, false, None, true, false).await;
    }

    #[tokio::test]
    async fn artifact_tool_registers_a_conversation_bound_saved_file() {
        direct_flow(false, false, true, false, None, false, false).await;
    }

    #[tokio::test]
    async fn unsupported_permission_prompt_can_be_rejected_without_opening_detail() {
        direct_flow(false, false, false, true, None, false, false).await;
    }

    #[tokio::test]
    #[ignore = "requires local Codex login and network; run explicitly for integration acceptance"]
    async fn real_codex_reaches_mock_discord_through_direct_daemon() {
        direct_flow(false, true, false, false, None, false, false).await;
    }

    #[tokio::test]
    async fn long_command_pages_before_accept_and_keeps_cancel_available() {
        direct_flow(false, false, false, false, None, false, true).await;
    }

    async fn direct_flow(
        mcp_approval: bool,
        real_codex: bool,
        artifact_tool: bool,
        unsupported_approval: bool,
        mcp_form_approval: Option<bool>,
        mcp_run_grant: bool,
        long_command_approval: bool,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let home = root.join("codex-home");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        if real_codex {
            let source = PathBuf::from(std::env::var("HOME").unwrap()).join(".codex/auth.json");
            fs::copy(source, home.join("auth.json")).unwrap();
            fs::set_permissions(home.join("auth.json"), fs::Permissions::from_mode(0o600)).unwrap();
        }
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        let mock_command = root.join("mock-codex");
        fs::write(
            &mock_command,
            format!(
                "#!/bin/sh\nexec python3 '{}/tests/fixtures/mock_app_server.py'\n",
                env!("CARGO_MANIFEST_DIR")
            ),
        )
        .unwrap();
        fs::set_permissions(&mock_command, fs::Permissions::from_mode(0o700)).unwrap();
        let cfg = DirectConfig {
            discord: crate::config::Discord {
                guild_id: "1".into(),
                allowed_user_id: "2".into(),
                token_file: root.join("bot-token"),
                response_mode: crate::config::ResponseMode::All,
            },
            codex: Codex {
                command: if real_codex {
                    PathBuf::from("codex")
                } else {
                    mock_command
                },
                home,
                workspace_root: None,
                model_provider: "openai".into(),
                sandbox: "workspace-write".into(),
                approval_policy: "on-request".into(),
                network_access: false,
            },
            storage: crate::config::Storage {
                state_dir: state.clone(),
                temp_dir: root.join("temp"),
                socket_path: root.join("admin.sock"),
            },
            limits: crate::config::Limits {
                attachments: 2,
                attachment_bytes: 1024 * 1024,
                input_bytes: 1024 * 1024,
                text_bytes: 4096,
                image_pixels: 1000000,
                artifact_bytes: 1024 * 1024,
                temp_bytes: 2 * 1024 * 1024,
                output_bytes: 8192,
                output_total_bytes: 1024 * 1024,
                delivery_retention_secs: 3600,
                queue_conversation: 5,
                queue_global: 20,
                validation_secs: 120,
            },
            default_model: "gpt-5.6-luna".into(),
            default_reasoning_effort: "high".into(),
        };
        fs::write(&cfg.discord.token_file, "token").unwrap();
        storage::initialize_direct(&cfg).unwrap();
        let _lock = storage::StateLock::acquire(&cfg.storage.state_dir).unwrap();
        let (store, _done) = Store::open_direct(&cfg).unwrap();
        let prompt = if real_codex {
            "Reply with exactly DIRECT_GATEWAY_OK. Do not call tools."
        } else {
            "hello"
        };
        let original = json!({"id":"999","channel_id":"4","guild_id":"1","author":{"id":"2","bot":false},"webhook_id":null,"content":prompt,"attachments":[],"edited_timestamp":null});
        let posts = Arc::new(StdMutex::new(Vec::<Value>::new()));
        let posted = posts.clone();
        let callbacks = Arc::new(StdMutex::new(Vec::<Value>::new()));
        let callback_copy = callbacks.clone();
        let edits = Arc::new(StdMutex::new(Vec::<Value>::new()));
        let edit_copy = edits.clone();
        let approval_edits = Arc::new(StdMutex::new(Vec::<Value>::new()));
        let approval_edit_copy = approval_edits.clone();
        let router = Router::new()
            .route(
                "/channels/4",
                get(|| async { Json(json!({"id":"4","guild_id":"1","type":0})) }),
            )
            .route(
                "/channels/4/messages/999",
                get({
                    let original = original.clone();
                    move || {
                        let original = original.clone();
                        async move { Json(original) }
                    }
                }),
            )
            .route(
                "/channels/4/messages",
                post(move |Json(body): Json<Value>| {
                    let posted = posted.clone();
                    async move {
                        posted.lock().unwrap().push(body);
                        Json(json!({"id":"1000","channel_id":"4"}))
                    }
                }),
            )
            .route(
                "/channels/4/messages/1000",
                patch(move |Json(body): Json<Value>| {
                    let seen = approval_edit_copy.clone();
                    async move {
                        seen.lock().unwrap().push(body);
                        Json(json!({"id":"1000","channel_id":"4"}))
                    }
                }),
            )
            .route(
                "/interactions/{id}/{token}/callback",
                post(move |Json(body): Json<Value>| {
                    let callbacks = callback_copy.clone();
                    async move {
                        callbacks.lock().unwrap().push(body);
                        Json(Value::Null)
                    }
                }),
            )
            .route(
                "/webhooks/{app}/{token}/messages/@original",
                patch(move |Json(body): Json<Value>| {
                    let edits = edit_copy.clone();
                    async move {
                        edits.lock().unwrap().push(body);
                        Json(Value::Null)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let discord = Discord::with_endpoint("token".into(), format!("http://{address}")).unwrap();
        let app = DirectApplication {
            cfg: cfg.clone(),
            store: store.clone(),
            discord: discord.clone(),
            files: Files::new().unwrap(),
            runs: DirectRunService {
                store: store.clone(),
                pool: CodexRuntimePool::new(LaunchConfig {
                    command: if real_codex {
                        PathBuf::from("codex")
                    } else {
                        PathBuf::from("python3")
                    },
                    args: if real_codex {
                        vec!["app-server".into(), "--listen".into(), "stdio://".into()]
                    } else {
                        let mut args = vec![format!(
                            "{}/tests/fixtures/mock_app_server.py",
                            env!("CARGO_MANIFEST_DIR")
                        )];
                        if long_command_approval {
                            args.push("--request-long-command-approval".into());
                        } else if mcp_approval {
                            args.push("--request-mcp-approval".into());
                        }
                        if mcp_form_approval.is_some() {
                            args.push("--request-mcp-form-approval".into());
                        }
                        if mcp_run_grant {
                            args.push("--request-mcp-run-grant".into());
                        }
                        if artifact_tool {
                            args.push("--request-artifact".into());
                        }
                        if unsupported_approval {
                            args.push("--request-unsupported-approval".into());
                        }
                        args
                    },
                    codex_home: if real_codex {
                        cfg.codex.home.clone()
                    } else {
                        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    },
                    initialize_timeout: Duration::from_secs(if real_codex { 20 } else { 10 }),
                    request_timeout: Duration::from_secs(if real_codex { 45 } else { 2 }),
                    experimental_api: true,
                }),
                content: DirectContent::new(&state).unwrap(),
                state_dir: state,
                workspace_root: cfg.workspace_root(),
                output_limit: cfg.limits.output_bytes,
                image_max_count: 16,
                image_max_bytes: cfg.limits.artifact_bytes,
            },
            delivery: Delivery {
                store: store.clone(),
                discord,
            },
        };
        let controller = DirectCoordinator::new(app);
        controller.connected.store(true, Ordering::Release);
        let (tx, rx) = mpsc::channel(8);
        let worker = {
            let controller = controller.clone();
            tokio::spawn(async move { controller.admit_loop(rx).await })
        };
        let scheduler = {
            let controller = controller.clone();
            tokio::spawn(async move { controller.scheduler_loop().await })
        };
        tx.send(Incoming::Message(original.clone())).await.unwrap();
        tx.send(Incoming::Message(original)).await.unwrap();
        if mcp_approval
            || unsupported_approval
            || mcp_form_approval.is_some()
            || mcp_run_grant
            || long_command_approval
        {
            let card = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(value) = posts
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|post| {
                            post["content"].as_str().is_some_and(|s| {
                                s.contains(if long_command_approval {
                                    "コマンド実行"
                                } else if unsupported_approval {
                                    "権限変更"
                                } else {
                                    "MCP操作"
                                })
                            })
                        })
                        .cloned()
                    {
                        break value;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap();
            let detail = card["components"][0]["components"][0]["custom_id"]
                .as_str()
                .unwrap();
            let approval = detail.replace(
                ":detail:",
                if long_command_approval {
                    ":accept:"
                } else if unsupported_approval {
                    ":reject:"
                } else if mcp_run_grant {
                    ":run:"
                } else if mcp_form_approval == Some(false) {
                    ":decline:"
                } else {
                    ":accept:"
                },
            );
            let interaction = |id: &str, custom: &str| json!({"id":id,"token":"token","application_id":"123","guild_id":"1","channel_id":"4","member":{"user":{"id":"2"}},"data":{"custom_id":custom}});
            if long_command_approval {
                controller
                    .handle_interaction(interaction("1001", detail))
                    .await
                    .unwrap();
                let first = callbacks.lock().unwrap().last().unwrap().clone();
                assert_eq!(first["type"], 4);
                let first_text = first["data"]["content"].as_str().unwrap();
                assert!(first_text.encode_utf16().count() <= 1900);
                assert!(first_text.contains("LONG\\_COMMAND\\_START"));
                assert!(!first_text.contains("LONG\\_COMMAND\\_END"));
                let first_buttons = first["data"]["components"][0]["components"]
                    .as_array()
                    .unwrap();
                assert!(
                    !first_buttons
                        .iter()
                        .any(|button| button["label"] == "今回だけ許可")
                );
                assert!(
                    first_buttons
                        .iter()
                        .any(|button| button["label"] == "取り消し")
                );
                let forged_before = edits.lock().unwrap().len();
                controller
                    .handle_interaction(interaction("1001", &approval))
                    .await
                    .unwrap();
                let forged = edits.lock().unwrap().last().unwrap().clone();
                assert_eq!(edits.lock().unwrap().len(), forged_before + 1);
                assert!(
                    forged["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("全ページを確認してから許可"))
                );
                let mut next = first_buttons
                    .iter()
                    .find(|button| button["label"] == "次へ")
                    .and_then(|button| button["custom_id"].as_str())
                    .unwrap()
                    .to_owned();
                let mut page = 1usize;
                loop {
                    controller
                        .handle_interaction(interaction("1001", &next))
                        .await
                        .unwrap();
                    let current = callbacks.lock().unwrap().last().unwrap().clone();
                    assert_eq!(current["type"], 7);
                    let text = current["data"]["content"].as_str().unwrap();
                    assert!(text.encode_utf16().count() <= 1900);
                    let buttons = current["data"]["components"][0]["components"]
                        .as_array()
                        .unwrap();
                    if let Some(button) = buttons.iter().find(|button| button["label"] == "次へ")
                    {
                        page += 1;
                        next = button["custom_id"].as_str().unwrap().to_owned();
                        continue;
                    }
                    assert!(page >= 1);
                    assert!(text.contains("LONG\\_COMMAND\\_END"));
                    assert!(
                        buttons
                            .iter()
                            .any(|button| button["label"] == "今回だけ許可")
                    );
                    assert!(buttons.iter().any(|button| button["label"] == "取り消し"));
                    break;
                }
            }
            if mcp_approval {
                controller
                    .handle_interaction(interaction("1001", detail))
                    .await
                    .unwrap();
                assert!(callbacks.lock().unwrap().iter().any(|body| {
                    body["data"]["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("mcp\\_tool\\_call\\_approval\\_item\\-one"))
                }));
            }
            if mcp_form_approval.is_some() || mcp_run_grant {
                controller
                    .handle_interaction(interaction("1001", detail))
                    .await
                    .unwrap();
                let view = callbacks.lock().unwrap().last().unwrap().clone();
                let content = view["data"]["content"].as_str().unwrap();
                assert!(content.contains(if mcp_run_grant {
                    "browser\\_click"
                } else {
                    "browser\\_run\\_code\\_unsafe"
                }));
                assert!(
                    content.contains(if mcp_run_grant {
                        "button\\-1"
                    } else {
                        "async \\(page\\) =\\> await page\\.title\\(\\)"
                    }),
                    "rendered content: {content}"
                );
                let buttons = view["data"]["components"][0]["components"]
                    .as_array()
                    .unwrap();
                assert!(
                    buttons
                        .iter()
                        .any(|button| button["label"] == "今回だけ許可")
                );
                assert!(buttons.iter().any(|button| button["label"] == "拒否"));
                assert_eq!(
                    buttons
                        .iter()
                        .any(|button| button["label"] == "この依頼中、このツールを許可"),
                    mcp_run_grant
                );
            }
            controller
                .handle_interaction(interaction("1002", &approval))
                .await
                .unwrap();
        }
        tokio::time::timeout(
            Duration::from_secs(if real_codex { 150 } else { 10 }),
            async {
                loop {
                    if posts.lock().unwrap().iter().any(|post| {
                        post["content"].as_str().is_some_and(|s| {
                            s.contains(if real_codex {
                                "DIRECT_GATEWAY_OK"
                            } else {
                                "DONE"
                            })
                        })
                    }) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "answer delivery timed out; posts={:?}",
                posts.lock().unwrap()
            )
        });
        let states = store.status().await.unwrap();
        assert_eq!(states["requests"], json!([["COMPLETED", 1]]));
        if mcp_run_grant {
            assert_eq!(
                posts
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|post| post["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("CodexがMCP操作")
                            && text.contains("確認を求めています")))
                    .count(),
                1
            );
        }
        if mcp_approval || unsupported_approval || mcp_form_approval.is_some() || mcp_run_grant {
            let updates = approval_edits.lock().unwrap();
            assert!(updates.iter().any(|body| body["components"] == json!([])));
        }
        assert_eq!(
            posts
                .lock()
                .unwrap()
                .iter()
                .filter(|post| post["content"].as_str().is_some_and(|s| s.contains(
                    if real_codex {
                        "DIRECT_GATEWAY_OK"
                    } else {
                        "DONE"
                    }
                )))
                .count(),
            1
        );
        if artifact_tool {
            let artifacts = crate::direct_artifacts::list(&store, "4").await.unwrap();
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].source_path, "report.txt");
            let listing = controller
                .command("1004", "4", &json!({"data":{"name":"get","options":[]}}))
                .await
                .unwrap();
            assert!(listing.contains("report.txt"));
            assert_eq!(
                controller
                    .app
                    .runs
                    .content
                    .read_artifact(&artifacts[0].saved, cfg.limits.artifact_bytes)
                    .unwrap(),
                b"mock artifact"
            );
        }
        if !real_codex {
            let slash = json!({"id":"2001","token":"token","application_id":"123","guild_id":"1","channel_id":"4","member":{"user":{"id":"2"}},"data":{"name":"model","options":[]}});
            controller.handle_interaction(slash).await.unwrap();
            assert_eq!(callbacks.lock().unwrap().last().unwrap()["type"], 5);
            let menu = edits.lock().unwrap().last().unwrap().clone();
            assert!(menu["content"].as_str().unwrap().contains("選択中のモデル"));
            let select = &menu["components"][0]["components"][0];
            assert_eq!(select["type"], 3);
            assert_eq!(select["custom_id"], "direct:model:2001");
            assert!(
                select["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|o| o["value"] == "gpt-5.6-terra")
            );
            let choice = json!({"id":"2002","token":"token","application_id":"123","guild_id":"1","channel_id":"4","member":{"user":{"id":"2"}},"data":{"custom_id":"direct:model:2001","component_type":3,"values":["gpt-5.6-terra"]}});
            controller.handle_interaction(choice).await.unwrap();
            assert_eq!(callbacks.lock().unwrap().last().unwrap()["type"], 6);
            assert_eq!(
                store.conversation("4").await.unwrap().selected_model,
                "gpt-5.6-terra"
            );
            let applied = edits.lock().unwrap().last().unwrap().clone();
            assert!(
                applied["content"]
                    .as_str()
                    .unwrap()
                    .contains("gpt-5.6-terra")
            );
            assert_eq!(applied["components"], json!([]));
            let effort_slash = json!({"id":"2011","token":"token","application_id":"123","guild_id":"1","channel_id":"4","member":{"user":{"id":"2"}},"data":{"name":"effort","options":[]}});
            controller.handle_interaction(effort_slash).await.unwrap();
            assert_eq!(callbacks.lock().unwrap().last().unwrap()["type"], 5);
            let effort_view = edits.lock().unwrap().last().unwrap().clone();
            assert!(
                effort_view["content"]
                    .as_str()
                    .unwrap()
                    .contains("選択中の推論レベル")
            );
            let effort_select = &effort_view["components"][0]["components"][0];
            assert_eq!(effort_select["custom_id"], "direct:effort:2011");
            assert!(
                effort_select["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|option| option["value"] == "high")
            );
            let effort_choice = json!({"id":"2012","token":"token","application_id":"123","guild_id":"1","channel_id":"4","member":{"user":{"id":"2"}},"data":{"custom_id":"direct:effort:2011","component_type":3,"values":["high"]}});
            controller.handle_interaction(effort_choice).await.unwrap();
            assert_eq!(
                store
                    .conversation("4")
                    .await
                    .unwrap()
                    .selected_reasoning_effort,
                "high"
            );
            let explicit_effort = controller
                .command(
                    "2013",
                    "4",
                    &json!({"data":{"name":"effort","options":[{"name":"level","value":"low"}]}}),
                )
                .await
                .unwrap();
            assert!(explicit_effort.contains("low"));
            assert_eq!(
                store
                    .conversation("4")
                    .await
                    .unwrap()
                    .selected_reasoning_effort,
                "low"
            );
            let workspace = controller
                .command(
                    "2003",
                    "4",
                    &json!({"data":{"name":"workspace","options":[]}}),
                )
                .await
                .unwrap();
            assert!(workspace.contains("workspaces/4"));
        }
        controller.cancel.cancel();
        worker.await.unwrap().unwrap();
        scheduler.await.unwrap().unwrap();
        server.abort();
    }
}
