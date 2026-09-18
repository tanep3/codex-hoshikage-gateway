//! Converts validated Discord input to the Codex App Server `UserInput`
//! schema. The existing attachment validator deliberately keeps its own
//! transport-neutral receipt/digest; this is the only wire conversion.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub fn app_server_input(prepared: &Value) -> Result<Vec<Value>> {
    let messages = prepared
        .as_array()
        .context("validated input is not an array")?;
    ensure!(
        messages.len() == 1 && messages[0]["role"] == "user",
        "validated user input shape changed"
    );
    let content = messages[0]["content"]
        .as_array()
        .context("validated content is not an array")?;
    ensure!(!content.is_empty(), "empty Codex input");
    let mut result = Vec::with_capacity(content.len());
    for item in content {
        let converted = match item["type"].as_str() {
            Some("input_text") => {
                let text = item["text"].as_str().context("validated text missing")?;
                json!({"type":"text","text":text})
            }
            Some("input_image") => {
                let url = item["image_url"]
                    .as_str()
                    .context("validated image missing")?;
                ensure!(
                    [
                        "data:image/png;base64,",
                        "data:image/jpeg;base64,",
                        "data:image/webp;base64,"
                    ]
                    .iter()
                    .any(|prefix| url.starts_with(prefix)),
                    "validated image origin changed"
                );
                let detail = item["detail"].as_str().unwrap_or("high");
                ensure!(
                    matches!(detail, "auto" | "low" | "high" | "original"),
                    "unsupported image detail"
                );
                json!({"type":"image","url":url,"detail":detail})
            }
            _ => anyhow::bail!("unsupported validated input type"),
        };
        result.push(converted);
    }
    Ok(result)
}
