//! Validated API 0.6 boundaries. No tool-specific semantics or private-body persistence.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub const PROFILE: &str = "source-conversation-v3";
pub const PRESENTATION_LIMIT: usize = 65_536;
pub const ARGUMENT_LIMIT: usize = 262_144;
pub const PREPARATION_MS: i64 = 60_000;

fn id(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty() && s.len() <= 8192 && !s.chars().any(char::is_control),
        "invalid identifier"
    );
    Ok(())
}
fn fixed(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty() && s.len() <= 128 && s.is_ascii() && !s.bytes().any(|x| x.is_ascii_control()),
        "invalid fixed identifier"
    );
    Ok(())
}
fn time(s: &str) -> Result<i64> {
    Ok((OffsetDateTime::parse(s, &Rfc3339)?.unix_timestamp_nanos() / 1_000_000) as i64)
}
fn size<T: Serialize>(v: &T, limit: usize) -> Result<()> {
    ensure!(
        serde_json::to_vec(v)?.len() <= limit,
        "API object too large"
    );
    Ok(())
}
fn enum_value(v: &str, values: &[&str]) -> Result<()> {
    ensure!(values.contains(&v), "unknown API value");
    Ok(())
}
fn unique_strings(v: &[String], max: usize) -> Result<()> {
    ensure!(v.len() <= max, "array limit");
    let mut seen = std::collections::HashSet::new();
    for x in v {
        fixed(x)?;
        ensure!(seen.insert(x), "duplicate value");
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub id: String,
    pub version: u64,
}
impl Selection {
    pub fn guard() -> Self {
        Self {
            id: "evaluated-turn-notion-guard".into(),
            version: 1,
        }
    }
    pub fn validate(&self) -> Result<()> {
        fixed(&self.id)?;
        ensure!(self.version > 0, "invalid policy version");
        Ok(())
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    pub started_at: String,
    pub deadline_at: String,
    pub recovery_state: String,
    pub turn_start_status: String,
    pub configuration_isolation: String,
}
impl Preparation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            time(&self.deadline_at)?.checked_sub(time(&self.started_at)?) == Some(PREPARATION_MS),
            "preparation deadline changed"
        );
        enum_value(&self.recovery_state, &["none", "reconciling", "fenced"])?;
        enum_value(
            &self.turn_start_status,
            &["not_sent", "intent_recorded", "confirmed", "unknown"],
        )?;
        enum_value(
            &self.configuration_isolation,
            &["not_required", "pending", "confirmed"],
        )
    }
}
// Option fields are required-but-nullable on the wire; checked before deserialization.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPolicy {
    pub selection: Option<Selection>,
    pub binding_id: String,
    pub generation: Option<String>,
    pub state: String,
    pub reason: Option<String>,
    pub restrictions: Vec<String>,
    pub upstream_overrides: Vec<String>,
    pub preparation: Preparation,
}
fn required(v: &Value, names: &[&str]) -> Result<()> {
    let o = v.as_object().context("expected object")?;
    for n in names {
        ensure!(o.contains_key(*n), "missing required field");
    }
    Ok(())
}
impl ExecutionPolicy {
    pub fn parse(v: &Value) -> Result<Self> {
        required(v, &["selection", "generation", "reason"])?;
        let p: Self = serde_json::from_value(v.clone()).context("invalid execution policy")?;
        p.validate()?;
        Ok(p)
    }
    pub fn validate(&self) -> Result<()> {
        size(self, 4096)?;
        id(&self.binding_id)?;
        self.preparation.validate()?;
        enum_value(&self.state, &["preparing", "ready", "failed", "closed"])?;
        if let Some(g) = &self.generation {
            id(g)?;
        }
        unique_strings(&self.restrictions, 64)?;
        ensure!(
            self.upstream_overrides.len() <= 64
                && self.upstream_overrides.iter().all(|s| s.len() <= 1024),
            "policy description limit"
        );
        if let Some(selection) = &self.selection {
            selection.validate()?;
            ensure!(
                self.state != "ready" || self.generation.is_some(),
                "ready policy lacks generation"
            );
        } else {
            ensure!(
                self.generation.is_none()
                    && self.restrictions.is_empty()
                    && self.upstream_overrides.is_empty(),
                "unselected policy has restrictions"
            );
        }
        if let Some(r) = &self.reason {
            enum_value(
                r,
                &[
                    "policy_setup_failed",
                    "policy_setup_unknown",
                    "policy_configuration_conflict",
                    "scope_ended",
                    "policy_setup_timeout",
                    "policy_setup_cancelled",
                ],
            )?;
        }
        ensure!(
            !matches!(self.state.as_str(), "preparing" | "ready") || self.reason.is_none(),
            "active policy has failure reason"
        );
        ensure!(
            !matches!(self.state.as_str(), "failed" | "closed") || self.reason.is_some(),
            "terminal policy lacks reason"
        );
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunContext {
    pub principal_id: String,
    pub channel_id: String,
    pub run_id: String,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub instance_id: String,
    pub recovery_generation: String,
    pub context: RunContext,
    pub response_id: String,
    pub conversation_id: String,
    pub workspace_id: String,
    pub turn_id: String,
    pub input_generation: u64,
    pub config_generation: String,
    pub server: String,
    pub tool: String,
    pub execution_policy_binding_id: String,
    pub policy_generation: Option<String>,
    pub definition_generation: Option<String>,
}
impl Scope {
    pub fn parse(v: &Value, p: &ExecutionPolicy) -> Result<Self> {
        required(v, &["policy_generation", "definition_generation"])?;
        let s: Self = serde_json::from_value(v.clone()).context("invalid scope")?;
        for x in [
            &s.instance_id,
            &s.recovery_generation,
            &s.context.principal_id,
            &s.context.channel_id,
            &s.context.run_id,
            &s.response_id,
            &s.conversation_id,
            &s.workspace_id,
            &s.turn_id,
            &s.config_generation,
            &s.server,
            &s.tool,
            &s.execution_policy_binding_id,
        ] {
            id(x)?;
        }
        ensure!(
            s.context.principal_id.len() <= 128 && s.context.channel_id.len() <= 128,
            "context limit"
        );
        for g in [&s.policy_generation, &s.definition_generation]
            .into_iter()
            .flatten()
        {
            id(g)?;
        }
        ensure!(
            s.execution_policy_binding_id == p.binding_id && s.policy_generation == p.generation,
            "policy scope mismatch"
        );
        Ok(s)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseLimits {
    pub capabilities: usize,
    pub capability_extension: usize,
    pub response: usize,
    pub presentation: usize,
    pub operation_details: usize,
    pub interaction: usize,
    pub interaction_list: usize,
    pub grant_list: usize,
    pub control: usize,
    pub max_interactions: usize,
    pub max_pending_interactions: usize,
    pub max_grants: usize,
}
impl Default for ResponseLimits {
    fn default() -> Self {
        Self {
            capabilities: 1048576,
            capability_extension: 65536,
            response: 262144,
            presentation: 65536,
            operation_details: 65536,
            interaction: 262144,
            interaction_list: 67108864,
            grant_list: 1048576,
            control: 65536,
            max_interactions: 256,
            max_pending_interactions: 16,
            max_grants: 16,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDescriptor {
    pub id: String,
    pub version: u64,
    pub enabled: bool,
    pub label: String,
    pub restrictions: Vec<String>,
    pub upstream_overrides: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub enabled: bool,
    pub profile: String,
    pub renderers: Vec<String>,
    pub max_response_bytes: usize,
    pub max_argument_bytes: usize,
    pub max_display_text_utf16_units: usize,
    pub max_display_fields: usize,
    pub max_presentations_per_interaction_per_audience: usize,
    pub max_get_wait_ms: u64,
    pub retry_after_ms: u64,
    pub private_details: bool,
    pub max_private_pages: usize,
    pub max_private_display_bytes: usize,
    pub policies: Vec<PolicyDescriptor>,
    pub response_limits: ResponseLimits,
    pub policy_preparation_timeout_ms: i64,
}
impl Capabilities {
    pub fn parse(v: &Value) -> Result<Self> {
        size(v, 65536)?;
        let c: Self = serde_json::from_value(v.clone()).context("invalid 0.6 capability")?;
        ensure!(
            c.profile == PROFILE && c.response_limits == ResponseLimits::default(),
            "unsupported limits/profile"
        );
        ensure!(
            c.max_response_bytes == 65536
                && c.max_argument_bytes == 262144
                && c.max_display_text_utf16_units == 4800
                && c.max_display_fields == 24
                && c.max_presentations_per_interaction_per_audience == 4
                && c.max_get_wait_ms == 250
                && c.retry_after_ms == 2000
                && c.private_details
                && c.max_private_pages == 64
                && c.max_private_display_bytes == 262144
                && c.policy_preparation_timeout_ms == 60000,
            "unsupported capability contract"
        );
        unique_strings(&c.renderers, 2)?;
        ensure!(
            c.renderers.len() == 2
                && c.renderers.iter().any(|s| s == "raw-arguments-v1")
                && c.renderers.iter().any(|s| s == "evaluated-operation-v1"),
            "unsupported renderers"
        );
        ensure!(c.policies.len() <= 32, "policy count");
        let mut seen = std::collections::HashSet::new();
        for p in &c.policies {
            Selection {
                id: p.id.clone(),
                version: p.version,
            }
            .validate()?;
            size(p, 2048)?;
            unique_strings(&p.restrictions, 64)?;
            ensure!(
                p.label.len() <= 1024
                    && p.upstream_overrides.len() <= 64
                    && p.upstream_overrides.iter().all(|s| s.len() <= 1024),
                "descriptor limits"
            );
            ensure!(seen.insert((&p.id, p.version)), "duplicate policy");
        }
        Ok(c)
    }
    pub fn supports(&self, policy: Option<&Selection>) -> bool {
        self.enabled
            && policy.is_none_or(|s| {
                self.policies
                    .iter()
                    .any(|p| p.enabled && p.id == s.id && p.version == s.version)
            })
    }
}

pub fn response_limit(path: &str, modern: bool) -> usize {
    let path = path.split('?').next().unwrap_or(path);
    if path.ends_with("/presentation") {
        if modern { 65536 } else { 32768 }
    } else if path.ends_with("/mcp-grants") || path.ends_with("/capabilities") {
        1048576
    } else if path.ends_with("/interactions") {
        67108864
    } else if path.starts_with("/v2/codex/interactions/") && path.ends_with("/operation") {
        if modern { 65536 } else { 262144 }
    } else if path.ends_with("/reply")
        || path.ends_with("/revoke")
        || path.starts_with("/v2/codex/stops")
        || path.starts_with("/v2/codex/operations/")
    {
        65536
    } else {
        262144
    }
}

/// Validated presentation retains prose only in memory and never implements Debug.
pub struct Presentation {
    value: Value,
    pub policy: ExecutionPolicy,
    pub scope: Option<Scope>,
}
impl Presentation {
    pub fn parse(v: Value) -> Result<Self> {
        size(&v, 65536)?;
        required(
            &v,
            &[
                "turn_id",
                "scope",
                "scope_fingerprint",
                "presentation_id",
                "presentation_fingerprint",
                "renderer",
                "reason",
                "expires_at",
                "tool_policy",
                "page",
            ],
        )?;
        ensure!(v["profile"] == PROFILE, "wrong presentation profile");
        for k in ["interaction_id", "response_id"] {
            id(v[k].as_str().context("missing identity")?)?;
        }
        ensure!(
            v["revision"].as_u64().is_some_and(|n| n > 0),
            "invalid revision"
        );
        let p = ExecutionPolicy::parse(&v["execution_policy"])?;
        let scope = if v["scope"].is_null() {
            None
        } else {
            Some(Scope::parse(&v["scope"], &p)?)
        };
        if let Some(s) = &scope {
            ensure!(
                v["response_id"] == s.response_id && v["turn_id"] == s.turn_id,
                "scope identity mismatch"
            );
            id(v["scope_fingerprint"]
                .as_str()
                .context("missing scope fingerprint")?)?;
        } else {
            ensure!(v["scope_fingerprint"].is_null(), "unbound fingerprint");
        }
        let audience = &v["audience"];
        required(audience, &["kind", "channel_id", "principal_id"])?;
        let kind = audience["kind"].as_str().context("audience kind")?;
        enum_value(kind, &["source_conversation", "requester"])?;
        let ch = audience["channel_id"]
            .as_str()
            .context("audience channel")?;
        id(ch)?;
        ensure!(ch.len() <= 128, "audience length");
        if kind == "source_conversation" {
            ensure!(audience["principal_id"].is_null(), "public principal");
        } else {
            id(audience["principal_id"]
                .as_str()
                .context("private principal")?)?;
        }
        if let Some(s) = &scope {
            ensure!(
                ch == s.context.channel_id
                    && (kind != "requester" || audience["principal_id"] == s.context.principal_id),
                "audience scope mismatch"
            );
        }
        let integrity = &v["argument_integrity"];
        required(integrity, &["status", "source", "reason"])?;
        ensure!(integrity["source"] == "codex_call_event", "argument source");
        let complete = integrity["status"] == "complete";
        ensure!(
            complete || integrity["status"] == "unavailable",
            "argument integrity"
        );
        if complete {
            ensure!(
                integrity["reason"].is_null(),
                "complete arguments with failure"
            );
        } else {
            enum_value(
                integrity["reason"]
                    .as_str()
                    .context("argument failure reason")?,
                &[
                    "call_unbound",
                    "arguments_unavailable",
                    "arguments_invalid",
                    "arguments_too_large",
                    "operation_details_expired",
                ],
            )?;
        }
        let sem = &v["semantic_assessment"];
        required(sem, &["status", "reason", "effects"])?;
        let assessed = sem["status"] == "evaluated";
        ensure!(
            assessed || sem["status"] == "unreviewed" || sem["status"] == "unavailable",
            "semantic state"
        );
        if assessed {
            ensure!(sem["reason"].is_null(), "evaluated reason");
            effects(&sem["effects"])?;
        } else {
            ensure!(sem["effects"].is_null(), "unknown semantics have effects");
            enum_value(
                sem["reason"].as_str().context("semantic reason")?,
                &[
                    "policy_not_selected",
                    "tool_not_evaluated",
                    "definition_changed",
                    "catalog_unavailable",
                ],
            )?;
        }
        let t = &v["tool_policy"];
        if p.selection.is_none() {
            ensure!(t.is_null(), "policy on unselected execution");
        } else if !t.is_null() {
            required(
                t,
                &[
                    "policy_id",
                    "version",
                    "policy_generation",
                    "definition_generation",
                    "effects",
                    "turn_eligible",
                    "reason",
                    "grant_scope",
                    "eligible_operations",
                    "always_confirm_operations",
                    "decision",
                ],
            )?;
            let sel = p.selection.as_ref().context("missing selected policy")?;
            ensure!(
                t["policy_id"] == sel.id
                    && t["version"] == sel.version
                    && t["policy_generation"] == json!(p.generation),
                "tool policy binding"
            );
            if let Some(s) = &scope {
                ensure!(
                    t["definition_generation"] == json!(s.definition_generation),
                    "tool definition mismatch"
                );
            }
            enum_value(
                t["decision"].as_str().context("policy decision")?,
                &["not_blocked", "blocked", "unavailable"],
            )?;
            ensure!(t["turn_eligible"].is_boolean(), "policy eligibility");
            for k in ["eligible_operations", "always_confirm_operations"] {
                unique_strings(&serde_json::from_value::<Vec<String>>(t[k].clone())?, 64)?;
            }
            if !t["effects"].is_null() {
                effects(&t["effects"])?;
            }
        }
        let state = v["state"].as_str().context("presentation state")?;
        enum_value(
            state,
            &["ready", "private_required", "unavailable", "closed"],
        )?;
        let a = &v["actions"];
        for k in [
            "allow_once",
            "allow_turn_tool",
            "decline",
            "open_private_details",
            "retry",
        ] {
            ensure!(a[k].is_boolean(), "action missing");
        }
        ensure!(
            v["reason"] == v["diagnostic"]["code"],
            "diagnostic mismatch"
        );
        ensure!(
            v["diagnostic"]["retryable"].is_boolean(),
            "diagnostic retry"
        );
        if v["diagnostic"]["retryable"] == true {
            ensure!(v["diagnostic"]["retry_after_ms"] == 2000, "retry interval");
        } else {
            ensure!(
                v["diagnostic"]["retry_after_ms"].is_null(),
                "unexpected retry interval"
            );
        }
        if matches!(state, "ready" | "private_required") {
            for k in ["presentation_id", "presentation_fingerprint"] {
                id(v[k].as_str().context("display identity")?)?;
            }
            time(v["expires_at"].as_str().context("display expiry")?)?;
            let page = &v["page"];
            let n = page["count"].as_u64().context("page count")?;
            let i = page["index"].as_u64().context("page index")?;
            ensure!(
                (1..=64).contains(&n) && i < n && (kind != "source_conversation" || n == 1),
                "page bounds"
            );
            let token = page["token"].as_str().context("page token")?;
            ensure!(
                !token.is_empty() && token.is_ascii() && token.len() <= 256,
                "page token limit"
            );
            id(page["content_fingerprint"]
                .as_str()
                .context("page fingerprint")?)?;
        } else {
            for k in [
                "presentation_id",
                "presentation_fingerprint",
                "expires_at",
                "page",
            ] {
                ensure!(v[k].is_null(), "invalid unavailable display");
            }
        }
        validate_display(&v["display"], kind)?;
        if state == "ready" {
            ensure!(
                v["display"]["provenance"] == "proxy_verified_call",
                "ready display lacks provenance"
            );
            ensure!(
                v["reason"].is_null() && complete && scope.is_some() && p.state == "ready",
                "ready lacks execution evidence"
            );
            enum_value(
                v["renderer"].as_str().context("renderer")?,
                &["raw-arguments-v1", "evaluated-operation-v1"],
            )?;
            ensure!(
                a["decline"] == true && a["open_private_details"] == false && a["retry"] == false,
                "ready actions"
            );
        } else {
            ensure!(
                a["allow_once"] == false && a["allow_turn_tool"] == false,
                "non-ready permission"
            );
        }
        if state == "private_required" {
            ensure!(
                kind == "source_conversation" && a["open_private_details"] == true,
                "private entry"
            );
        }
        if state == "closed" {
            ensure!(
                a.as_object()
                    .context("actions")?
                    .values()
                    .all(|x| x == false),
                "closed actions"
            );
        }
        if a["allow_once"] == true {
            ensure!(
                v["display"]["omissions"]
                    .as_array()
                    .is_some_and(Vec::is_empty),
                "incomplete display"
            );
            ensure!(
                p.selection.is_none() || (!t.is_null() && t["decision"] == "not_blocked"),
                "unchecked policy"
            );
        }
        if assessed && !t.is_null() {
            ensure!(
                t["effects"] == sem["effects"],
                "policy and semantic effects differ"
            );
        }
        if a["allow_turn_tool"] == true {
            ensure!(
                a["allow_once"] == true
                    && assessed
                    && p.selection.is_some()
                    && t["turn_eligible"] == true
                    && t["grant_scope"] == "turn_tool"
                    && t["reason"].is_null()
                    && scope.as_ref().is_some_and(
                        |s| s.definition_generation.is_some() && s.policy_generation.is_some()
                    ),
                "invalid turn permission"
            );
        }
        Ok(Self {
            value: v,
            policy: p,
            scope,
        })
    }
    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn permit_body(&self, turn: bool, tokens: &[String], now_ms: i64) -> Result<Value> {
        let v = &self.value;
        ensure!(
            v["state"] == "ready"
                && v["actions"][if turn {
                    "allow_turn_tool"
                } else {
                    "allow_once"
                }] == true,
            "permission unavailable"
        );
        ensure!(
            time(v["expires_at"].as_str().context("expiry")?)? > now_ms,
            "display expired"
        );
        ensure!(
            tokens.len() == v["page"]["count"].as_u64().context("page count")? as usize,
            "incomplete pages"
        );
        let mut seen = std::collections::HashSet::new();
        for t in tokens {
            ensure!(
                !t.is_empty() && t.is_ascii() && t.len() <= 256 && seen.insert(t),
                "invalid page tokens"
            );
        }
        ensure!(
            tokens
                .get(v["page"]["index"].as_u64().context("page index")? as usize)
                .is_some_and(|t| v["page"]["token"] == *t),
            "page token mismatch"
        );
        let mut body = json!({"expected_revision":v["revision"],"expected_scope_fingerprint":v["scope_fingerprint"],"approval_view":v["audience"]["kind"],"expected_presentation_fingerprint":v["presentation_fingerprint"],"expected_page_tokens":tokens,"expected_policy_binding_id":self.policy.binding_id,"response":{"action":"accept","content":{}}});
        if turn {
            body["grant_scope"] = json!("turn_tool");
        }
        Ok(body)
    }
}
fn effects(v: &Value) -> Result<()> {
    let xs = v.as_array().context("effects array")?;
    ensure!(!xs.is_empty() && xs.len() <= 9, "effects count");
    let mut seen = std::collections::HashSet::new();
    for x in xs {
        let x = x.as_str().context("effect")?;
        enum_value(
            x,
            &[
                "read",
                "write",
                "delete",
                "external_send",
                "permission_change",
                "execute",
                "credential",
                "session_change",
                "unknown",
            ],
        )?;
        ensure!(seen.insert(x), "duplicate effect");
    }
    Ok(())
}
fn validate_display(v: &Value, kind: &str) -> Result<()> {
    required(
        v,
        &[
            "title",
            "fields",
            "limitations",
            "omissions",
            "disclosure",
            "provenance",
        ],
    )?;
    ensure!(
        v["disclosure"]
            == if kind == "requester" {
                "requester_only"
            } else {
                "source_conversation"
            },
        "display audience mismatch"
    );
    enum_value(
        v["provenance"].as_str().context("display provenance")?,
        &["proxy_verified_call", "unavailable"],
    )?;
    let mut count = 0;
    let mut part = |v: &Value, max: usize| -> Result<()> {
        let s = v.as_str().context("display text")?;
        let n = s.encode_utf16().count();
        ensure!(n <= max, "display text limit");
        count += n;
        Ok(())
    };
    part(&v["title"], 200)?;
    let fs = v["fields"].as_array().context("display fields")?;
    ensure!(fs.len() <= 24, "field count");
    for f in fs {
        part(&f["label"], 200)?;
        part(&f["value"], 1000)?;
    }
    for k in ["limitations", "omissions"] {
        for x in v[k].as_array().context("display list")? {
            part(x, 4800)?;
        }
    }
    ensure!(count <= 4800, "page display limit");
    Ok(())
}
/// Decline deliberately does not depend on presentation, scope, or policy binding.
pub fn decline_body(revision: u64) -> Result<Value> {
    ensure!(revision > 0, "invalid revision");
    Ok(json!({"expected_revision":revision,"response":{"action":"decline"}}))
}

impl crate::storage::Store {
    /// Freeze a selection before dispatch. Existing requests cannot change policy.
    pub async fn fix_v06_selection(
        &self,
        request: &str,
        selection: Option<Selection>,
    ) -> Result<()> {
        if let Some(s) = &selection {
            s.validate()?;
        }
        let request = request.to_owned();
        let selection = serde_json::to_string(&selection)?;
        self.call(true, move |c| {
            use rusqlite::OptionalExtension;
            let tx = c.transaction()?;
            let old: Option<String> = tx
                .query_row(
                    "SELECT selection_json FROM mcp_v06_runs WHERE request_id=?1",
                    [&request],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(old) = old {
                ensure!(old == selection, "request policy is immutable");
            } else {
                let (state, started): (String, Option<i64>) = tx.query_row(
                    "SELECT state,dispatch_started_at FROM requests WHERE id=?1",
                    [&request],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                ensure!(
                    matches!(state.as_str(), "RECEIVED" | "QUEUED") && started.is_none(),
                    "cannot change dispatched profile"
                );
                tx.execute(
                    "INSERT INTO mcp_v06_runs(request_id,profile,selection_json) VALUES(?1,?2,?3)",
                    rusqlite::params![request, PROFILE, selection],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn v06_selection(&self, request: &str) -> Result<Option<Option<Selection>>> {
        let request = request.to_owned();
        self.call(false, move |c| {
            use rusqlite::OptionalExtension;
            let v: Option<String> = c
                .query_row(
                    "SELECT selection_json FROM mcp_v06_runs WHERE request_id=?1",
                    [request],
                    |r| r.get(0),
                )
                .optional()?;
            v.map(|s| serde_json::from_str(&s).map_err(Into::into))
                .transpose()
        })
        .await
    }
    /// CAS prevents a slower policy read from replacing a newer observation.
    pub async fn save_v06_policy(
        &self,
        request: &str,
        policy: ExecutionPolicy,
        expected_version: i64,
    ) -> Result<()> {
        policy.validate()?;
        let request = request.to_owned();
        self.call(true,move|c|{
            let tx=c.transaction()?;
            let (selection,old,version):(String,Option<String>,i64)=tx.query_row("SELECT selection_json,execution_policy_json,version FROM mcp_v06_runs WHERE request_id=?1",[&request],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            ensure!(version==expected_version,"policy observation superseded");
            ensure!(serde_json::from_str::<Option<Selection>>(&selection)?==policy.selection,"requested/effective policy mismatch");
            if let Some(old)=old {
                let old=ExecutionPolicy::parse(&serde_json::from_str(&old)?)?;
                ensure!(old.binding_id==policy.binding_id && old.preparation.started_at==policy.preparation.started_at && old.preparation.deadline_at==policy.preparation.deadline_at,"policy binding/deadline changed");
                if matches!(old.state.as_str(),"failed"|"closed") {ensure!(old.state==policy.state && old.reason==policy.reason,"terminal policy resurrected");}
                ensure!(old.generation.is_none() || old.generation==policy.generation,"policy generation changed");
                ensure!(old.state!="ready" || policy.state!="preparing","policy readiness regressed");
                if old.preparation.turn_start_status!="not_sent" {ensure!(policy.preparation.turn_start_status!="not_sent","turn evidence regressed");}
            }
            tx.execute("UPDATE mcp_v06_runs SET execution_policy_json=?2,binding_id=?3,version=version+1 WHERE request_id=?1",rusqlite::params![request,serde_json::to_string(&policy)?,policy.binding_id])?;
            tx.commit()?;Ok(())
        }).await
    }
}

/// Display only the Proxy's prepared fields. Never reinterpret operation.arguments.
/// Each fragment is independently escaped before splitting, so escape pairs stay intact.
pub fn render_page(p: &Presentation) -> Result<Vec<String>> {
    let d = &p.value()["display"];
    let mut blocks = Vec::new();
    blocks.push(d["title"].as_str().context("title")?.to_owned());
    for f in d["fields"].as_array().context("fields")? {
        blocks.push(format!(
            "{}\n{}",
            f["label"].as_str().context("label")?,
            f["value"].as_str().context("value")?
        ));
    }
    for x in d["limitations"].as_array().context("limitations")? {
        blocks.push(x.as_str().context("limitation")?.to_owned());
    }
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for c in blocks.join("\n\n").chars() {
        let escaped = match c {
            '\n' => "\n".to_owned(),
            c if c.is_control()
                || matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}') =>
            {
                format!("\\u{{{:x}}}", c as u32)
            }
            c if "\\`*_{}[]()#+-.!|>~<@".contains(c) => format!("\\{c}"),
            c => c.to_string(),
        };
        let n = escaped.encode_utf16().count();
        if units + n > 1800 {
            chunks.push(std::mem::take(&mut chunk));
            units = 0;
        }
        chunk.push_str(&escaped);
        units += n;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    ensure!(
        !chunks.is_empty()
            && chunks.len() <= 32
            && chunks
                .iter()
                .map(|s| s.encode_utf16().count())
                .sum::<usize>()
                <= 32768,
        "render size exceeded"
    );
    let count = chunks.len();
    let page = p.value()["page"]["index"].as_u64().unwrap_or(0) + 1;
    let pages = p.value()["page"]["count"].as_u64().unwrap_or(1);
    for (i, s) in chunks.iter_mut().enumerate() {
        *s = format!("{page}/{pages}ページ · {}/{count}\n{s}", i + 1);
        if i + 1 == count && p.value()["actions"]["allow_turn_tool"] == true {
            s.push_str("\n\n依頼中の許可は、このツールの引数が変わる呼出しも対象です。次の発言・停止には引き継ぎません。");
        }
        ensure!(
            s.encode_utf16().count() <= 1900,
            "rendered post exceeded limit"
        );
    }
    Ok(chunks)
}

/// Per-view, in-memory proof. The controller must supply confirmed Discord message IDs.
/// This object contains no display text and cannot be recovered as READY after restart.
pub struct PageReceipts {
    identity: Value,
    tokens: Vec<Option<String>>,
    messages: Vec<Vec<String>>,
    display_bytes: usize,
    display_digests: Vec<Option<String>>,
}
impl PageReceipts {
    pub fn new(p: &Presentation) -> Result<Self> {
        ensure!(p.value()["state"] == "ready", "not a confirmable view");
        let n = p.value()["page"]["count"].as_u64().context("page count")? as usize;
        Ok(Self {
            identity: page_identity(p),
            tokens: vec![None; n],
            messages: vec![vec![]; n],
            display_bytes: 0,
            display_digests: vec![None; n],
        })
    }
    pub fn confirm(&mut self, p: &Presentation, ids: &[String]) -> Result<()> {
        ensure!(page_identity(p) == self.identity, "page view changed");
        let i = p.value()["page"]["index"].as_u64().context("page index")? as usize;
        let rendered = render_page(p)?;
        ensure!(ids.len() == rendered.len(), "not all fragments delivered");
        let mut unique = std::collections::HashSet::new();
        for id in ids {
            crate::discord::snowflake(id)?;
            ensure!(unique.insert(id), "duplicate fragment receipt");
        }
        for (j, old) in self.messages.iter().enumerate() {
            if i != j {
                ensure!(
                    old.iter().all(|id| !ids.contains(id)),
                    "message reused across pages"
                );
            }
        }
        let token = p.value()["page"]["token"]
            .as_str()
            .context("page token")?
            .to_owned();
        if let Some(old) = &self.tokens[i] {
            ensure!(
                old == &token
                    && self.messages[i] == ids
                    && self.display_digests[i].as_deref()
                        == Some(
                            crate::domain::digest(&serde_json::to_vec(&p.value()["display"])?)
                                .as_str()
                        ),
                "page receipt changed"
            );
            return Ok(());
        }
        ensure!(self.tokens[..i].iter().all(Option::is_some), "page skipped");
        ensure!(
            !self.tokens.iter().flatten().any(|t| t == &token),
            "page token reused"
        );
        let bytes = serde_json::to_vec(&p.value()["display"])?.len();
        ensure!(self.display_bytes + bytes <= 262144, "total display limit");
        self.display_bytes += bytes;
        self.display_digests[i] = Some(crate::domain::digest(&serde_json::to_vec(
            &p.value()["display"],
        )?));
        self.tokens[i] = Some(token);
        self.messages[i] = ids.to_vec();
        Ok(())
    }
    pub fn permit_body(&self, current: &Presentation, turn: bool, now: i64) -> Result<Value> {
        ensure!(
            page_identity(current) == self.identity,
            "current page changed"
        );
        let page = current.value()["page"]["index"]
            .as_u64()
            .context("page index")? as usize;
        ensure!(
            self.display_digests[page].as_deref()
                == Some(
                    crate::domain::digest(&serde_json::to_vec(&current.value()["display"])?)
                        .as_str()
                ),
            "display content changed without fingerprint"
        );
        let tokens = self
            .tokens
            .iter()
            .cloned()
            .collect::<Option<Vec<_>>>()
            .context("pages not confirmed")?;
        current.permit_body(turn, &tokens, now)
    }
}
fn page_identity(p: &Presentation) -> Value {
    let v = p.value();
    json!({"interaction_id":v["interaction_id"],"response_id":v["response_id"],"turn_id":v["turn_id"],"revision":v["revision"],"scope":v["scope"],"scope_fingerprint":v["scope_fingerprint"],"presentation_id":v["presentation_id"],"presentation_fingerprint":v["presentation_fingerprint"],"audience":v["audience"],"expires_at":v["expires_at"],"count":v["page"]["count"],"content_fingerprint":v["page"]["content_fingerprint"]})
}

impl crate::proxy::Proxy {
    /// Use only for resources whose persisted Response profile is 0.6.
    pub async fn approval_v06_json(
        &self,
        method: reqwest::Method,
        path: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Value> {
        self.v2_json_limited(method, path, key, body, response_limit(path, true))
            .await
    }
    pub async fn approval_v06_page(
        &self,
        interaction: &str,
        requester: bool,
        page: u64,
        presentation: Option<&str>,
    ) -> Result<Presentation> {
        ensure!(
            page < 64 && (requester || page == 0) && (page == 0 || presentation.is_some()),
            "invalid page request"
        );
        id(interaction)?;
        let mut url = reqwest::Url::parse("http://localhost/v2/codex/interactions/")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid API base"))?
            .pop_if_empty()
            .push(interaction)
            .push("presentation");
        let mut path = url.path().to_owned();
        let params = url_params(requester, page, presentation)?;
        if !params.is_empty() {
            path.push('?');
            path.push_str(&params);
        }
        let v = self
            .approval_v06_json(reqwest::Method::GET, &path, None, None)
            .await?;
        let p = Presentation::parse(v)?;
        ensure!(
            p.value()["interaction_id"] == interaction,
            "presentation target mismatch"
        );
        ensure!(
            p.value()["audience"]["kind"]
                == if requester {
                    "requester"
                } else {
                    "source_conversation"
                },
            "presentation audience mismatch"
        );
        if !p.value()["page"].is_null() {
            ensure!(p.value()["page"]["index"] == page, "wrong page returned");
        }
        if let Some(id) = presentation {
            // Non-ready catalog responses carry no presentation identity. They
            // cannot authorize anything; the caller must fetch and verify the
            // original presentation again after the catalog is available.
            if !p.value()["presentation_id"].is_null() {
                ensure!(p.value()["presentation_id"] == id, "presentation replaced");
            }
        }
        Ok(p)
    }
}
fn url_params(requester: bool, page: u64, presentation: Option<&str>) -> Result<String> {
    // reqwest::Url handles percent encoding for query values; opaque IDs are not paths.
    let mut url = reqwest::Url::parse("http://localhost/")?;
    {
        let mut query = url.query_pairs_mut();
        if requester {
            query.append_pair("audience", "requester");
            query.append_pair("page", &page.to_string());
        }
        if let Some(p) = presentation {
            id(p)?;
            query.append_pair("presentation_id", p);
        }
    }
    Ok(url.query().unwrap_or("").to_owned())
}

impl crate::storage::Store {
    pub async fn observe_v06_policy(&self, request: &str, value: &Value) -> Result<()> {
        let id = request.to_owned();
        let version = self
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT version FROM mcp_v06_runs WHERE request_id=?1",
                    [id],
                    |r| r.get(0),
                )?)
            })
            .await?;
        self.save_v06_policy(request, ExecutionPolicy::parse(value)?, version)
            .await
    }
    pub async fn v06_status(&self, request: &str) -> Result<Option<String>> {
        let id = request.to_owned();
        let value: Option<String> = self
            .call(false, move |c| {
                use rusqlite::OptionalExtension;
                Ok(c.query_row(
                    "SELECT execution_policy_json FROM mcp_v06_runs WHERE request_id=?1",
                    [id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
            })
            .await?;
        let Some(value) = value else { return Ok(None) };
        let p = ExecutionPolicy::parse(&serde_json::from_str(&value)?)?;
        Ok(Some(if p.preparation.turn_start_status=="not_sent" && p.preparation.configuration_isolation=="pending" && matches!(p.state.as_str(),"failed"|"closed") {
            "AIはまだ開始していません。Proxyの設定処理の終了を確認しています。新しい依頼は待機します。再実行せずお待ちください。"
        }else if p.state=="preparing" {
            "実行の準備中です。開始を待つか、取りやめる場合は /stop を選んでください。"
        }else if p.state=="failed" {
            "実行準備に失敗しました。/status で占有解除を確認し、設定を確認してから改めて依頼してください。自動再実行はしません。"
        }else {return Ok(None)}.to_owned()))
    }
}
/// Reject inconsistent release claims instead of freeing an uncertain workspace.
pub fn validate_response_policy(v: &Value) -> Result<()> {
    let p = ExecutionPolicy::parse(&v["approval_policy"])?;
    if v["execution_status"] == "not_started" {
        ensure!(
            p.preparation.turn_start_status == "not_sent",
            "execution evidence regressed"
        );
        if matches!(v["phase"].as_str(), Some("rejected" | "cancelled")) {
            ensure!(
                p.preparation.configuration_isolation != "pending"
                    && matches!(p.state.as_str(), "failed" | "closed"),
                "premature policy release"
            );
        }
    }
    if v["execution_status"] == "in_progress" {
        ensure!(
            p.preparation.turn_start_status == "confirmed",
            "unconfirmed running execution"
        );
    }
    Ok(())
}
pub fn validate_grant(v: &Value) -> Result<()> {
    ensure!(v["profile"] == PROFILE, "grant profile mismatch");
    let p = ExecutionPolicy::parse(&v["execution_policy"])?;
    let s = Scope::parse(&v["scope"], &p)?;
    let sel = p.selection.context("unselected grant")?;
    let g = &v["grant_policy"];
    ensure!(
        g["policy_id"] == sel.id
            && g["policy_version"] == sel.version
            && g["execution_policy_binding_id"] == p.binding_id
            && g["policy_generation"] == json!(s.policy_generation)
            && g["definition_generation"] == json!(s.definition_generation)
            && g["grant_scope"] == "turn_tool",
        "grant policy binding mismatch"
    );
    effects(&g["allowed_effects"])?;
    effects(&g["always_confirm_effects"])?;
    for k in ["eligible_operations", "always_confirm_operations"] {
        unique_strings(&serde_json::from_value::<Vec<String>>(g[k].clone())?, 64)?;
    }
    let a = &v["availability"];
    match a["state"].as_str() {
        Some("ready") => ensure!(
            a["reason"].is_null() && a["retry_after_ms"].is_null(),
            "ready grant availability"
        ),
        Some("refreshing") => ensure!(
            a["reason"] == "catalog_loading" && a["retry_after_ms"] == 2000,
            "refreshing grant availability"
        ),
        Some("inactive") => {
            enum_value(
                a["reason"].as_str().context("inactive grant reason")?,
                &[
                    "activation_pending",
                    "catalog_failed",
                    "catalog_changed",
                    "policy_changed",
                    "config_changed",
                    "input_changed",
                    "scope_ended",
                    "operator_revoked",
                    "expired",
                    "runtime_restarted",
                    "response_unknown",
                ],
            )?;
            ensure!(a["retry_after_ms"].is_null(), "inactive retry");
        }
        _ => anyhow::bail!("unknown grant availability"),
    }
    ensure!(
        s.definition_generation.is_some() && s.policy_generation.is_some(),
        "grant requires evaluated generations"
    );
    Ok(())
}
