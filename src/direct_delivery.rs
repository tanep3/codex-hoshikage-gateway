//! Delivery of the same saved answer after a Discord failure or restart.
use crate::{
    delivery::{Delivery, chunks},
    direct_content::DirectContent,
    domain::RequestState,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;

impl Delivery {
    pub async fn direct_answer(
        &self,
        request_id: &str,
        discord_thread_id: &str,
        max_bytes: usize,
    ) -> Result<bool> {
        let request = self.store.request(request_id).await?;
        ensure!(
            request.thread_id == discord_thread_id,
            "answer destination mismatch"
        );
        ensure!(
            request.state == RequestState::Completed,
            "Codex answer is not confirmed complete"
        );
        let saved = self
            .store
            .direct_answer(request_id.to_owned())
            .await?
            .context("confirmed answer has no saved content")?;
        let state_dir = self
            .store
            .path
            .parent()
            .context("database has no state directory")?;
        let content = DirectContent::new(state_dir)?.read_answer(&saved, max_bytes)?;
        let parts = chunks(&content);
        for (index, part) in parts.iter().enumerate() {
            if !self
                .text(
                    request_id,
                    discord_thread_id,
                    "answer",
                    index as i64,
                    part,
                    json!([]),
                )
                .await?
            {
                return Ok(false);
            }
        }
        self.trim_answer(request_id, discord_thread_id, parts.len())
            .await
    }
}
