//! API 0.4 public presentations. Never render private operation arguments here.
use crate::{
    application::App,
    domain,
    mcp_ui::Record,
    proxy::{field, path_id},
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(crate) struct Receipt {
    pub token: String,
    pub message_id: String,
    pub card_digest: String,
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    let s = v[k].as_str().context("presentation string missing")?;
    ensure!(
        !s.is_empty() && s.len() <= 8192,
        "presentation string limit"
    );
    Ok(s)
}
fn expiry(v: &Value) -> Result<i64> {
    Ok(
        (OffsetDateTime::parse(string(v, "expires_at")?, &Rfc3339)?.unix_timestamp_nanos()
            / 1_000_000) as i64,
    )
}
fn known_reason(s: &str) -> bool {
    matches!(
        s,
        "private_arguments"
            | "unsupported_renderer"
            | "unknown_arguments"
            | "information_incomplete"
            | "display_too_large"
            | "operation_unavailable"
            | "interaction_closed"
            | "presentation_limit"
    )
}
fn display_string(v: &Value) -> Result<&str> {
    let s = v.as_str().context("display string")?;
    ensure!(!s.is_empty() && !s.chars().any(|c|c.is_control() || matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')),"invalid display characters");
    Ok(s)
}
/// Structural validation, independent of prose. Tool semantics belong to Proxy.
pub fn validate_presentation(v: &Value) -> Result<()> {
    ensure!(
        serde_json::to_vec(v)?.len() <= 32768,
        "presentation byte limit"
    );
    ensure!(
        v.as_object().is_some_and(|o| o.keys().all(|k| matches!(
            k.as_str(),
            "interaction_id"
                | "response_id"
                | "turn_id"
                | "revision"
                | "scope_fingerprint"
                | "presentation_id"
                | "presentation_fingerprint"
                | "profile"
                | "renderer"
                | "audience"
                | "state"
                | "reason"
                | "expires_at"
                | "display"
                | "actions"
        ))),
        "unknown presentation field"
    );

    for k in [
        "interaction_id",
        "response_id",
        "turn_id",
        "revision",
        "scope_fingerprint",
        "presentation_id",
        "presentation_fingerprint",
        "profile",
        "renderer",
        "audience",
        "state",
        "reason",
        "expires_at",
        "display",
        "actions",
    ] {
        ensure!(v.get(k).is_some(), "missing presentation field");
    }
    string(v, "interaction_id")?;
    string(v, "response_id")?;
    ensure!(
        v["revision"].as_i64().is_some_and(|r| r > 0) && v["profile"] == "source-conversation-v1",
        "presentation profile/revision"
    );
    ensure!(
        v["audience"]["kind"] == "source_conversation"
            && string(&v["audience"], "channel_id")?.len() <= 128,
        "presentation audience"
    );
    let state = string(v, "state")?;
    ensure!(
        matches!(state, "inline" | "private_required" | "unavailable"),
        "unknown presentation state"
    );
    if state == "inline" {
        ensure!(v["reason"].is_null(), "inline reason");
    } else {
        ensure!(known_reason(string(v, "reason")?), "unknown reason");
    }
    for k in [
        "turn_id",
        "scope_fingerprint",
        "presentation_id",
        "presentation_fingerprint",
        "expires_at",
    ] {
        if !v[k].is_null() {
            string(v, k)?;
        }
    }
    if state != "unavailable" && v["reason"] != "presentation_limit" {
        string(v, "presentation_id")?;
        string(v, "presentation_fingerprint")?;
        expiry(v)?;
    }
    if !v["renderer"].is_null() {
        ensure!(
            matches!(
                string(v, "renderer")?,
                "browser-find-v1" | "browser-navigate-v1" | "browser-tabs-list-v1"
            ),
            "unknown renderer"
        );
    }
    let a = &v["actions"];
    for k in [
        "allow_once",
        "allow_turn_tool",
        "decline",
        "open_private_details",
    ] {
        ensure!(a[k].is_boolean(), "invalid action type");
    }
    match state {
        "inline" => {
            ensure!(
                a["allow_once"] == true
                    && a["decline"] == true
                    && a["open_private_details"] == false
                    && !v["renderer"].is_null(),
                "invalid inline actions"
            );
            string(v, "turn_id")?;
            string(v, "scope_fingerprint")?;
        }
        "private_required" => ensure!(
            a["allow_once"] == false
                && a["allow_turn_tool"] == false
                && a["open_private_details"] == true
                && a["decline"] == true,
            "invalid private actions"
        ),
        _ => ensure!(
            a["allow_once"] == false
                && a["allow_turn_tool"] == false
                && a["open_private_details"] == false,
            "invalid unavailable actions"
        ),
    }
    let d = &v["display"];
    ensure!(
        d["disclosure"] == "source_conversation"
            && matches!(
                d["provenance"].as_str(),
                Some("proxy_verified_call" | "unavailable")
            ),
        "invalid disclosure"
    );
    if state == "inline" {
        ensure!(
            d["provenance"] == "proxy_verified_call",
            "unverified inline"
        );
    }
    let mut units = display_string(&d["title"])?.encode_utf16().count();
    let fields = d["fields"].as_array().context("fields")?;
    ensure!(fields.len() <= 8, "too many fields");
    for f in fields {
        units += display_string(&f["label"])?.encode_utf16().count()
            + display_string(&f["value"])?.encode_utf16().count();
    }
    for v in d["limitations"].as_array().context("limitations")? {
        units += display_string(v)?.encode_utf16().count();
    }
    let omissions = d["omissions"].as_array().context("omissions")?;
    for v in omissions {
        let s = display_string(v)?;
        ensure!(
            matches!(
                s,
                "private_arguments"
                    | "unknown_arguments"
                    | "information_incomplete"
                    | "display_too_large"
            ),
            "unknown omission"
        );
        units += s.encode_utf16().count();
    }
    ensure!(units <= 1400, "display text limit");
    ensure!(
        state != "inline" || omissions.is_empty(),
        "incomplete inline"
    );
    Ok(())
}
fn button(id: String, label: &str, style: u8) -> Value {
    json!({"type":2,"custom_id":id,"label":label,"style":style})
}
fn row(v: Vec<Value>) -> Value {
    if v.is_empty() {
        json!([])
    } else {
        json!([{"type":1,"components":v}])
    }
}
fn fence(s: &str) -> String {
    let n = s.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1;
    let f = "`".repeat(n.max(3));
    format!("{f}\n{s}\n{f}")
}
fn fallback(rec: &Record, reason: &str, private: bool, decline: bool) -> (String, Value) {
    let mut buttons = vec![];
    if private {
        buttons.push(button(
            format!("mt:details:{}:{}", rec.id, rec.revision),
            "本人限定で確認",
            1,
        ));
    }
    if decline {
        buttons.push(button(
            format!("mcp:{}:{}:decline", rec.id, rec.revision),
            "拒否",
            4,
        ));
    }
    (
        format!(
            "{reason}\n{}",
            if private {
                "本人限定で内容を確認してから、許可するか選んでください。作業停止は /stop。"
            } else {
                "許可せず、拒否または /stop で停止してください。解消しない場合は運用者に確認してください。"
            }
        ),
        row(buttons),
    )
}
fn card(v: &Value, rec: &Record, view: &str) -> Result<(String, Value, bool)> {
    let d = &v["display"];
    let mut lines = vec![string(d, "title")?.to_owned()];
    for f in d["fields"].as_array().context("fields")? {
        lines.push(format!("{}：{}", string(f, "label")?, string(f, "value")?));
    }
    for l in d["limitations"].as_array().context("limitations")? {
        lines.push(l.as_str().context("limitation")?.into());
    }
    let mut text = fence(&lines.join("\n"));
    let a = &v["actions"];
    let mut buttons = vec![];
    let inline = v["state"] == "inline";
    if inline {
        if a["allow_turn_tool"] == true {
            text.push_str("\nこの依頼の同じツールを別の引数でも許可します。追加発言・停止・終了・最大10分で失効します。取消は /mcp。");
            buttons.push(button(
                format!("mi:{view}:turn"),
                "この依頼中、このツールを許可",
                3,
            ));
        }
        text.push_str("\n「今回だけ許可」は、この呼出し1回だけを許可します。");
        buttons.push(button(format!("mi:{view}:once"), "今回だけ許可", 3));
    } else if a["open_private_details"] == true {
        buttons.push(button(
            format!("mt:details:{}:{}", rec.id, rec.revision),
            "本人限定で確認",
            1,
        ));
    }
    if a["decline"] == true {
        buttons.push(button(
            format!("mcp:{}:{}:decline", rec.id, rec.revision),
            "拒否",
            4,
        ));
    }
    if text.encode_utf16().count() > 2000 {
        let (t, b) = fallback(
            rec,
            "操作内容が長いため、最初のカードには全体を表示できません。",
            v["state"] != "unavailable",
            a["decline"] == true,
        );
        return Ok((t, b, false));
    }
    Ok((text, row(buttons), inline))
}
fn card_digest(text: &str, components: &Value) -> Result<String> {
    Ok(domain::digest(&serde_json::to_vec(&(
        text,
        crate::delivery::canonical_components(components),
    ))?))
}
impl App {
    pub(crate) async fn has_inline_run(&self, request: &str) -> Result<bool> {
        let id = request.to_owned();
        self.store
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM mcp_inline_runs WHERE request_id=?1)",
                    [id],
                    |r| r.get(0),
                )?)
            })
            .await
    }
    async fn invalidate_inline(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.store
            .call(true, move |c| {
                c.execute(
                    "UPDATE mcp_inline_views SET active=0 WHERE interaction_local_id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await
    }
    async fn presentation(&self, rec: &Record, thread: &str) -> Result<Value> {
        let s = self.settings().await;
        ensure!(
            s.proxy.v2.mcp_caps.read().unwrap().inline,
            "inline capability unavailable"
        );
        let op = self.mcp_operation(&rec.id, thread, rec.revision).await?;
        let p = s
            .proxy
            .v2_json(
                Method::GET,
                &format!(
                    "/v2/codex/interactions/{}/presentation",
                    path_id(&rec.remote)?
                ),
                None,
                None,
            )
            .await?;
        validate_presentation(&p)?;
        ensure!(
            p["interaction_id"] == rec.remote
                && p["response_id"] == rec.response
                && p["revision"] == rec.revision
                && p["turn_id"] == op["turn_id"]
                && p["scope_fingerprint"] == op["scope_fingerprint"],
            "presentation identity mismatch"
        );
        ensure!(
            p["audience"]["channel_id"] == self.context(&rec.request).await?["channel_id"],
            "presentation audience mismatch"
        );
        if !p["expires_at"].is_null() {
            ensure!(
                expiry(&p)? <= rec.expires
                    && expiry(&p)? > domain::now_ms()
                    && expiry(&p)? <= domain::now_ms() + 600_000,
                "presentation expired"
            );
        }
        if p["state"] == "inline" {
            ensure!(
                op["binding_status"] == "verified"
                    && op["unavailable_reason"].is_null()
                    && op["arguments"].is_object()
                    && op["redacted_paths"].as_array().is_some_and(Vec::is_empty),
                "operation unavailable"
            );
            let expected = match p["renderer"].as_str() {
                Some("browser-find-v1") => "browser_find",
                Some("browser-navigate-v1") => "browser_navigate",
                Some("browser-tabs-list-v1") => "browser_tabs",
                _ => anyhow::bail!("renderer missing"),
            };
            ensure!(op["tool"] == expected, "renderer/tool mismatch");
            ensure!(
                p["actions"]["allow_turn_tool"] != true
                    || crate::mcp_grants::turn_eligible(&op, *s.proxy.v2.mcp_caps.read().unwrap()),
                "turn grant ineligible"
            );
        }
        Ok(p)
    }
    pub(crate) async fn render_inline_mcp(&self, rec: &Record, thread: &str) -> Result<()> {
        let p = match self.presentation(rec, thread).await {
            Ok(p) => p,
            Err(_) => {
                self.invalidate_inline(&rec.id).await?;
                let (t, b) = fallback(rec, "操作内容をまだ確認できません。", false, true);
                self.delivery
                    .text(&rec.id, thread, "mcp_action", 0, &t, b)
                    .await?;
                return Ok(());
            }
        };
        if p["state"] != "inline" {
            self.invalidate_inline(&rec.id).await?;
            let (t, b, _) = card(&p, rec, "")?;
            self.delivery
                .text(&rec.id, thread, "mcp_action", 0, &t, b)
                .await?;
            return Ok(());
        }
        let snapshot = domain::digest(&serde_json::to_vec(&p)?);
        let (local, pid, token, revision, expires, hash) = (
            rec.id.clone(),
            string(&p, "presentation_id")?.to_owned(),
            string(&p, "presentation_fingerprint")?.to_owned(),
            rec.revision,
            expiry(&p)?,
            snapshot.clone(),
        );
        let (invalidate_id, invalidate_hash) = (local.clone(), hash.clone());
        self.store.call(true,move|c|{c.execute("UPDATE mcp_inline_views SET active=0 WHERE interaction_local_id=?1 AND snapshot_digest!=?2",params![invalidate_id,invalidate_hash])?;Ok(())}).await?;
        let view=self.store.call(true,move|c|{let tx=c.transaction()?;
            tx.execute("UPDATE mcp_inline_views SET active=0 WHERE interaction_local_id=?1 AND snapshot_digest!=?2",params![local,hash])?;
            let id:Option<(String,String)>=tx.query_row("SELECT id,snapshot_digest FROM mcp_inline_views WHERE interaction_local_id=?1 AND presentation_id=?2 AND fingerprint=?3",params![local,pid,token],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let id=if let Some((id,old))=id{ensure!(old==hash,"presentation token reused for changed content");tx.execute("UPDATE mcp_inline_views SET active=1 WHERE id=?1",[&id])?;id}else{let id=domain::id();tx.execute("INSERT INTO mcp_inline_views VALUES(?1,?2,?3,?4,?5,?6,?7,1)",params![id,local,pid,token,revision,hash,expires])?;id};tx.commit()?;Ok(id)
        }).await?;
        let (t, b, inline) = card(&p, rec, &view)?;
        if !inline {
            self.invalidate_inline(&rec.id).await?;
        }
        self.delivery
            .text(&rec.id, thread, "mcp_action", 0, &t, b)
            .await?;
        Ok(())
    }
    pub async fn handle_inline_mcp(&self, event: &Value) -> Result<()> {
        let s = self.settings().await;
        ensure!(
            event["type"] == 3
                && event["guild_id"] == s.cfg.discord.guild_id
                && event["member"]["user"]["id"] == s.cfg.discord.allowed_user_id,
            "操作権限がありません"
        );
        let (iid, token, app, thread) = (
            field(event, "id")?,
            field(event, "token")?,
            field(event, "application_id")?,
            field(event, "channel_id")?,
        );
        self.discord.acknowledge_update(&iid, &token).await?;
        let result=async {
            ensure!(!self.recovery.load(Ordering::SeqCst) && !self.reloading.load(Ordering::SeqCst),"復旧確認中です");
            let custom=event["data"]["custom_id"].as_str().context("button")?;let parts:Vec<_>=custom.split(':').collect();
            ensure!(parts.len()==3 && parts[0]=="mi" && matches!(parts[2],"once"|"turn"),"invalid control");
            let view=parts[1].to_owned();let turn=parts[2]=="turn";
            let id=view.clone();let(local,revision,hash):(String,i64,String)=self.store.call(false,move|c|Ok(c.query_row("SELECT interaction_local_id,revision,snapshot_digest FROM mcp_inline_views WHERE id=?1 AND active=1 AND expires_at>?2",params![id,domain::now_ms()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)).await?;
            let rec=self.mcp_record(&local).await?;ensure!(rec.revision==revision,"old revision");
            let p=self.presentation(&rec,&thread).await?;ensure!(p["state"]=="inline" && domain::digest(&serde_json::to_vec(&p)?)==hash,"changed presentation");
            ensure!(!turn || p["actions"]["allow_turn_tool"]==true,"turn not allowed");
            let(t,b,inline)=card(&p,&rec,&view)?;ensure!(inline,"supplement required");let digest=card_digest(&t,&b)?;
            let mid=event["message"]["id"].as_str().context("message identity")?.to_owned();
            let receipt=Receipt{token:string(&p,"presentation_fingerprint")?.to_owned(),message_id:mid.clone(),card_digest:digest.clone()};
            let (id,ch,vid)=(local.clone(),thread.clone(),view.clone());
            let confirmed:bool=self.store.call(false,move|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM deliveries d JOIN mcp_inline_views v ON v.interaction_local_id=d.target_id WHERE v.id=?1 AND v.active=1 AND d.target_id=?2 AND d.thread_id=?3 AND d.kind='mcp_action' AND d.part=0 AND d.state='CONFIRMED' AND d.message_id=?4 AND d.confirmed_digest=?5)",params![vid,id,ch,mid,digest],|r|r.get(0))?)).await?;
            ensure!(confirmed,"表示の送信確認ができません");
            self.mcp_reply_with_view(&local,&thread,revision,"accept",serde_json::Map::new(),Some((string(&p,"scope_fingerprint")?.to_owned(),turn)),Some(receipt)).await
        }.await;
        if result.is_err() {
            self.discord.followup_error(&app,&token,"このカードからの許可を確認できませんでした。最新のカードで内容を確認し直してください。回答の送信結果は元のカードで確認できます。許可は自動再送しません。").await?;
        }
        Ok(())
    }
}
