//! Human-readable approval cards and transient Discord activity indication.
use crate::{application::App, domain, proxy::path_id};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Duration};

fn approval_paths(d: &Value) -> Option<Vec<&str>> {
    // A grant root extends the approval scope; do not infer it from file paths.
    if !d["grantRoot"].is_null() {
        return None;
    }
    if let Some(changes) = d["changes"].as_object() {
        let paths: Vec<_> = changes.keys().map(String::as_str).collect();
        return (!paths.is_empty() && paths.iter().all(|p| !p.trim().is_empty())).then_some(paths);
    }
    let paths: Vec<_> = d["paths"]
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<_>>()?;
    (!paths.is_empty() && paths.iter().all(|p| !p.trim().is_empty())).then_some(paths)
}

pub fn approval_can_accept(view: &Value) -> bool {
    let d = &view["details"];
    let described =
        d["command"].as_str().is_some_and(|s| !s.trim().is_empty()) || approval_paths(d).is_some();
    described
        && view["available_decisions"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "accept"))
}

pub fn approval_description(view: &Value) -> String {
    let d = &view["details"];
    let title = match d["kind"].as_str() {
        Some("command") => "コマンドの実行許可",
        Some("file_change") => "ファイル変更の許可",
        _ if approval_paths(d).is_some() => "ファイル変更の許可",
        _ => "操作の実行許可",
    };
    let mut lines = vec![format!("**{title}が必要です**")];
    if let Some(reason) = d["reason"].as_str().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("目的：{}", safe_text(reason, 500)));
    }
    if let Some(command) = d["command"].as_str().filter(|s| !s.trim().is_empty()) {
        lines.push(format!(
            "実行する内容：\n```text\n{}\n```",
            safe_text(command, 950)
        ));
    } else if let Some(paths) = approval_paths(d) {
        lines.push(format!(
            "対象ファイル：\n{}",
            paths
                .iter()
                .take(8)
                .map(|p| format!("• {}", safe_text(p, 100)))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        if paths.len() > 8 {
            lines.push(format!(
                "ほか{}件は省略しています。全対象を確認できない場合は取消してください。",
                paths.len() - 8
            ));
        }
    } else {
        lines.push(
            "操作内容・許可範囲を確認できないため、承認ボタンを表示していません。「取消」でこの要求を取り消し、このメッセージを運用者へ伝えてください。"
                .into(),
        );
    }
    if approval_can_accept(view) {
        lines.push("「今回のみ承認」は、この操作だけを許可します。以後の操作を自動承認する設定には変えません。".into());
    } else if approval_paths(d).is_some()
        || d["command"].as_str().is_some_and(|s| !s.trim().is_empty())
    {
        lines.push("この要求では今回のみの承認を選べません。「取消」で取り消し、運用者へ確認してください。".into());
    }
    lines.join("\n\n")
}
fn safe_text(s: &str, limit: usize) -> String {
    let normalized = s.replace('`', "ˋ").replace('@', "＠");
    let mut out = String::new();
    let mut used = 0;
    for c in normalized
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
    {
        if used + c.len_utf16() > limit {
            out.push_str("\n…（長いため省略。内容を確認できない場合は取消してください）");
            break;
        }
        used += c.len_utf16();
        out.push(c);
    }
    out
}

impl App {
    pub async fn approval_ui_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{}}
            if !self.connected.load(Ordering::SeqCst) || self.recovery.load(Ordering::SeqCst) {
                continue;
            }
            let ids=self.store.call(false,|c|{let mut q=c.prepare("SELECT id FROM approvals WHERE state IN ('PENDING','DECISION_PENDING') ORDER BY expires_at LIMIT 40")?;Ok(q.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
            for id in ids {
                if self.cancel.is_cancelled() {
                    return Ok(());
                }
                if self.refresh_approval(&id).await.is_err() {
                    tracing::warn!(event = "approval_display_refresh_failed");
                }
            }
        }
    }
    pub async fn refresh_approval(&self, aid: &str) -> Result<()> {
        let s = self.settings().await;
        let a = aid.to_owned();
        let (rid, local): (String, String) = self
            .store
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT request_id,state FROM approvals WHERE id=?1",
                    [a],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await?;
        let r = self.store.request(&rid).await?;
        self.authorized_thread(&r.thread_id).await?;
        let result = s
            .proxy
            .get(&format!("/v1/codex/approvals/{}", path_id(aid)?))
            .await;
        let (text, buttons, closed) = match result {
            Ok(v) => {
                ensure!(
                    v["id"] == aid
                        && v["details"]["threadId"].as_str() == r.proxy_thread_id.as_deref()
                        && v["details"]["turnId"].as_str() == r.turn_id.as_deref(),
                    "approval target mismatch"
                );
                match v["state"].as_str() {
                    Some("pending")
                        if !r.state.terminal()
                            && local == "PENDING"
                            && v["expires_at_ms"]
                                .as_i64()
                                .is_none_or(|t| t > domain::now_ms()) =>
                    {
                        let mut buttons = vec![];
                        for (decision, label, style) in [
                            ("accept", "今回のみ承認", 3),
                            ("decline", "拒否", 4),
                            ("cancel", "取消", 2),
                        ] {
                            if decision == "accept" && !approval_can_accept(&v) {
                                continue;
                            }
                            if v["available_decisions"]
                                .as_array()
                                .is_some_and(|a| a.iter().any(|v| v == decision))
                            {
                                buttons.push(json!({"type":2,"style":style,"label":label,"custom_id":format!("approval:{aid}:{decision}")}));
                            }
                        }
                        (self.redact(&s, &approval_description(&v)), buttons, false)
                    }
                    Some("approved") if v["reply_status"] == "written" => (
                        "承認をCodexへ送りました。続きの回答はこの会話に届きます。".into(),
                        vec![],
                        true,
                    ),
                    Some("denied") => ("この操作を拒否しました。".into(), vec![], true),
                    Some("cancelled") => ("この承認を取り消しました。".into(), vec![], true),
                    Some("expired") => (
                        "承認の期限が切れました。このボタンからは実行できません。".into(),
                        vec![],
                        true,
                    ),
                    _ if r.state.terminal() => (
                        "作業が終了したため、この承認は操作できません。".into(),
                        vec![],
                        true,
                    ),
                    _ => (
                        "承認の送信状況を確認しています。重複送信せずに待っています。".into(),
                        vec![],
                        false,
                    ),
                }
            }
            Err(_) if r.state.terminal() => (
                "作業が終了したため、この承認は操作できません。".into(),
                vec![],
                true,
            ),
            Err(_) => (
                "承認の状態を確認できません。接続の回復を待っています。".into(),
                vec![],
                false,
            ),
        };
        let components = if buttons.is_empty() {
            json!([])
        } else {
            json!([{"type":1,"components":buttons}])
        };
        if self
            .delivery
            .text(aid, &r.thread_id, "approval", 0, &text, components)
            .await?
            && closed
        {
            let a = aid.to_owned();
            self.store
                .call(true, move |c| {
                    c.execute("UPDATE approvals SET state='CLOSED' WHERE id=?1", [a])?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }
    pub async fn typing_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(7));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{}}
            if !self.connected.load(Ordering::SeqCst)
                || self.recovery.load(Ordering::SeqCst)
                || !self.settings().await.proxy.gate.is_ready()
            {
                continue;
            }
            self.typing_tick().await?;
        }
    }
    pub async fn typing_tick(&self) -> Result<()> {
        let threads=self.store.call(false,|c|{let mut q=c.prepare("SELECT DISTINCT r.thread_id FROM requests r WHERE r.state IN ('SENDING','RUNNING') AND NOT EXISTS(SELECT 1 FROM approvals a WHERE a.request_id=r.id AND a.state IN ('PENDING','DECISION_PENDING')) AND NOT EXISTS(SELECT 1 FROM mcp_interactions m WHERE m.request_id=r.id AND m.closed=0 AND m.state IN ('pending','sending')) LIMIT 2")?;Ok(q.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
        for thread in threads {
            if self.authorized_thread(&thread).await.is_err() {
                continue;
            }
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                self.discord.api(
                    reqwest::Method::POST,
                    &format!("/channels/{}/typing", crate::discord::snowflake(&thread)?),
                    None,
                ),
            )
            .await;
        }
        Ok(())
    }
}
