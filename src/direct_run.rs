//! Durable dispatch of a Gateway request to a dedicated Codex child.
//!
//! This does not own Discord admission, approval decisions, or delivery.
use crate::{
    codex_execution::{CodexExecution, ExecutionOptions, TurnIdentity, TurnSnapshot},
    codex_transport::{CodexRuntimePool, Event, RuntimeLease, TransportError},
    direct_approval::{DirectInteraction, ManualDecision},
    direct_content::DirectContent,
    direct_image_store::{ImageRecord, ImageRecordState},
    direct_images::{GeneratedImageStatus, inventory},
    direct_input::app_server_input,
    direct_workspace::ensure_conversation_workspace,
    files::PreparedInput,
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct DirectRunService {
    pub store: Store,
    pub pool: CodexRuntimePool,
    pub content: DirectContent,
    pub state_dir: PathBuf,
    pub output_limit: usize,
    pub image_max_count: usize,
    pub image_max_bytes: usize,
}

pub struct ActiveRun {
    pub request_id: String,
    pub identity: TurnIdentity,
    lease: RuntimeLease,
    events: broadcast::Receiver<Event>,
    _on_drop: UnknownOnDrop,
}

struct UnknownOnDrop {
    store: Store,
    request_id: String,
    armed: AtomicBool,
}
impl UnknownOnDrop {
    fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }
}
impl Drop for UnknownOnDrop {
    fn drop(&mut self) {
        if !self.armed.load(Ordering::Acquire) {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let store = self.store.clone();
            let id = self.request_id.clone();
            handle.spawn(async move {
                let _ = store
                    .mark_direct_unknown(id, "dispatch_task_cancelled".into())
                    .await;
            });
        }
    }
}

impl DirectRunService {
    pub async fn start_prepared(
        &self,
        request_id: String,
        discord_thread_id: String,
        options: ExecutionOptions,
        prepared: &PreparedInput,
    ) -> Result<ActiveRun> {
        let input = app_server_input(&prepared.input)?;
        self.start(request_id, discord_thread_id, options, input)
            .await
    }

    /// Persist the exact App Server request before any approval card is shown.
    pub async fn register_interaction(
        &self,
        run: &ActiveRun,
        event: &Event,
    ) -> Result<Option<(String, DirectInteraction)>> {
        self.register_interaction_with_evidence(run, event, None)
            .await
    }

    pub async fn register_interaction_with_evidence(
        &self,
        run: &ActiveRun,
        event: &Event,
        evidence: Option<Value>,
    ) -> Result<Option<(String, DirectInteraction)>> {
        let Some(mut interaction) = run.interaction(event)? else {
            return Ok(None);
        };
        if let Some(evidence) = evidence {
            interaction.bind_mcp_evidence(evidence)?;
        }
        let id = self
            .store
            .record_direct_interaction(run.request_id.clone(), interaction.clone())
            .await?;
        Ok(Some((id, interaction)))
    }

    /// The action is bound to the active Run, displayed fingerprint and one
    /// upstream RPC request. Even a definite write failure stays fenced after
    /// the durable SENDING commit.
    pub async fn reply_manual(
        &self,
        run: &ActiveRun,
        interaction_id: String,
        expected_fingerprint: String,
        decision: ManualDecision,
    ) -> Result<()> {
        let rpc_id = self
            .store
            .begin_direct_approval_reply(
                run.request_id.clone(),
                interaction_id.clone(),
                expected_fingerprint,
                decision,
            )
            .await?;
        let wire_decision = match decision {
            ManualDecision::AcceptOnce => "accept",
            ManualDecision::Decline => "decline",
            ManualDecision::Cancel => "cancel",
        };
        if let Err(error) = run
            .transport()
            .respond(rpc_id, json!({"decision":wire_decision}))
            .await
        {
            let _ = self
                .store
                .mark_direct_approval_unknown(interaction_id)
                .await;
            return Err(error.into());
        }
        self.store.mark_direct_approval_sent(interaction_id).await
    }

    pub async fn reply_mcp_tool(
        &self,
        run: &ActiveRun,
        interaction_id: String,
        interaction: DirectInteraction,
        decision: ManualDecision,
    ) -> Result<()> {
        let result = interaction.mcp_tool_decision(decision)?;
        ensure!(
            interaction.thread_id == run.identity.thread_id
                && interaction.turn_id == run.identity.turn_id,
            "MCP confirmation belongs to another run"
        );
        let rpc_id = self
            .store
            .begin_direct_mcp_reply(
                run.request_id.clone(),
                interaction_id.clone(),
                interaction.fingerprint,
                decision,
            )
            .await?;
        if let Err(error) = run.transport().respond(rpc_id, result).await {
            let _ = self
                .store
                .mark_direct_approval_unknown(interaction_id)
                .await;
            return Err(error.into());
        }
        self.store.mark_direct_approval_sent(interaction_id).await
    }

