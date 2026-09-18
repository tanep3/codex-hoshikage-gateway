//! Model catalog is a control operation, independent of the two Turn slots.
use crate::{
    codex_execution::CodexExecution,
    codex_transport::{CodexTransport, LaunchConfig},
};
use anyhow::{Context, Result, ensure};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectModel {
    pub id: String,
    pub display_name: String,
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
        raw.into_iter()
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
                Ok(DirectModel {
                    id: id.into(),
                    display_name: display.into(),
                })
            })
            .collect()
    }

    pub async fn validate(&self, id: &str) -> Result<()> {
        ensure!(
            self.list().await?.iter().any(|model| model.id == id),
            "requested Codex model is unavailable"
        );
        Ok(())
    }
}
