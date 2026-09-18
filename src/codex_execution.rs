//! Typed Codex operations above stdio. Persistence and admission belong to the
//! Gateway application; callers must commit an execution intent before start.
use crate::codex_transport::{CodexTransport, TransportError};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf};

#[derive(Clone)]
pub struct CodexExecution {
    transport: CodexTransport,
}

#[derive(Clone, Debug)]
pub struct ExecutionOptions {
    pub cwd: PathBuf,
    pub model: String,
    pub model_provider: String,
    pub sandbox: String,
    pub approval_policy: String,
    pub network_access: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnIdentity {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Clone, Debug)]
pub struct TurnSnapshot {
    pub identity: TurnIdentity,
    pub status: String,
    pub final_text: Option<String>,
    pub items_view_full: bool,
    pub items: Vec<Value>,
}

impl CodexExecution {
    pub fn new(transport: CodexTransport) -> Self {
        Self { transport }
    }

    pub async fn start_thread(&self, options: &ExecutionOptions) -> Result<String, TransportError> {
        let result = self
            .transport
            .request(
                "thread/start",
                json!({
                    "cwd":options.cwd,
                    "model":options.model,
                    "modelProvider":options.model_provider,
                    "sandbox":options.sandbox,
                    "approvalPolicy":options.approval_policy,
                }),
            )
            .await?;
        result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                TransportError::ResultUnknown("thread/start response has no thread ID".into())
            })
    }

    pub async fn resume_thread(
        &self,
        thread_id: &str,
        options: &ExecutionOptions,
    ) -> Result<(), TransportError> {
        let result = self
            .transport
            .request(
                "thread/resume",
                json!({
                    "threadId":thread_id,
                    "cwd":options.cwd,
                    "model":options.model,
                    "modelProvider":options.model_provider,
                }),
            )
            .await?;
        if result.pointer("/thread/id").and_then(Value::as_str) != Some(thread_id) {
            return Err(TransportError::ResultUnknown(
                "thread/resume returned a different thread".into(),
            ));
        }
        Ok(())
    }

    /// Not idempotent. The caller must persist intent and never automatically
    /// repeat this call after `ResultUnknown` or task cancellation.
    pub async fn start_turn(
        &self,
        thread_id: &str,
        options: &ExecutionOptions,
        input: Vec<Value>,
    ) -> Result<TurnIdentity, TransportError> {
        if input.is_empty() {
            return Err(TransportError::NotSent("empty turn input".into()));
        }
        let sandbox_policy = match options.sandbox.as_str() {
            "workspace-write" => json!({
                "type": "workspaceWrite",
                "writableRoots": [options.cwd],
                "networkAccess": options.network_access
            }),
            "read-only" => json!({"type":"readOnly","networkAccess":options.network_access}),
            _ => return Err(TransportError::NotSent("invalid sandbox policy".into())),
        };
        let result = self
            .transport
            .request(
                "turn/start",
                json!({
                    "threadId":thread_id,
                    "cwd":options.cwd,
                    "model":options.model,
                    "input":input,
                    "approvalPolicy":options.approval_policy,
                    "sandboxPolicy":sandbox_policy,
                }),
            )
            .await?;
        let turn_id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                TransportError::ResultUnknown("turn/start response has no turn ID".into())
            })?;
        Ok(TurnIdentity {
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
        })
    }

    pub async fn read_turn(&self, identity: &TurnIdentity) -> Result<TurnSnapshot, TransportError> {
        let result = self
            .transport
            .request(
                "thread/read",
                json!({
                    "threadId":identity.thread_id,"includeTurns":true
                }),
            )
            .await?;
        if result.pointer("/thread/id").and_then(Value::as_str) != Some(&identity.thread_id) {
            return Err(TransportError::Protocol(
                "thread/read identity mismatch".into(),
            ));
        }
        let turns = result
            .pointer("/thread/turns")
            .and_then(Value::as_array)
            .ok_or_else(|| TransportError::Protocol("thread/read has no turns".into()))?;
        let turn = turns
            .iter()
            .find(|turn| turn["id"] == identity.turn_id)
            .ok_or_else(|| TransportError::ResultUnknown("turn absent from thread/read".into()))?;
        let status = turn["status"]
            .as_str()
            .ok_or_else(|| TransportError::Protocol("turn status absent".into()))?;
        let items = turn["items"]
            .as_array()
            .ok_or_else(|| TransportError::Protocol("turn items absent".into()))?;
        let final_text = items.iter().rev().find_map(|item| {
            (item["type"] == "agentMessage" && item["phase"] != "commentary")
                .then(|| item["text"].as_str())
                .flatten()
                .map(str::to_owned)
        });
        Ok(TurnSnapshot {
            identity: identity.clone(),
            status: status.into(),
            final_text,
            items_view_full: turn["itemsView"] == "full",
            items: items.clone(),
        })
    }

    /// An acknowledgement only means Codex accepted the interrupt request.
    /// The caller must observe the terminal Turn state separately.
    pub async fn interrupt(&self, identity: &TurnIdentity) -> Result<(), TransportError> {
        self.transport
            .request(
                "turn/interrupt",
                json!({
                    "threadId":identity.thread_id,"turnId":identity.turn_id
                }),
            )
            .await?;
        Ok(())
    }

    /// The approval coordinator must invalidate grants before calling this.
    pub async fn steer(
        &self,
        identity: &TurnIdentity,
        input: Vec<Value>,
    ) -> Result<(), TransportError> {
        if input.is_empty() {
            return Err(TransportError::NotSent("empty steer input".into()));
        }
        let result = self
            .transport
            .request(
                "turn/steer",
                json!({
                    "threadId":identity.thread_id,"expectedTurnId":identity.turn_id,"input":input
                }),
            )
            .await?;
        if result["turnId"].as_str() != Some(&identity.turn_id) {
            return Err(TransportError::ResultUnknown(
                "turn/steer did not confirm the expected turn".into(),
            ));
        }
        Ok(())
    }

    pub async fn list_models(&self) -> Result<Vec<Value>, TransportError> {
        let mut result = Vec::new();
        let mut cursor = Value::Null;
        let mut seen = HashSet::new();
        for _ in 0..100 {
            let page = self
                .transport
                .request(
                    "model/list",
                    json!({
                        "limit":100,"includeHidden":false,"cursor":cursor
                    }),
                )
                .await?;
            let data = page["data"]
                .as_array()
                .ok_or_else(|| TransportError::Protocol("model/list missing data".into()))?;
            result.extend(data.iter().cloned());
            cursor = page.get("nextCursor").cloned().unwrap_or(Value::Null);
            if cursor.is_null() {
                return Ok(result);
            }
            let next = cursor
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| TransportError::Protocol("model/list invalid cursor".into()))?;
            if !seen.insert(next.to_owned()) {
                return Err(TransportError::Protocol(
                    "model/list repeated cursor".into(),
                ));
            }
        }
        Err(TransportError::Protocol(
            "model/list exceeded 100 pages".into(),
        ))
    }
}
