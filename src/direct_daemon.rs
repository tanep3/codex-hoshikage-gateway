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
    direct_models::{DirectModel, DirectModelCatalog},
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

impl DirectCoordinator {
    fn answerable(operation: &DirectInteraction) -> bool {
        matches!(
            operation.kind,
            InteractionKind::CommandApproval | InteractionKind::FileChangeApproval
        ) || operation
            .mcp_tool_decision(ManualDecision::AcceptOnce)
            .is_ok()
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
                                if conversation.paused {
                                    this.notice(&id, &request.thread_id, "受け付けました。待機列は停止中です。/resume で再開、/cancel でこの待機依頼を取り消せます。").await?;
                                }
                            }
                            Ok(DirectAdmission::Ignored | DirectAdmission::Duplicate) => {}
                            Err(error) => {
                                tracing::warn!(event="direct_admission_failed", category=%error.to_string().split(':').next().unwrap_or("unknown"));
                                if message["guild_id"] == this.app.cfg.discord.guild_id
                                    && message["author"]["id"] == this.app.cfg.discord.allowed_user_id
                                    && let (Some(thread),Some(message_id))=(thread,message_id) {
                                    let _=this.notice(&message_id,&thread,"受付できませんでした。/status で会話状態を確認し、入力や添付を見直してください。AIへは送信していません。").await;
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
                RunEvent::Approval { interaction_id, operation } => {
                    self.show_approval(request_id, &thread, interaction_id, *operation).await?;
                }
                RunEvent::UnsupportedApproval => {
                    self.notice(request_id,&thread,"Codexから未対応の確認形式が届きました。実行は保留中です。/stop で中断できます。AIを再実行しません。").await?;
                }
                RunEvent::Terminal(_) => break,
                RunEvent::DeliveryPending => {
                    self.notice(request_id,&thread,"回答は保存されましたが、Discordへの配信を確認できません。/status で状態を確認してください。AIは再実行しません。").await?;
                    break;
                }
                RunEvent::ResultUnknown => {
                    self.notice(request_id,&thread,"作業の終了を確認できません。/status で状態を確認してください。AIは再実行しません。").await?;
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
        self.approvals
            .lock()
            .await
            .retain(|_, card| card.request_id != request_id);
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
        } else {
            kind.into()
        };
        self.approvals.lock().await.insert(
            id.clone(),
            ApprovalCard {
                request_id: request_id.into(),
                thread_id: thread.into(),
                fingerprint: operation.fingerprint.clone(),
                operation,
            },
        );
        // Arbitrary upstream arguments can contain credentials or code. The
        // public message intentionally contains only the operation category.
        let components = json!([{"type":1,"components":[
            {"type":2,"style":1,"label":"自分だけに表示して確認","custom_id":format!("direct:detail:{id}")},
            {"type":2,"style":4,"label":"拒否","custom_id":format!("direct:decline:{id}")}
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
            let applied = self.app.choose_model(thread, id, model).await?;
            let current = self.app.store.conversation(thread).await?.selected_model;
            Ok::<String, anyhow::Error>(if applied {
                format!("選択中のモデルを {current} に変更しました。次の依頼から使います。")
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
                Ok(format!(
                    "実行中: {active}\n待機列: {}\n選択モデル: {}\n会話状態: {}",
                    if cv.paused {
                        "停止中"
                    } else {
                        "再開済み"
                    },
                    cv.selected_model,
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
                    self.app.choose_model(thread, id, model).await?;
                    Ok(format!("次の依頼からモデルを {model} に変更しました。"))
                } else {
                    let cv = self.app.store.conversation(thread).await?;
                    Ok(format!(
                        "選択中のモデル: {}\n/models で一覧を確認できます。",
                        cv.selected_model
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
                    tokio::time::timeout(Duration::from_secs(5), active.commands.send(command))
                        .await??;
                    tokio::time::timeout(Duration::from_secs(20), receiver).await???
                } else if name == "cancel" {
                    self.app.cancel(id, thread, None).await?
                } else {
                    self.app.stop(id, thread, None).await?
                };
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
        let (Some("direct"), Some(action), Some(card_id), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
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
        if action == "detail" {
            let rendered = serde_json::to_string_pretty(&json!({
                "upstream_request":card.operation.params,
                "matched_mcp_call":card.operation.mcp_evidence
            }))?;
            let complete = rendered.chars().count() <= 1400;
            let detail = if complete {
                format!(
                    "Codexが求めた操作の実引数（本人限定）:\n```json\n{rendered}\n```\n内容を確認して選んでください。"
                )
            } else {
                "実引数が表示上限を超えました。この画面では全文を確認できないため、許可はできません。拒否または /stop を選んでください。".into()
            };
            let choices = if complete && Self::answerable(&card.operation) {
                json!([{"type":1,"components":[
                    {"type":2,"style":3,"label":"今回だけ許可","custom_id":format!("direct:accept:{card_id}")},
                    {"type":2,"style":4,"label":"拒否","custom_id":format!("direct:decline:{card_id}")}
                ]}])
            } else {
                json!([{"type":1,"components":[
                    {"type":2,"style":4,"label":"拒否","custom_id":format!("direct:decline:{card_id}")}
                ]}])
            };
            self.app.discord.interaction_callback(id,token,json!({"type":4,"data":{"content":detail,"flags":64,"components":choices,"allowed_mentions":{"parse":[]}}})).await?;
            return Ok(());
        }
        self.app.discord.acknowledge(id, token).await?;
        let decision = match action {
            "accept" => ManualDecision::AcceptOnce,
            "decline" => ManualDecision::Decline,
            _ => {
                self.app
                    .discord
                    .reply(app_id, token, "このボタンは使用できません。")
                    .await?;
                return Ok(());
            }
        };
        if action == "accept"
            && (serde_json::to_string_pretty(&json!({"upstream_request":card.operation.params,"matched_mcp_call":card.operation.mcp_evidence}))?
                .chars()
                .count()
                > 1400
                || !Self::answerable(&card.operation))
        {
            self.app
                .discord
                .reply(
                    app_id,
                    token,
                    "この確認形式はまだ回答できません。/stop で中断してください。",
                )
                .await?;
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
        let command = if !Self::answerable(&card.operation) {
            RunCommand::RejectUnsupported {
                interaction_id: card_id.into(),
                fingerprint: card.fingerprint,
                reply,
            }
        } else if card.operation.kind == InteractionKind::UserInput {
            RunCommand::McpToolApproval {
                interaction_id: card_id.into(),
                operation: card.operation,
                decision,
                reply,
            }
        } else {
            RunCommand::Approval {
                interaction_id: card_id.into(),
                fingerprint: card.fingerprint,
                decision,
                reply,
            }
        };
        tokio::time::timeout(Duration::from_secs(5), active.commands.send(command)).await??;
        let result = tokio::time::timeout(Duration::from_secs(20), receiver).await??;
        match result {
            Ok(())=>{
                self.approvals.lock().await.remove(card_id);
                self.app.discord.reply(app_id,token,if decision==ManualDecision::Decline{"この操作を拒否しました。続きの回答はこの会話に届きます。"}else{"この操作だけを許可しました。続きの回答はこの会話に届きます。"}).await?
            }
            Err(_)=>self.app.discord.reply(app_id,token,"確認の結果を確定できません。再度押さず、/status で作業状態を確認してください。").await?
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
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Mutex as StdMutex};

    #[tokio::test]
    async fn raw_message_reaches_direct_child_and_saved_answer_is_delivered_once() {
        direct_flow(false, false, false, false).await;
    }

    #[tokio::test]
    async fn mcp_approval_button_returns_to_exact_direct_run() {
        direct_flow(true, false, false, false).await;
    }

    #[tokio::test]
    async fn artifact_tool_registers_a_conversation_bound_saved_file() {
        direct_flow(false, false, true, false).await;
    }

    #[tokio::test]
    async fn unsupported_permission_prompt_can_be_rejected_without_opening_detail() {
        direct_flow(false, false, false, true).await;
    }

    #[tokio::test]
    #[ignore = "requires local Codex login and network; run explicitly for integration acceptance"]
    async fn real_codex_reaches_mock_discord_through_direct_daemon() {
        direct_flow(false, true, false, false).await;
    }

    async fn direct_flow(
        mcp_approval: bool,
        real_codex: bool,
        artifact_tool: bool,
        unsupported_approval: bool,
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
                        if mcp_approval {
                            args.push("--request-mcp-approval".into());
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
        if mcp_approval || unsupported_approval {
            let card = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(value) = posts
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|post| {
                            post["content"].as_str().is_some_and(|s| {
                                s.contains(if unsupported_approval {
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
                if unsupported_approval {
                    ":decline:"
                } else {
                    ":accept:"
                },
            );
            let interaction = |id: &str, custom: &str| json!({"id":id,"token":"token","application_id":"123","guild_id":"1","channel_id":"4","member":{"user":{"id":"2"}},"data":{"custom_id":custom}});
            if mcp_approval {
                controller
                    .handle_interaction(interaction("1001", detail))
                    .await
                    .unwrap();
                assert!(callbacks.lock().unwrap().iter().any(|body| {
                    body["data"]["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("mcp_tool_call_approval_item-one"))
                }));
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
