//! Inventory and validate image-generation outputs from the exact Codex Turn.
//! Neither filesystem scanning nor answer-text path extraction is allowed.
use crate::codex_execution::TurnSnapshot;
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedImageStatus {
    Ready(Vec<u8>),
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedImage {
    pub item_id: String,
    pub ordinal: usize,
    pub status: GeneratedImageStatus,
}

pub fn inventory(
    snapshot: &TurnSnapshot,
    max_count: usize,
    max_bytes: usize,
) -> Result<Vec<GeneratedImage>> {
    ensure!(
        snapshot.items_view_full,
        "Codex image inventory is incomplete"
    );
    ensure!(
        max_count > 0 && max_bytes > 0,
        "generated image limits are invalid"
    );
    let mut result = Vec::new();
    let mut ids = HashSet::new();
    for item in snapshot
        .items
        .iter()
        .filter(|item| item["type"] == "imageGeneration")
    {
        ensure!(
            result.len() < max_count,
            "generated image count limit exceeded"
        );
        let id = item["id"].as_str().context("generated image ID missing")?;
        ensure!(
            !id.is_empty() && id.len() <= 256 && ids.insert(id.to_owned()),
            "generated image identity invalid"
        );
        let status = match item["status"].as_str() {
            Some("completed") => {
                let encoded = item["result"]
                    .as_str()
                    .context("generated image content missing")?;
                ensure!(
                    encoded.len() <= max_bytes.div_ceil(3).saturating_mul(4),
                    "generated image is too large"
                );
                let bytes = STANDARD
                    .decode(encoded)
                    .context("generated image is not base64")?;
                ensure!(bytes.len() <= max_bytes, "generated image is too large");
                validate_png(&bytes)?;
                GeneratedImageStatus::Ready(bytes)
            }
            Some("failed" | "cancelled") => GeneratedImageStatus::Failed,
            _ => GeneratedImageStatus::Unknown,
        };
        result.push(GeneratedImage {
            item_id: id.into(),
            ordinal: result.len(),
            status,
        });
    }
    Ok(result)
}

fn validate_png(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() >= 33
            && bytes.starts_with(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR")
            && bytes[16..20] != [0; 4]
            && bytes[20..24] != [0; 4],
        "generated image is not a valid PNG header"
    );
    Ok(())
}

pub fn has_images(items: &[Value]) -> bool {
    items.iter().any(|item| item["type"] == "imageGeneration")
}
