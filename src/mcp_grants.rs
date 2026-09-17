//! API 0.3: requester-only operation details and explicit Run-scoped grants.
use crate::{
    application::App,
    domain::{self, Request},
    proxy::{field, path_id},
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Clone, Copy, Default, Debug)]
pub struct Capabilities {
    pub details: bool,
    pub turn: bool,
    pub inline: bool,
}
impl Capabilities {
    pub fn parse(v: &Value) -> Self {
        let d = &v["mcp_operation_details"];
        let t = &v["mcp_turn_approval"];
        let inline = &v["mcp_inline_approval"];
        Self {
            inline: inline["enabled"] == true
                && inline["profile"] == "source-conversation-v1"
                && inline["max_response_bytes"].as_u64() == Some(32768)
                && inline["max_display_text_utf16_units"].as_u64() == Some(1400)
                && inline["max_display_fields"].as_u64() == Some(8)
                && inline["max_presentations_per_interaction"].as_u64() == Some(4),
            details: d["enabled"] == true
                && d["profile"] == "native-item-id-v1"
                && d["max_argument_bytes"] == 65536
                && d["disclosure"] == "requester_only",
            turn: t["enabled"] == true
                && t["profile"] == "native-item-id-v1"
                && t["max_grants"] == 16
                && t["ttl_seconds"] == 600
                && t["max_records"] == 256,
        }
    }
}
fn timestamp(v: &Value) -> Result<i64> {
    Ok(
        (OffsetDateTime::parse(v.as_str().context("timestamp")?, &Rfc3339)?.unix_timestamp_nanos()
            / 1_000_000) as i64,
    )
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    let s = v[k].as_str().context("string missing")?;
    ensure!(!s.is_empty() && s.len() <= 8192, "string limit");
    Ok(s)
}
fn canonical_scope(scope: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in [
        "instance_id",
        "recovery_generation",
        "context",
        "response_id",
        "conversation_id",
        "workspace_id",
        "turn_id",
        "input_generation",
        "config_generation",
        "server",
        "tool",
    ] {
        out.insert(key.to_owned(), scope[key].clone());
    }
    Value::Object(out)
}
fn safe(s: &str) -> String {
    s.replace('@', "＠").replace('`', "｀")
}
fn code_block(s: &str) -> String {
    let mut longest: usize = 0;
    let mut run = 0;
    for ch in s.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest.saturating_add(1).max(3));
    format!("{fence}\n{s}\n{fence}")
}
fn btn(id: String, label: &str, style: u8) -> Value {
    json!({"type":2,"custom_id":id,"label":label,"style":style})
}
fn row(buttons: Vec<Value>) -> Value {
    json!([{"type":1,"components":buttons}])
}

