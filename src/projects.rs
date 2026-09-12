use crate::{application::App, domain};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
pub(crate) struct PendingProject {
    token: String,
    operation: String,
    models: Vec<String>,
    created: Instant,
}
const MENU_TTL: Duration = Duration::from_secs(600);
impl App {
    pub(crate) async fn project_menu(&self, v: &Value) -> Option<Value> {
        let channel = v["channel_id"].as_str()?;
        let custom = v["data"]["custom_id"].as_str().unwrap_or("");
        let is_project = (v["data"]["name"] == "model"
            && v["data"]["options"].as_array().is_none_or(|a| a.is_empty()))
            || v["content"].as_str().is_some_and(|s| s.trim() == "/model");
        if !is_project && !custom.starts_with("model-page:") {
            return None;
        }
        let pending = self.pending_projects.lock().await;
        let p = pending.get(channel)?;
        if p.created.elapsed() >= MENU_TTL {
            return None;
        }
        let page = if let Some(rest) = custom.strip_prefix("model-page:") {
            let (token, page) = rest.split_once(':')?;
            if token != p.token {
                return None;
            }
            page.parse::<usize>().ok()?
        } else {
            0
        };
        let pages = p.models.len().div_ceil(25);
        if page >= pages {
            return None;
        }
        let options: Vec<_> = p.models.iter().enumerate().skip(page*25).take(25)
            .map(|(index,m)|json!({"label":m.chars().take(100).collect::<String>(),"value":index.to_string()})).collect();
        let mut rows = vec![
            json!({"type":1,"components":[{"type":3,"custom_id":format!("model-model:{}",p.token),"placeholder":format!("モデルを選択 ({}/{})",page+1,pages),"min_values":1,"max_values":1,"options":options}]}),
        ];
        let mut buttons = Vec::new();
        if page > 0 {
            buttons.push(json!({"type":2,"style":2,"label":"前へ","custom_id":format!("model-page:{}:{}",p.token,page-1)}));
        }
        if page + 1 < pages {
            buttons.push(json!({"type":2,"style":2,"label":"次へ","custom_id":format!("model-page:{}:{}",p.token,page+1)}));
        }
        if !buttons.is_empty() {
            rows.push(json!({"type":1,"components":buttons}));
        }
        Some(json!(rows))
    }
    pub(crate) async fn start_model_menu(&self, operation: &str, channel: &str) -> Result<String> {
        let s = self.settings().await;
        let data = s.proxy.get("/v1/models").await?;
        let mut models: Vec<String> = data["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["id"].as_str())
            .filter(|m| !m.is_empty() && m.len() <= 256)
            .map(str::to_owned)
            .collect();
        models.sort();
        models.dedup();
        ensure!(!models.is_empty(), "no models available");
        let mut pending = self.pending_projects.lock().await;
        pending.retain(|_, p| p.created.elapsed() < MENU_TTL);
        ensure!(
            pending.len() < 64 || pending.contains_key(channel),
            "too many model menus"
        );
        pending.insert(
            channel.into(),
            PendingProject {
                token: domain::id(),
                operation: operation.into(),
                models,
                created: Instant::now(),
            },
        );
        let current = self.store.conversation(channel).await?.selected_model;
        Ok(format!(
            "選択中のモデル: {current}\n変更するモデルを一覧から選んでください（10分以内）。次の依頼から適用します。"
        ))
    }
    pub(crate) async fn project_selection(&self, v: &Value) -> Result<String> {
        let channel = v["channel_id"].as_str().context("channel missing")?;
        self.authorized_thread(channel).await?;
        let custom = v["data"]["custom_id"]
            .as_str()
            .context("selection missing")?;
        let (kind, rest) = custom.split_once(':').context("invalid selection")?;
        let token = rest.split(':').next().unwrap_or("");
        let mut pending = self.pending_projects.lock().await;
        let Some(p) = pending
            .get(channel)
            .filter(|p| p.token == token && p.created.elapsed() < MENU_TTL)
        else {
            return Ok(
                "この選択は期限切れ、または受付済みです。/model で確認してください。".into(),
            );
        };
        if kind == "model-page" {
            return Ok("モデルを選んでください。".into());
        }
        ensure!(
            kind == "model-model" && v["data"]["values"].as_array().is_some_and(|a| a.len() == 1),
            "invalid model choice"
        );
        let index = v["data"]["values"][0]
            .as_str()
            .context("choice missing")?
            .parse::<usize>()?;
        let model = p.models.get(index).context("invalid model index")?.clone();
        let p = pending.remove(channel).unwrap();
        drop(pending);
        self.choose_model(&p.operation, channel, Some(&model)).await
    }
}
