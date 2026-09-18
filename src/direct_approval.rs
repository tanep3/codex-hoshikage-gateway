//! Correlates an App Server initiated request with exactly one active Run.
//! Decisions and Discord rendering belong to the approval coordinator.
use crate::{codex_execution::TurnIdentity, codex_transport::Event, domain};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InteractionKind {
    CommandApproval,
    FileChangeApproval,
    UserInput,
    PermissionsApproval,
    McpElicitation,
    DynamicTool,
}

#[derive(Clone, Debug)]
pub struct DirectInteraction {
    pub rpc_id: Value,
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: Option<String>,
    pub kind: InteractionKind,
    pub method: String,
    /// Exact upstream content for a private, authorized view. Never send this
    /// field directly to a public Discord message or logs.
    pub params: Value,
    pub fingerprint: String,
    /// A matching `item/started` MCP call. The question text alone cannot
    /// establish which server, tool, or arguments are about to run.
    pub mcp_evidence: Option<Value>,
}

impl DirectInteraction {
    pub fn from_event(event: &Event, active: &TurnIdentity) -> Result<Option<Self>> {
        let Event::ServerRequest { id, method, params } = event else {
            return Ok(None);
        };
        let kind = match method.as_str() {
            "item/commandExecution/requestApproval" => InteractionKind::CommandApproval,
            "item/fileChange/requestApproval" => InteractionKind::FileChangeApproval,
            "item/tool/requestUserInput" | "tool/requestUserInput" => InteractionKind::UserInput,
            "item/permissions/requestApproval" => InteractionKind::PermissionsApproval,
            "mcpServer/elicitation/request" => InteractionKind::McpElicitation,
            "item/tool/call" => InteractionKind::DynamicTool,
            _ => return Ok(None),
        };
        ensure!(
            id.is_string() || id.is_i64() || id.is_u64(),
            "invalid App Server request ID"
        );
        ensure!(
            params["threadId"].as_str() == Some(active.thread_id.as_str()),
            "App Server request belongs to another thread"
        );
        let wire_turn = params["turnId"].as_str();
        ensure!(
            wire_turn == Some(active.turn_id.as_str())
                || (kind == InteractionKind::McpElicitation && wire_turn.is_none()),
            "App Server request belongs to another turn"
        );
        let item_id = match kind {
            InteractionKind::DynamicTool => params["callId"].as_str(),
            _ => params["itemId"].as_str(),
        }
        .map(str::to_owned);
        ensure!(
            matches!(kind, InteractionKind::McpElicitation) || item_id.is_some(),
            "App Server request item ID missing"
        );
        let canonical = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))?;
        ensure!(
            canonical.len() <= MAX_REQUEST_BYTES,
            "App Server request exceeds approval limit"
        );
        Ok(Some(Self {
            rpc_id: id.clone(),
            thread_id: active.thread_id.clone(),
            turn_id: active.turn_id.clone(),
            item_id,
            kind,
            method: method.clone(),
            params: params.clone(),
            fingerprint: domain::digest(&canonical),
            mcp_evidence: None,
        }))
    }

    pub fn bind_mcp_evidence(&mut self, item: Value) -> Result<()> {
        ensure!(
            matches!(
                self.kind,
                InteractionKind::UserInput | InteractionKind::McpElicitation
            ),
            "MCP evidence on another interaction"
        );
        ensure!(
            item["type"] == "mcpToolCall"
                && (self.kind == InteractionKind::McpElicitation
                    || item["id"].as_str() == self.item_id.as_deref())
                && item["server"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 128)
                && item["tool"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 128)
                && item["arguments"].is_object(),
            "MCP call evidence does not match the question"
        );
        if self.kind == InteractionKind::McpElicitation {
            let expected_message = format!(
                "Allow the {} MCP server to run tool \"{}\"?",
                item["server"].as_str().unwrap_or(""),
                item["tool"].as_str().unwrap_or("")
            );
            ensure!(
                self.params["serverName"] == item["server"]
                    && self.params["_meta"]["tool_params"] == item["arguments"]
                    && self.params["message"] == expected_message
                    && item["id"].as_str().is_some_and(|s| !s.is_empty()),
                "MCP call evidence does not match the elicitation"
            );
        }
        ensure!(
            serde_json::to_vec(&item)?.len() <= MAX_REQUEST_BYTES,
            "MCP call evidence too large"
        );
        self.fingerprint =
            domain::digest(serde_json::to_vec(&json!([self.fingerprint, item]))?.as_slice());
        self.mcp_evidence = Some(item);
        Ok(())
    }

    /// A Run grant is deliberately narrower than Codex's native session
    /// persistence: the actor can revoke it before a Steer or Stop.
    pub fn run_grant_tool(&self) -> Option<(String, String)> {
        if self.kind != InteractionKind::McpElicitation
            || self.mcp_tool_decision(ManualDecision::AcceptOnce).is_err()
            || !self.params["_meta"]["persist"]
                .as_array()
                .is_some_and(|choices| choices.iter().any(|choice| choice == "session"))
        {
            return None;
        }
        let evidence = self.mcp_evidence.as_ref()?;
        let server = evidence["server"].as_str()?;
        let tool = evidence["tool"].as_str()?;
        let lower = tool.to_ascii_lowercase();
        if [
            "unsafe", "evaluate", "run_code", "execute", "eval", "delete", "remove", "purchase",
            "payment", "secret", "password",
        ]
        .iter()
        .any(|word| lower.contains(word))
        {
            return None;
        }
        Some((server.into(), tool.into()))
    }

    /// Only the exact upstream command/file-change schema can use this reply.
    /// Session-wide acceptance is deliberately excluded from the direct-mode
    /// manual path; a separate explicit grant design is needed for that.
    pub fn manual_decision(&self, decision: ManualDecision) -> Result<Value> {
        ensure!(
            matches!(
                self.kind,
                InteractionKind::CommandApproval | InteractionKind::FileChangeApproval
            ),
            "this interaction needs a method-specific reply"
        );
        let wire = match decision {
            ManualDecision::AcceptOnce => "accept",
            ManualDecision::Decline => "decline",
            ManualDecision::Cancel => "cancel",
        };
        if let Some(offered) = self.params.get("availableDecisions") {
            let choices = offered
                .as_array()
                .context("App Server decisions are not an array")?;
            ensure!(
                choices.iter().any(|choice| choice.as_str() == Some(wire)),
                "decision not offered by App Server"
            );
        }
        Ok(json!({"decision":wire}))
    }

    /// The current Codex MCP prompt is one non-secret Allow/Cancel question.
    /// Its wire shape differs from command/file approvals. Other user input
    /// must use an explicit form coordinator and cannot use these buttons.
    pub fn mcp_tool_decision(&self, decision: ManualDecision) -> Result<Value> {
        if self.kind == InteractionKind::McpElicitation {
            return self.mcp_form_tool_decision(decision);
        }
        ensure!(
            self.kind == InteractionKind::UserInput && self.method == "item/tool/requestUserInput",
            "not a Codex MCP tool confirmation"
        );
        if decision == ManualDecision::AcceptOnce {
            ensure!(
                self.mcp_evidence.is_some(),
                "matching MCP tool call evidence is missing"
            );
        }
        let questions = self.params["questions"]
            .as_array()
            .context("MCP confirmation questions missing")?;
        ensure!(
            questions.len() == 1,
            "MCP confirmation must have one question"
        );
        let question = &questions[0];
        let item = self.item_id.as_deref().context("MCP item ID missing")?;
        let id = question["id"].as_str().context("MCP question ID missing")?;
        ensure!(
            id == format!("mcp_tool_call_approval_{item}")
                && question["isOther"] == false
                && question["isSecret"] == false,
            "MCP confirmation shape is not trusted"
        );
        let offered = question["options"]
            .as_array()
            .context("MCP confirmation options missing")?;
        ensure!(
            offered.iter().any(|option| option["label"] == "Allow")
                && offered.iter().any(|option| option["label"] == "Cancel"),
            "MCP confirmation choices are incomplete"
        );
        let choice = match decision {
            ManualDecision::AcceptOnce => "Allow",
            ManualDecision::Decline | ManualDecision::Cancel => "Cancel",
        };
        let mut answers = serde_json::Map::new();
        answers.insert(id.into(), json!({"answers":[choice]}));
        Ok(json!({"answers":answers}))
    }

    fn mcp_form_tool_decision(&self, decision: ManualDecision) -> Result<Value> {
        ensure!(
            self.method == "mcpServer/elicitation/request",
            "not an MCP elicitation"
        );
        if decision != ManualDecision::AcceptOnce {
            return Ok(json!({"action":"decline","content":null}));
        }
        ensure!(
            self.params["mode"] == "form"
                && self.params["_meta"]["codex_approval_kind"] == "mcp_tool_call"
                && self.params["threadId"] == self.thread_id
                && self.params["turnId"] == self.turn_id,
            "not a turn-bound MCP tool confirmation"
        );
        ensure!(
            self.params["serverName"]
                .as_str()
                .is_some_and(|text| !text.is_empty() && text.len() <= 128)
                && self.params["message"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty() && text.len() <= 8192)
                && self.params["_meta"]["tool_params"].is_object(),
            "MCP tool confirmation details are incomplete"
        );
        let schema = self.params["requestedSchema"]
            .as_object()
            .context("MCP tool confirmation schema missing")?;
        ensure!(
            schema.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "type" | "properties" | "required" | "additionalProperties"
                )
            }) && schema.get("type") == Some(&json!("object"))
                && schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|p| p.is_empty())
                && schema
                    .get("required")
                    .is_none_or(|v| v.as_array().is_some_and(|r| r.is_empty()))
                && schema
                    .get("additionalProperties")
                    .is_none_or(|v| v == false),
            "MCP tool confirmation is not an empty form"
        );
        Ok(json!({"action":"accept","content":{}}))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualDecision {
    AcceptOnce,
    Decline,
    Cancel,
}

#[derive(Clone, Debug)]
pub enum RunGrantAudit {
    Selected {
        server: String,
        tool: String,
    },
    Applied {
        initial_interaction_id: String,
        server: String,
        tool: String,
    },
}