impl App {
    pub(crate) async fn prepare_mcp_context(
        &self,
        r: &Request,
        s: &crate::application::Settings,
    ) -> Result<()> {
        let caps = *s.proxy.v2.mcp_caps.read().unwrap();
        let modern_caps = s.proxy.v2.mcp_v06_caps.read().unwrap().clone();
        let modern = modern_caps.as_ref().is_some_and(|c| c.enabled);
        ensure!(
            !modern
                || modern_caps
                    .as_ref()
                    .is_some_and(|c| c.supports(Some(&crate::mcp_v06::Selection::guard()))),
            "選択したMCPポリシーを利用できません。Proxyの設定を確認してください"
        );
        let fixed = self.store.v06_selection(&r.id).await?;
        if modern || fixed.is_some() {
            ensure!(modern, "0.6 capability or selected policy is unavailable");
            self.store
                .fix_v06_selection(&r.id, Some(crate::mcp_v06::Selection::guard()))
                .await?;
        }
        if !modern && !caps.details && !caps.turn {
            return Ok(());
        }
        let inline = !modern && caps.inline && caps.details;
        let (id, thread, user, guild) = (
            r.id.clone(),
            r.thread_id.clone(),
            s.cfg.discord.allowed_user_id.clone(),
            s.cfg.discord.guild_id.clone(),
        );
        self.store
            .call(true, move |c| {
                let instance: String =
                    c.query_row("SELECT instance_uuid FROM schema_meta", [], |r| r.get(0))?;
                let principal = domain::digest(&serde_json::to_vec(&json!([
                    "mcp-principal-v1",
                    instance,
                    user
                ]))?);
                let channel = domain::digest(&serde_json::to_vec(&json!([
                    "mcp-channel-v1",
                    instance,
                    guild,
                    thread
                ]))?);
                c.execute(
                    "INSERT OR IGNORE INTO mcp_run_context VALUES(?1,?2,?3,?1)",
                    params![id, principal, channel],
                )?;
                if inline {
                    c.execute("INSERT OR IGNORE INTO mcp_inline_runs VALUES(?1)", [&id])?;
                }
                Ok(())
            })
            .await
    }
    pub(crate) async fn context(&self, id: &str) -> Result<Value> {
        let id = id.to_owned();
        self.store.call(false,move|c|Ok(c.query_row("SELECT principal_id,channel_id,run_id FROM mcp_run_context WHERE request_id=?1",[id],|r|Ok(json!({"principal_id":r.get::<_,String>(0)?,"channel_id":r.get::<_,String>(1)?,"run_id":r.get::<_,String>(2)?}))).optional()?.unwrap_or(Value::Null))).await
    }
    pub(crate) async fn scope_matches(&self, r: &Request, scope: &Value) -> Result<()> {
        ensure!(scope.get("context").is_some(), "context missing");
        let s = self.settings().await;
        let b = s
            .proxy
            .v2
            .binding
            .read()
            .unwrap()
            .clone()
            .context("binding missing")?;
        let thread = r.thread_id.clone();
        let (cv,ws):(String,String)=self.store.call(false,move|c|Ok(c.query_row("SELECT conversation_id,workspace_id FROM proxy_conversations WHERE thread_id=?1",[thread],|r|Ok((r.get(0)?,r.get(1)?)))?)).await?;
        ensure!(
            scope["instance_id"] == b.instance_id
                && scope["recovery_generation"] == b.generation
                && scope["response_id"].as_str() == r.response_id.as_deref()
                && scope["turn_id"].as_str() == r.turn_id.as_deref()
                && scope["conversation_id"] == cv
                && scope["workspace_id"] == ws
                && scope["context"] == self.context(&r.id).await?,
            "許可の対象がこの作業と一致しません"
        );
        for k in [
            "instance_id",
            "recovery_generation",
            "response_id",
            "turn_id",
            "conversation_id",
            "workspace_id",
            "server",
            "tool",
            "config_generation",
        ] {
            text(scope, k)?;
        }
        ensure!(
            scope["input_generation"].as_u64().is_some(),
            "input generation missing"
        );
        Ok(())
    }
    pub(crate) async fn mcp_operation(
        &self,
        local: &str,
        thread: &str,
        revision: i64,
    ) -> Result<Value> {
        let rec = self.mcp_record(local).await?;
        self.mcp_pending(&rec, thread, revision).await?;
        let s = self.settings().await;
        ensure!(
            s.proxy.v2.mcp_caps.read().unwrap().details,
            "操作詳細は現在利用できません"
        );
        let r = self.store.request(&rec.request).await?;
        let v = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/interactions/{}/operation", path_id(&rec.remote)?),
                None,
                None,
            )
            .await?;
        ensure!(
            v["interaction_id"] == rec.remote
                && v["response_id"] == rec.response
                && v["revision"] == revision
                && v["disclosure"] == "requester_only"
                && v["turn_id"].as_str() == r.turn_id.as_deref(),
            "操作詳細の対応が一致しません"
        );
        for k in [
            "call_id",
            "server",
            "tool",
            "binding_status",
            "unavailable_reason",
            "config_generation",
            "input_generation",
            "scope",
            "scope_fingerprint",
            "turn_grant_eligible",
            "ineligible_reason",
            "arguments",
            "redacted_paths",
        ] {
            ensure!(v.get(k).is_some(), "operation field missing");
        }
        ensure!(
            matches!(
                v["binding_status"].as_str(),
                Some("verified" | "unavailable")
            ) && v["turn_grant_eligible"].is_boolean(),
            "invalid operation state"
        );
        ensure!(
            v["redacted_paths"]
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_string)),
            "invalid redaction metadata"
        );
        ensure!(
            matches!(
                v["unavailable_reason"].as_str(),
                Some("stable_call_id_unavailable" | "operation_details_expired")
            ) || v["unavailable_reason"].is_null(),
            "unknown unavailable reason"
        );
        ensure!(
            matches!(
                v["ineligible_reason"].as_str(),
                Some(
                    "binding_unavailable"
                        | "high_risk_tool"
                        | "redacted_arguments"
                        | "approval_context_required"
                        | "tool_not_allowlisted"
                        | "config_changed"
                        | "input_changed"
                )
            ) || v["ineligible_reason"].is_null(),
            "unknown ineligible reason"
        );
        ensure!(revision >= 1, "invalid revision");
        if v["binding_status"] == "unavailable" {
            for k in [
                "call_id",
                "server",
                "tool",
                "config_generation",
                "input_generation",
                "scope",
                "scope_fingerprint",
                "arguments",
            ] {
                ensure!(v[k].is_null(), "unavailable metadata mismatch");
            }
            ensure!(
                v["turn_grant_eligible"] == false
                    && v["unavailable_reason"] == "stable_call_id_unavailable"
                    && v["ineligible_reason"] == "binding_unavailable",
                "unavailable state mismatch"
            );
        }
        if v["binding_status"] == "verified" {
            self.scope_matches(&r, &v["scope"]).await?;
            for k in ["server", "tool", "config_generation", "input_generation"] {
                ensure!(v[k] == v["scope"][k], "scope metadata mismatch");
            }
            for k in [
                "call_id",
                "server",
                "tool",
                "config_generation",
                "scope_fingerprint",
            ] {
                text(&v, k)?;
            }
        }
        ensure!(
            v["arguments"].is_null() || v["arguments"].is_object(),
            "invalid arguments"
        );
        ensure!(
            serde_json::to_vec(&v["arguments"])?.len() <= 65536,
            "arguments oversized"
        );
        Ok(v)
    }
    async fn show_mcp_details(
        &self,
        local: &str,
        thread: &str,
        revision: i64,
        pagination: (Option<&str>, usize),
        app: &str,
        token: &str,
    ) -> Result<()> {
        let (view, page) = pagination;
        let v = self.mcp_operation(local, thread, revision).await?;
        if v["binding_status"] != "verified"
            || !v["unavailable_reason"].is_null()
            || !v["arguments"].is_object()
        {
            return self.discord.reply_components(app,token,"実際の操作内容を取得できません。許可せず元のカードから拒否するか、/stop で停止してください。",json!([])).await;
        }
        let s = self.settings().await;
        let rec = self.mcp_record(local).await?;
        let fingerprint = text(&v, "scope_fingerprint")?.to_owned();
        let scope_digest = domain::digest(&serde_json::to_vec(&canonical_scope(&v["scope"]))?);
        let view_id = if let Some(id) = view {
            self.check_mcp_view(id, local, revision, &fingerprint, &scope_digest)
                .await?;
            id.to_owned()
        } else {
            let id = domain::id();
            let (i, l, user, fp, sd, expires) = (
                id.clone(),
                local.to_owned(),
                s.cfg.discord.allowed_user_id.clone(),
                fingerprint,
                scope_digest,
                rec.expires.min(domain::now_ms() + 600000),
            );
            self.store
                .call(true, move |c| {
                    c.execute(
                        "DELETE FROM mcp_detail_views WHERE expires_at<=?1",
                        [domain::now_ms()],
                    )?;
                    c.execute("DELETE FROM mcp_detail_views WHERE interaction_local_id=?1 AND viewer_id=?2",params![l,user])?;
                    c.execute(
                        "INSERT INTO mcp_detail_views VALUES(?1,?2,?3,?4,?5,?6,?7)",
                        params![i, l, user, revision, fp, sd, expires],
                    )?;
                    Ok(())
                })
                .await?;
            id
        };
        let body = format!(
            "操作詳細（実イベント由来・本人限定）\nサーバー: {}\nツール: {}\n\n実引数・実行コード：\n{}\n\n秘匿箇所: {}",
            text(&v, "server")?,
            text(&v, "tool")?,
            serde_json::to_string_pretty(&v["arguments"])?,
            v["redacted_paths"]
        );
        let redacted = self.redact(&s, &body);
        let gateway_redacted = redacted != body;
        let body = format!(
            "{}\nGatewayによる追加秘匿: {}",
            redacted,
            if gateway_redacted {
                "あり（伏字を含みます）"
            } else {
                "なし"
            }
        );
        let chars: Vec<_> = body.chars().collect();
        let pages: Vec<String> = chars.chunks(500).map(|c| c.iter().collect()).collect();
        ensure!(page < pages.len(), "ページがありません");
        let mut buttons = vec![];
        if page > 0 {
            buttons.push(btn(format!("mt:page:{view_id}:{}", page - 1), "前へ", 2));
        }
        if page + 1 < pages.len() {
            buttons.push(btn(format!("mt:page:{view_id}:{}", page + 1), "次へ", 2));
        }
        buttons.push(btn(format!("mt:once:{view_id}"), "今回だけ許可", 3));
        let eligible = !gateway_redacted && turn_eligible(&v, *s.proxy.v2.mcp_caps.read().unwrap());
        if eligible {
            buttons.push(btn(
                format!("mt:turn:{view_id}"),
                "この依頼中、このツールを許可",
                3,
            ));
        }
        buttons.push(btn(format!("mcp:{local}:{revision}:decline"), "拒否", 4));
        // Scope explanation and controls are delivered together, never as a later notice.
        let explanation = if eligible {
            "ターン許可は引数変更も含み、サイトや読取りだけに限定されません。最大10分・この作業の終了まで有効です。Steerや停止で失効します。/mcp で確認・取消できます。"
        } else {
            "この操作はターン許可の対象外です。内容を確認できる場合だけ、今回だけ許可してください。"
        };
        self.discord
            .reply_components(
                app,
                token,
                &format!(
                    "{}/{}ページ\n{}\n{}",
                    page + 1,
                    pages.len(),
                    code_block(&pages[page]),
                    explanation
                ),
                row(buttons),
            )
            .await?;
        Ok(())
    }
    async fn check_mcp_view(
        &self,
        id: &str,
        local: &str,
        revision: i64,
        fp: &str,
        sd: &str,
    ) -> Result<()> {
        let id = id.to_owned();
        let row:(String,String,i64,String,String,i64)=self.store.call(false,move|c|Ok(c.query_row("SELECT interaction_local_id,viewer_id,revision,fingerprint,scope_digest,expires_at FROM mcp_detail_views WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?)).await?;
        ensure!(
            row.0 == local
                && row.1 == self.settings().await.cfg.discord.allowed_user_id
                && row.2 == revision
                && row.3 == fp
                && row.4 == sd
                && row.5 > domain::now_ms(),
            "古い詳細画面です。元のカードから開き直してください"
        );
        Ok(())
    }
    pub async fn handle_mcp_turn(&self, event: &Value) -> Result<()> {
        let s = self.settings().await;
        ensure!(
            event["type"] == 3
                && event["guild_id"] == s.cfg.discord.guild_id
                && event["member"]["user"]["id"] == s.cfg.discord.allowed_user_id,
            "操作権限がありません"
        );
        let id = field(event, "id")?;
        let token = field(event, "token")?;
        let app = field(event, "application_id")?;
        let thread = field(event, "channel_id")?;
        let custom = event["data"]["custom_id"].as_str().context("custom id")?;
        if ["mt:page:", "mt:once:", "mt:turn:", "mt:revoke-menu:"]
            .iter()
            .any(|p| custom.starts_with(p))
        {
            self.discord.acknowledge_update(&id, &token).await?;
        } else {
            self.discord.acknowledge(&id, &token).await?;
        }
        let result = async {
            ensure!(
                !self.recovery.load(std::sync::atomic::Ordering::SeqCst)
                    && !self.reloading.load(std::sync::atomic::Ordering::SeqCst),
                "復旧確認中です"
            );
            self.authorized_thread(&thread).await?;
            let custom = event["data"]["custom_id"].as_str().context("custom id")?;
            let parts: Vec<_> = custom.split(':').collect();
            ensure!(parts.len() >= 3 && parts[0] == "mt", "invalid control");
            let action = parts[1];
            let target = parts[2];
            if action == "details" {
                let rev = parts.get(3).context("revision")?.parse()?;
                return self
                    .show_mcp_details(target, &thread, rev, (None, 0), &app, &token)
                    .await;
            }
            if action == "grants" {
                return self.mcp_grant_menu(target, &thread, &app, &token).await;
            }
            if action == "revoke-menu" {
                let chosen = event["data"]["values"][0]
                    .as_str()
                    .context("取消対象を選んでください")?;
                return self.revoke_mcp_grant(chosen, &thread, &app, &token).await;
            }
            if action == "revoke" {
                return self.revoke_mcp_grant(target, &thread, &app, &token).await;
            }
            ensure!(matches!(action, "page" | "once" | "turn"), "invalid action");
            let t = target.to_owned();
            let (local, revision): (String, i64) = self
                .store
                .call(false, move |c| {
                    Ok(c.query_row(
                        "SELECT interaction_local_id,revision FROM mcp_detail_views WHERE id=?1",
                        [t],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?)
                })
                .await?;
            if action == "page" {
                return self
                    .show_mcp_details(
                        &local,
                        &thread,
                        revision,
                        (Some(target), parts.get(3).context("page")?.parse()?),
                        &app,
                        &token,
                    )
                    .await;
            }
            let v = self.mcp_operation(&local, &thread, revision).await?;
            let fp = text(&v, "scope_fingerprint")?;
            let sd = domain::digest(&serde_json::to_vec(&canonical_scope(&v["scope"]))?);
            self.check_mcp_view(target, &local, revision, fp, &sd)
                .await?;
            ensure!(
                v["binding_status"] == "verified"
                    && v["unavailable_reason"].is_null()
                    && v["arguments"].is_object(),
                "操作内容が失効しました"
            );
            let turn = action == "turn";
            let raw = serde_json::to_string_pretty(&v["arguments"])?;
            ensure!(
                !turn || self.redact(&s, &raw) == raw,
                "秘匿を含む操作は個別確認してください"
            );
            ensure!(
                !turn || turn_eligible(&v, *s.proxy.v2.mcp_caps.read().unwrap()),
                "この操作はターン許可できません"
            );
            self.mcp_reply_scoped(
                &local,
                &thread,
                revision,
                "accept",
                serde_json::Map::new(),
                Some((fp.to_owned(), turn)),
            )
            .await?;
            self.discord
                .reply_components(
                    &app,
                    &token,
                    "許可の回答を送りました。作業結果は元の会話で確認してください。",
                    json!([]),
                )
                .await
        }
        .await;
        if result.is_err() {
            self.discord.reply_components(&app,&token,"操作を確認できませんでした。古い画面・期限・権限・Proxyの状態を確認するため、元のカードまたは /mcp から開き直してください。許可は自動再送しません。",json!([])).await?;
        }
        Ok(())
    }
}
pub fn turn_eligible(v: &Value, caps: Capabilities) -> bool {
    caps.details
        && caps.turn
        && v["turn_grant_eligible"] == true
        && v["binding_status"] == "verified"
        && v["unavailable_reason"].is_null()
        && v["ineligible_reason"].is_null()
        && v["scope"]["context"].is_object()
        && v["redacted_paths"].as_array().is_some_and(Vec::is_empty)
        && v["tool"].as_str().is_some_and(|s| !s.is_empty())
        && !matches!(
            v["tool"].as_str(),
            Some("browser_evaluate" | "browser_run_code_unsafe")
        )
}

impl App {
    pub(crate) async fn mcp_grant_menu(
        &self,
        request: &str,
        thread: &str,
        app: &str,
        token: &str,
    ) -> Result<()> {
        self.authorized_thread(thread).await?;
        let r = self.store.request(request).await?;
        ensure!(r.thread_id == thread, "作業が一致しません");
        let s = self.settings().await;
        let modern = self.store.v06_selection(request).await?.is_some();
        ensure!(
            modern || s.proxy.v2.mcp_caps.read().unwrap().turn,
            "ターン許可は現在利用できません"
        );
        let response = r.response_id.as_ref().context("Response missing")?;
        let result = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/responses/{}/mcp-grants", path_id(response)?),
                None,
                None,
            )
            .await;
        let list = match result {
            Ok(v) => v,
            Err(_) if modern => return self.saved_v06_grants(request, thread, app, token).await,
            Err(error) => return Err(error),
        };
        ensure!(
            list["response_id"] == *response,
            "grant list response mismatch"
        );
        let data = list["data"].as_array().context("grant list missing")?;
        ensure!(data.len() <= 16, "grant count");
        let mut lines = vec!["この作業のMCP許可（本人限定）".to_owned()];
        let mut controls = vec![];
        let mut ids = std::collections::HashSet::new();
        for grant in data {
            let remote = text(grant, "grant_id")?.to_owned();
            ensure!(ids.insert(remote.clone()), "duplicate grant");
            self.scope_matches(&r, &grant["scope"]).await?;
            ensure!(
                !grant["scope"]["context"].is_null(),
                "grant context missing"
            );
            let state = text(grant, "state")?;
            ensure!(
                matches!(
                    state,
                    "pending" | "active" | "suspended" | "revoked" | "expired"
                ),
                "unknown grant state"
            );
            if matches!(state, "revoked" | "expired") {
                ensure!(
                    matches!(
                        grant["reason"].as_str(),
                        Some(
                            "operator_revoked"
                                | "scope_ended"
                                | "run_ended"
                                | "catalog_failed"
                                | "catalog_changed"
                                | "policy_changed"
                                | "config_changed"
                                | "input_changed"
                                | "expired"
                                | "runtime_restarted"
                        )
                    ),
                    "grant reason missing"
                );
            }
            timestamp(&grant["created_at"])?;
            timestamp(&grant["expires_at"])?;
            text(grant, "initial_interaction_id")?;
            let count = grant["application_count"]
                .as_i64()
                .filter(|n| *n >= 0)
                .context("count missing")?;
            if modern {
                crate::mcp_v06::validate_grant(grant)?;
                ensure!(
                    self.store
                        .v06_selection(request)
                        .await?
                        .context("missing v06 selection")?
                        == crate::mcp_v06::ExecutionPolicy::parse(&grant["execution_policy"])?
                            .selection,
                    "grant selection mismatch"
                );
            }
            let (req, scope, st, expiry) = (
                request.to_owned(),
                serde_json::to_string(&if modern {
                    json!({"scope":grant["scope"],"execution_policy":grant["execution_policy"],"grant_policy":grant["grant_policy"]})
                } else {
                    canonical_scope(&grant["scope"])
                })?,
                state.to_owned(),
                field(grant, "expires_at")?,
            );
            let local=self.store.call(true,move|c|{
    let old:Option<(String,String)>=c.query_row("SELECT id,scope_json FROM mcp_grant_records WHERE request_id=?1 AND grant_id=?2",params![req,remote],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let id=if let Some((id,old))=old {ensure!(old==scope,"grant scope changed");id}else{domain::id()};
    c.execute("INSERT INTO mcp_grant_records VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(request_id,grant_id) DO UPDATE SET state=excluded.state,expires_at=excluded.expires_at,application_count=excluded.application_count",params![id,req,remote,scope,st,expiry,count])?;Ok(id)
   }).await?;
            let label = match state {
                "active" => "有効",
                "pending" => "最初の回答を確認中",
                "suspended" => "結果不明・適用停止",
                "revoked" => "取消済み",
                _ => "失効",
            };
            if modern {
                lines.push(match grant["availability"]["state"].as_str() {
                    Some("refreshing") => {
                        "定義を更新中です。同じ定義の確認後に適用を再評価します。".into()
                    }
                    Some("inactive") => {
                        "この許可は現在、自動適用しません。取消は引き続き利用できます。".into()
                    }
                    _ => "適用時に対象・引数・期限を再確認します。".into(),
                });
            }
            let server = safe(text(&grant["scope"], "server")?);
            let tool = safe(text(&grant["scope"], "tool")?);
            let name = format!("{server} / {tool}");
            let short: String = name.chars().take(45).collect();
            lines.push(format!(
                "{}: {label}・適用送信 {count}件（成功件数ではありません）\n期限: {}",
                short,
                grant["expires_at"].as_str().unwrap()
            ));
            if modern {
                let operations = grant["grant_policy"]["eligible_operations"]
                    .as_array()
                    .context("grant operations")?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(safe)
                    .collect::<Vec<_>>()
                    .join(" / ");
                let confirm = grant["grant_policy"]["always_confirm_operations"]
                    .as_array()
                    .context("confirm operations")?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(safe)
                    .collect::<Vec<_>>()
                    .join(" / ");
                lines.push(format!(
                    "依頼中の許可対象: {operations}\n毎回確認する操作: {confirm}"
                ));
            }
            self.refresh_mcp_revoke(&local, &grant["grant_id"]).await?;
            let lookup = local.clone();
            let outcome: Option<(String,Option<i64>)> = self.store.call(false, move |c| Ok(c.query_row("SELECT state,in_flight_count FROM mcp_grant_revokes WHERE grant_local_id=?1",[lookup],|r|Ok((r.get(0)?,r.get(1)?))).optional()?)).await?;
            if let Some((status, count)) = &outcome {
                lines.push(match count {
                    Some(n) => format!("取消確認済み・取消時の送信中/結果不明 {n}件"),
                    None => format!(
                        "取消確認中（{status}）。この一覧を開き直すと元の取消を照会します。"
                    ),
                });
            }
            let uncertain = outcome.as_ref().is_some_and(|(st, _)| st != "succeeded");
            if matches!(state, "pending" | "active" | "suspended") || uncertain {
                controls.push(json!({"label":short,"value":local}));
            }
        }
        if data.is_empty() {
            lines.push("この作業のターン限定許可はありません。".into());
        }
        lines.push(
            "取消は以後の適用を止めます。実行済みの操作は戻りません。作業全体の停止は /stop。"
                .into(),
        );
        let components = if controls.is_empty() {
            json!([])
        } else {
            json!([{"type":1,"components":[{"type":3,"custom_id":format!("mt:revoke-menu:{request}"),"placeholder":"取り消す許可を選択","options":controls}]}])
        };
        self.mcp_private(app, token, &lines.join("\n"), components)
            .await
    }
    async fn refresh_mcp_revoke(&self, local: &str, remote: &Value) -> Result<()> {
        let id = local.to_owned();
        let key:Option<String>=self.store.call(false,move|c|Ok(c.query_row("SELECT operation_key FROM mcp_grant_revokes WHERE grant_local_id=?1 AND state!='succeeded'",[id],|r|r.get(0)).optional()?)).await?;
        if let Some(key) = key
            && let Ok(v) = self.settings().await.proxy.operation(&key).await
            && v["kind"] == "mcp_grant.revoke"
            && v["resource"]["type"] == "mcp_grant"
            && &v["resource"]["id"] == remote
            && v["state"] == "succeeded"
            && let Some(n) = v["in_flight_or_unknown_count"].as_i64().filter(|n| *n >= 0)
        {
            let id = local.to_owned();
            self.store.call(true,move|c|{c.execute("UPDATE mcp_grant_revokes SET state='succeeded',in_flight_count=?2 WHERE grant_local_id=?1",params![id,n])?;Ok(())}).await?;
        }
        Ok(())
    }
    async fn revoke_mcp_grant(
        &self,
        local: &str,
        thread: &str,
        app: &str,
        token: &str,
    ) -> Result<()> {
        let id = local.to_owned();
        let (request, remote, scope): (String, String, String) = self
            .store
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT request_id,grant_id,scope_json FROM mcp_grant_records WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .await?;
        let r = self.store.request(&request).await?;
        ensure!(r.thread_id == thread, "許可の会話が一致しません");
        let saved: Value = serde_json::from_str(&scope)?;
        self.scope_matches(
            &r,
            if saved.get("scope").is_some() {
                &saved["scope"]
            } else {
                &saved
            },
        )
        .await?;
        let s = self.settings().await;
        ensure!(
            self.store.v06_selection(&request).await?.is_some()
                || s.proxy.v2.mcp_caps.read().unwrap().turn,
            "ターン許可は現在利用できません"
        );
        let id = local.to_owned();
        let (key, fresh) = self
            .store
            .call(true, move |c| {
                let tx = c.transaction()?;
                let old: Option<String> = tx
                    .query_row(
                        "SELECT operation_key FROM mcp_grant_revokes WHERE grant_local_id=?1",
                        [&id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(key) = old {
                    return Ok((key, false));
                }
                let key = format!("revoke-{}", domain::id());
                tx.execute(
                    "INSERT INTO mcp_grant_revokes VALUES(?1,?2,'SENDING',NULL)",
                    params![id, key],
                )?;
                tx.commit()?;
                Ok((key, true))
            })
            .await?;
        let op = if fresh {
            s.proxy
                .v2_json(
                    Method::POST,
                    &format!("/v2/codex/mcp-grants/{}/revoke", path_id(&remote)?),
                    Some(&key),
                    Some(&json!({})),
                )
                .await
        } else {
            s.proxy.operation(&key).await
        };
        let (state, count) = match &op {
            Ok(v)
                if v["kind"] == "mcp_grant.revoke"
                    && v["resource"]["type"] == "mcp_grant"
                    && v["resource"]["id"] == remote
                    && v["state"] == "succeeded" =>
            {
                match v["in_flight_or_unknown_count"].as_i64().filter(|n| *n >= 0) {
                    Some(n) => ("succeeded", Some(n)),
                    None => ("unknown", None),
                }
            }
            _ => ("unknown", None),
        };
        let id = local.to_owned();
        self.store.call(true,move|c|{c.execute("UPDATE mcp_grant_revokes SET state=?2,in_flight_count=?3 WHERE grant_local_id=?1 AND state!='succeeded'",params![id,state,count])?;Ok(())}).await?;
        let message = if let Some(n) = count {
            format!(
                "この許可の新たな適用を停止しました。取消時点の送信中・結果不明は{n}件です。既に実行した操作は戻りません。作業停止は /stop。"
            )
        } else {
            "取消の完了をまだ確認できません。/mcp で状態を確認してください。再度取消を選んだ場合は元の操作を照会し、別の取消要求は送りません。".into()
        };
        self.discord
            .reply_components(app, token, &message, json!([]))
            .await
    }
    pub(crate) async fn latest_mcp_grants(
        &self,
        thread: &str,
        app: &str,
        token: &str,
    ) -> Result<String> {
        let t = thread.to_owned();
        let id:Option<String>=self.store.call(false,move|c|Ok(c.query_row("SELECT id FROM requests WHERE thread_id=?1 AND response_id IS NOT NULL ORDER BY sequence DESC LIMIT 1",[t],|r|r.get(0)).optional()?)).await?;
        if let Some(id) = id {
            self.mcp_grant_menu(&id, thread, app, token).await?;
            Ok(String::new())
        } else {
            Ok("この会話にはまだ作業がありません。".into())
        }
    }
}

impl App {
    async fn saved_v06_grants(
        &self,
        request: &str,
        thread: &str,
        app: &str,
        token: &str,
    ) -> Result<()> {
        let rid = request.to_owned();
        let rows:Vec<(String,String)>=self.store.call(false,move|c|{let mut q=c.prepare("SELECT id,scope_json FROM mcp_grant_records WHERE request_id=?1 ORDER BY id LIMIT 16")?;Ok(q.query_map([rid],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)}).await?;
        let mut options = vec![];
        let req = self.store.request(request).await?;
        ensure!(req.thread_id == thread, "grant conversation mismatch");
        for (id, scope) in rows {
            let scope: Value = serde_json::from_str(&scope)?;
            self.scope_matches(&req, &scope["scope"]).await?;
            let name = format!(
                "{} / {}",
                safe(text(&scope["scope"], "server")?),
                safe(text(&scope["scope"], "tool")?)
            );
            options.push(json!({"label":name.chars().take(80).collect::<String>(),"value":id}));
        }
        let controls = if options.is_empty() {
            json!([])
        } else {
            json!([{"type":1,"components":[{"type":3,"custom_id":format!("mt:revoke-menu:{request}"),"placeholder":"保存済みの許可を取り消す","options":options}]}])
        };
        self.mcp_private(app,token,"現在の許可一覧を取得できません。許可がないという意味ではありません。保存済みの許可は下から取消できます。すべての操作を止める場合は /stop を使ってください。状態を読み直すには /mcp を実行してください。",controls).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_pages_preserve_symbols_and_stay_within_discord_limit() {
        for text in [
            "`".repeat(500),
            "😀".repeat(500),
            "const value = `hello @name`;".into(),
        ] {
            let rendered = code_block(&text);
            assert!(rendered.contains(&text));
            assert!(rendered.encode_utf16().count() + 80 < 2000);
        }
    }
}
