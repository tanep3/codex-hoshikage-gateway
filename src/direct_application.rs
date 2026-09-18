//! Discord admission/execution boundary for the Gateway-owned Codex runtime.
//! This is kept separate from the Proxy-backed application during cutover.
use crate::{
    delivery::Delivery,
    direct_config::DirectConfig,
    direct_delivery::DirectImageDelivery,
    direct_run::{ActiveRun, DirectRunService},
    discord::{Discord, input_message, snowflake},
    domain,
    files::Files,
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;

#[derive(Clone)]
pub struct DirectApplication {
    pub cfg: DirectConfig,
    pub store: Store,
    pub discord: Discord,
    pub files: Files,
    pub runs: DirectRunService,
    pub delivery: Delivery,
}

#[derive(Debug)]
pub struct DirectDeliveryResult {
    pub answer_delivered: bool,
    pub images: DirectImageDelivery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectAdmission {
    Ignored,
    Duplicate,
    Accepted(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectControlOutcome {
    WaitingCancelled,
    InterruptAccepted,
    Paused,
    NothingActive,
    ActiveRunUnavailable,
}

impl DirectApplication {
    /// Reserve the Discord message before attachment validation. A second
    /// event carrying the same Message ID can never create another Codex turn.
    pub async fn admit_message(&self, raw: &Value) -> Result<DirectAdmission> {
        if raw["guild_id"] != self.cfg.discord.guild_id
            || raw["author"]["id"] != self.cfg.discord.allowed_user_id
            || raw["author"]["bot"] == true
            || !raw["webhook_id"].is_null()
            || crate::commands::is_text_control(raw["content"].as_str().unwrap_or(""))
            || !self
                .discord
                .should_respond(raw, self.cfg.discord.response_mode)
        {
            return Ok(DirectAdmission::Ignored);
        }
        let message = input_message(raw, &self.cfg.discord.guild_id)?;
        self.verify_location(&message.thread_id).await?;
        self.store
            .add_conversation(
                message.thread_id.clone(),
                crate::storage::PROXY_SCOPE.into(),
            )
            .await?;
        let Some(id) = self
            .store
            .reserve(
                message.id.clone(),
                message.thread_id.clone(),
                message.metadata_digest(),
                self.cfg.limits.clone(),
            )
            .await?
        else {
            return Ok(DirectAdmission::Duplicate);
        };
        let result = async {
            let prepared = self.files.prepare(&message, &self.cfg.limits).await?;
            self.store
                .finalize(
                    id.clone(),
                    prepared.metadata_digest,
                    prepared.digest,
                    prepared.attachments,
                )
                .await
        }
        .await;
        if let Err(error) = result {
            self.store
                .reject_admission(id, "direct_input_validation_failed")
                .await?;
            return Err(error);
        }
        Ok(DirectAdmission::Accepted(id))
    }

    pub async fn cancel(
        &self,
        interaction: &str,
        thread: &str,
        active: Option<&ActiveRun>,
    ) -> Result<DirectControlOutcome> {
        self.verify_location(thread).await?;
        let (kind, target) = self
            .store
            .cancel_latest(interaction.into(), thread.into())
            .await?;
        match kind.as_str() {
            "waiting" => Ok(DirectControlOutcome::WaitingCancelled),
            "empty" => Ok(DirectControlOutcome::NothingActive),
            "active" => {
                let Some(run) =
                    active.filter(|run| Some(run.request_id.as_str()) == target.as_deref())
                else {
                    return Ok(DirectControlOutcome::ActiveRunUnavailable);
                };
                self.interrupt_bound(interaction, run).await?;
                Ok(DirectControlOutcome::InterruptAccepted)
            }
            _ => anyhow::bail!("unknown cancel result"),
        }
    }

    pub async fn stop(
        &self,
        interaction: &str,
        thread: &str,
        active: Option<&ActiveRun>,
    ) -> Result<DirectControlOutcome> {
        self.verify_location(thread).await?;
        let target = self.store.stop(interaction.into(), thread.into()).await?;
        let Some(target) = target else {
            return Ok(DirectControlOutcome::Paused);
        };
        let Some(run) = active.filter(|run| run.request_id == target.id) else {
            return Ok(DirectControlOutcome::ActiveRunUnavailable);
        };
        self.interrupt_bound(interaction, run).await?;
        Ok(DirectControlOutcome::InterruptAccepted)
    }

    async fn interrupt_bound(&self, interaction: &str, run: &ActiveRun) -> Result<()> {
        self.store
            .begin_direct_interrupt(
                interaction.into(),
                run.request_id.clone(),
                run.identity.clone(),
            )
            .await?;
        let result = run.interrupt().await;
        self.store
            .finish_direct_control(interaction.into(), result.is_ok())
            .await?;
        result.map_err(anyhow::Error::new)
    }

    pub async fn steer(
        &self,
        interaction: &str,
        thread: &str,
        active: &ActiveRun,
        input: Vec<Value>,
    ) -> Result<()> {
        self.verify_location(thread).await?;
        ensure!(
            !active.identity.thread_id.is_empty()
                && self.store.request(&active.request_id).await?.thread_id == thread,
            "steer destination mismatch"
        );
        let digest = domain::digest(serde_json::to_vec(&input)?.as_slice());
        self.store
            .begin_direct_steer(
                interaction.into(),
                active.request_id.clone(),
                active.identity.clone(),
                digest,
            )
            .await?;
        let result = active.steer(input).await;
        self.store
            .finish_direct_control(interaction.into(), result.is_ok())
            .await?;
        result.map_err(anyhow::Error::new)
    }

    pub async fn resume(&self, interaction: &str, thread: &str) -> Result<bool> {
        self.verify_location(thread).await?;
        let Some((operation, _)) = self
            .store
            .reserve_resume(interaction.into(), thread.into())
            .await?
        else {
            return Ok(false);
        };
        self.store.apply_resume(operation).await
    }
    pub async fn choose_model(&self, thread: &str, interaction: &str, model: &str) -> Result<bool> {
        self.verify_location(thread).await?;
        crate::direct_models::DirectModelCatalog {
            launch: self.cfg.launch(),
        }
        .validate(model)
        .await?;
        self.store
            .select_direct_model(thread.into(), interaction.into(), model.into())
            .await
    }
    pub async fn verify_location(&self, channel: &str) -> Result<()> {
        let value = self
            .discord
            .get(&format!("/channels/{}", snowflake(channel)?))
            .await?;
        ensure!(
            value["id"] == channel && value["guild_id"] == self.cfg.discord.guild_id,
            "Discord conversation identity mismatch"
        );
        match value["type"].as_u64() {
            Some(0) => Ok(()),
            Some(11 | 12) => {
                ensure!(
                    value["thread_metadata"]["archived"] != true
                        && value["thread_metadata"]["locked"] != true,
                    "Discord conversation closed"
                );
                let parent = value["parent_id"]
                    .as_str()
                    .context("Discord parent missing")?;
                let parent_value = self
                    .discord
                    .get(&format!("/channels/{}", snowflake(parent)?))
                    .await?;
                ensure!(
                    parent_value["id"] == parent
                        && parent_value["guild_id"] == self.cfg.discord.guild_id
                        && matches!(parent_value["type"].as_u64(), Some(0 | 15)),
                    "Discord parent identity mismatch"
                );
                Ok(())
            }
            _ => anyhow::bail!("Discord location cannot host a Codex conversation"),
        }
    }

    /// Re-fetches the original Discord message and revalidates every attached
    /// byte before crossing the non-idempotent Codex send boundary.
    pub async fn start_queued(&self, request_id: &str) -> Result<ActiveRun> {
        let request = self.store.request(request_id).await?;
        ensure!(
            request.state == crate::domain::RequestState::Queued,
            "direct request is not queued"
        );
        self.verify_location(&request.thread_id).await?;
        let message = self
            .discord
            .message(
                &request.thread_id,
                &request.message_id,
                &self.cfg.discord.guild_id,
            )
            .await?;
        if message.user_id != self.cfg.discord.allowed_user_id {
            self.store
                .fail_direct_before_send(request.id.clone(), "input_author_changed")
                .await?;
            anyhow::bail!("Discord author changed");
        }
        let limits = self.store.input_limits(request.id.clone()).await?;
        let prepared = self.files.prepare(&message, &limits).await?;
        if prepared.digest != request.input_digest {
            self.store
                .fail_direct_before_send(request.id.clone(), "input_changed_before_send")
                .await?;
            anyhow::bail!("Discord input changed after admission");
        }
        self.runs
            .start_prepared(
                request.id,
                request.thread_id,
                self.cfg.execution(),
                &prepared,
            )
            .await
    }

    /// The caller observes the exact Turn terminal event, and handles any
    /// App Server approval/control requests while `ActiveRun` is retained.
    pub async fn finish_and_deliver(&self, run: &ActiveRun) -> Result<DirectDeliveryResult> {
        self.runs.confirm_terminal(run).await?;
        let request = self.store.request(&run.request_id).await?;
        let answer = if request.state == crate::domain::RequestState::Completed {
            self.delivery
                .direct_answer(
                    &request.id,
                    &request.thread_id,
                    self.cfg.limits.output_bytes,
                )
                .await
        } else {
            Ok(false)
        };
        let images = self
            .delivery
            .direct_images(
                &request.id,
                &request.thread_id,
                &self.cfg.discord.guild_id,
                self.cfg.limits.artifact_bytes,
            )
            .await?;
        let answer_delivered = answer?;
        Ok(DirectDeliveryResult {
            answer_delivered,
            images,
        })
    }
}
