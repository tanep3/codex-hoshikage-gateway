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
            self.kind == InteractionKind::UserInput,
            "MCP evidence on another interaction"
        );
        ensure!(
            item["type"] == "mcpToolCall"
                && item["id"].as_str() == self.item_id.as_deref()
                && item["server"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 128)
                && item["tool"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 128)
                && item["arguments"].is_object(),
            "MCP call evidence does not match the question"
        );
        ensure!(
            serde_json::to_vec(&item)?.len() <= MAX_REQUEST_BYTES,
            "MCP call evidence too large"
        );
        self.fingerprint =
            domain::digest(serde_json::to_vec(&json!([self.fingerprint, item]))?.as_slice());
        self.mcp_evidence = Some(item);
        Ok(())
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
        ensure!(
            self.kind == InteractionKind::UserInput && self.method == "item/tool/requestUserInput",
            "not a Codex MCP tool confirmation"
        );
        ensure!(
            self.mcp_evidence.is_some(),
            "matching MCP tool call evidence is missing"
        );
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualDecision {
    AcceptOnce,
    Decline,
    Cancel,
}