    /// The caller owns admission and authorizes this Discord conversation.
    /// The send-intent commit precedes thread/start or turn/start. Any uncertain
    /// outcome is fenced in SQLite, never transparently retried.
    pub async fn start(
        &self,
        request_id: String,
        discord_thread_id: String,
        mut options: ExecutionOptions,
        input: Vec<Value>,
    ) -> Result<ActiveRun> {
        ensure!(!input.is_empty(), "empty Codex input");
        let lease = self.pool.acquire().await.map_err(anyhow::Error::new)?;
        options.cwd = ensure_conversation_workspace(&self.state_dir, &discord_thread_id)?;
        let intent = self
            .store
            .prepare_direct(
                request_id.clone(),
                discord_thread_id,
                options.cwd.clone(),
                options.model_provider.clone(),
            )
            .await?;
        let dispatch = match self
            .store
            .begin_direct_send(request_id.clone(), intent)
            .await
        {
            Ok(dispatch) => dispatch,
            Err(error) => {
                if self
                    .store
                    .request(&request_id)
                    .await
                    .is_ok_and(|r| r.state == crate::domain::RequestState::Sending)
                {
                    self.store
                        .mark_direct_unknown(request_id.clone(), "send_boundary_failed".into())
                        .await?;
                }
                return Err(error);
            }
        };
        options.model = dispatch.selected_model.clone();
        let on_drop = UnknownOnDrop {
            store: self.store.clone(),
            request_id: request_id.clone(),
            armed: AtomicBool::new(true),
        };
        let execution = CodexExecution::new(lease.transport().clone());
        let thread_result = if let Some(thread) = dispatch.codex_thread_id {
            execution
                .resume_thread(&thread, &options)
                .await
                .map(|()| thread)
        } else {
            execution.start_thread(&options).await
        };
        let thread = match thread_result {
            Ok(thread) => thread,
            Err(error) => {
                self.store
                    .mark_direct_unknown(
                        request_id.clone(),
                        format!("thread_start_or_resume: {error}"),
                    )
                    .await?;
                on_drop.disarm();
                return Err(error.into());
            }
        };
        if let Err(error) = self
            .store
            .record_direct_thread(request_id.clone(), thread.clone())
            .await
        {
            self.store
                .mark_direct_unknown(request_id.clone(), "thread_record_failed".into())
                .await?;
            on_drop.disarm();
            return Err(error);
        }
        let events = lease.transport().subscribe();
        let started = execution.start_turn(&thread, &options, input).await;
        let identity = match started {
            Ok(identity) => identity,
            Err(error) => {
                self.store
                    .mark_direct_unknown(request_id.clone(), format!("turn_start: {error}"))
                    .await?;
                on_drop.disarm();
                return Err(error.into());
            }
        };
        if let Err(error) = self
            .store
            .acknowledge_direct_turn(
                request_id.clone(),
                identity.thread_id.clone(),
                identity.turn_id.clone(),
            )
            .await
        {
            self.store
                .mark_direct_unknown(
                    request_id.clone(),
                    "turn_acknowledgement_not_durable".into(),
                )
                .await?;
            on_drop.disarm();
            return Err(error);
        }
        Ok(ActiveRun {
            request_id,
            identity,
            lease,
            events,
            _on_drop: on_drop,
        })
    }

    /// Persist final text before marking the request terminal. `thread/read`
    /// must return this exact thread and turn; a missing result remains pending.
    pub async fn confirm_terminal(&self, run: &ActiveRun) -> Result<TurnSnapshot> {
        let execution = CodexExecution::new(run.lease.transport().clone());
        let snapshot = execution
            .read_turn(&run.identity)
            .await
            .map_err(anyhow::Error::new)?;
        ensure!(
            matches!(
                snapshot.status.as_str(),
                "completed" | "failed" | "interrupted"
            ),
            "Codex turn has not ended"
        );
        ensure!(
            snapshot.items_view_full,
            "Codex output inventory is incomplete"
        );
        match inventory(&snapshot, self.image_max_count, self.image_max_bytes) {
            Ok(images) => {
                let mut records = Vec::with_capacity(images.len());
                for image in images {
                    let state = match image.status {
                        GeneratedImageStatus::Ready(bytes) => match self.content.save_image(
                            &run.request_id,
                            &image.item_id,
                            &bytes,
                            self.image_max_bytes,
                        ) {
                            Ok(saved) => ImageRecordState::Ready(saved),
                            Err(_) => ImageRecordState::Unknown,
                        },
                        GeneratedImageStatus::Failed => ImageRecordState::Failed,
                        GeneratedImageStatus::Unknown => ImageRecordState::Unknown,
                    };
                    records.push(ImageRecord {
                        item_id: image.item_id,
                        ordinal: image.ordinal,
                        state,
                    });
                }
                self.store
                    .record_direct_images(run.request_id.clone(), run.identity.clone(), records)
                    .await?;
            }
            Err(_) => {
                self.store
                    .mark_direct_image_unknown(
                        run.request_id.clone(),
                        run.identity.clone(),
                        "inventory_unavailable",
                    )
                    .await?
            }
        }
        let answer = if snapshot.status == "completed" || snapshot.final_text.is_some() {
            Some(self.content.save_answer(
                &run.request_id,
                snapshot.final_text.as_deref().unwrap_or(""),
                self.output_limit,
            )?)
        } else {
            None
        };
        self.store
            .finish_direct(
                run.request_id.clone(),
                run.identity.clone(),
                snapshot.status.clone(),
                answer,
            )
            .await?;
        run._on_drop.disarm();
        self.store
            .close_direct_approvals(run.request_id.clone())
            .await?;
        Ok(snapshot)
    }
}

impl ActiveRun {
    pub fn transport(&self) -> &crate::codex_transport::CodexTransport {
        self.lease.transport()
    }
    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        self.events.recv().await
    }
    pub fn interaction(&self, event: &Event) -> Result<Option<DirectInteraction>> {
        DirectInteraction::from_event(event, &self.identity)
    }
    pub async fn interrupt(&self) -> Result<(), TransportError> {
        CodexExecution::new(self.lease.transport().clone())
            .interrupt(&self.identity)
            .await
    }
    pub async fn steer(&self, input: Vec<Value>) -> Result<(), TransportError> {
        CodexExecution::new(self.lease.transport().clone())
            .steer(&self.identity, input)
            .await
    }
    pub async fn shutdown(self) -> Result<()> {
        self.lease
            .shutdown()
            .await
            .context("Codex child shutdown failed")
    }
}
