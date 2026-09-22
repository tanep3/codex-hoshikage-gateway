//! Model catalog is a control operation, independent of the two Turn slots.
use crate::{
    codex_execution::CodexExecution,
    codex_transport::{CodexTransport, LaunchConfig},
};
use anyhow::{Context, Result, ensure};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectReasoningEffort {
    pub id: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectModel {
    pub id: String,
    pub display_name: String,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<DirectReasoningEffort>,
}

#[derive(Clone)]
pub struct DirectModelCatalog {
    pub launch: LaunchConfig,
}

impl DirectModelCatalog {
    pub async fn list(&self) -> Result<Vec<DirectModel>> {
        let transport = CodexTransport::launch(&self.launch).await?;
        let result = CodexExecution::new(transport.clone()).list_models().await;
        let _ = transport.shutdown().await;
        let raw = result?;
        ensure!(raw.len() <= 10000, "Codex model catalog is too large");
        let models = raw
            .into_iter()
            .map(|item| {
                let id = item["id"].as_str().context("Codex model ID missing")?;
                let display = item["displayName"]
                    .as_str()
                    .context("Codex model name missing")?;
                ensure!(
                    !id.is_empty()
                        && id.len() <= 128
                        && !display.is_empty()
                        && display.len() <= 256,
                    "Codex model identity invalid"
                );
                let default_effort = item["defaultReasoningEffort"]
                    .as_str()
                    .context("Codex default reasoning effort missing")?;
                let effort_items = item["supportedReasoningEfforts"]
                    .as_array()
                    .context("Codex reasoning effort catalog missing")?;
                ensure!(
                    !effort_items.is_empty() && effort_items.len() <= 32,
                    "Codex reasoning effort catalog invalid"
                );
                let mut seen = HashSet::new();
                let efforts = effort_items
                    .iter()
                    .map(|entry| {
                        let effort = entry["reasoningEffort"]
                            .as_str()
                            .context("Codex reasoning effort missing")?;
                        let description = entry["description"]
                            .as_str()
                            .context("Codex reasoning effort description missing")?;
                        ensure!(
                            !effort.is_empty()
                                && effort.len() <= 64
                                && description.len() <= 256
                                && seen.insert(effort.to_owned()),
                            "Codex reasoning effort identity invalid"
                        );
                        Ok(DirectReasoningEffort {
                            id: effort.into(),
                            description: description.into(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                ensure!(
                    efforts.iter().any(|effort| effort.id == default_effort),
                    "Codex default reasoning effort is unsupported"
                );
                Ok(DirectModel {
                    id: id.into(),
                    display_name: display.into(),
                    default_reasoning_effort: default_effort.into(),
                    supported_reasoning_efforts: efforts,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut ids = HashSet::new();
        ensure!(
            models.iter().all(|model| ids.insert(model.id.clone())),
            "duplicate Codex model ID"
        );
        Ok(models)
    }

    pub async fn find(&self, id: &str) -> Result<DirectModel> {
        self.list()
            .await?
            .into_iter()
            .find(|model| model.id == id)
            .context("requested Codex model is unavailable")
    }

    pub async fn validate(&self, id: &str) -> Result<()> {
        self.find(id).await.map(|_| ())
    }

    pub async fn validate_effort(&self, model_id: &str, effort: &str) -> Result<()> {
        let model = self.find(model_id).await?;
        ensure!(
            model
                .supported_reasoning_efforts
                .iter()
                .any(|candidate| candidate.id == effort),
            "requested reasoning effort is unavailable for the selected model"
        );
        Ok(())
    }
}
