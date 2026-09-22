use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RequestState {
    Received,
    Queued,
    Sending,
    Running,
    ApprovalRequired,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}
impl RequestState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Received => "RECEIVED",
            Self::Queued => "QUEUED",
            Self::Sending => "SENDING",
            Self::Running => "RUNNING",
            Self::ApprovalRequired => "APPROVAL_REQUIRED",
            Self::CancelRequested => "CANCEL_REQUESTED",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
            Self::Unknown => "UNKNOWN",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "RECEIVED" => Self::Received,
            "QUEUED" => Self::Queued,
            "SENDING" => Self::Sending,
            "RUNNING" => Self::Running,
            "APPROVAL_REQUIRED" => Self::ApprovalRequired,
            "CANCEL_REQUESTED" => Self::CancelRequested,
            "COMPLETED" => Self::Completed,
            "FAILED" => Self::Failed,
            "CANCELLED" => Self::Cancelled,
            "UNKNOWN" => Self::Unknown,
            _ => bail!("invalid request state"),
        })
    }
    pub fn allows(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        if self.terminal() {
            return false;
        }
        match self {
            Self::Received => matches!(next, Self::Queued | Self::Failed | Self::Cancelled),
            Self::Queued => matches!(next, Self::Sending | Self::Failed | Self::Cancelled),
            _ => !matches!(next, Self::Received | Self::Queued | Self::Sending),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub message_id: String,
    pub thread_id: String,
    pub project_id: String,
    pub sequence: i64,
    pub state: RequestState,
    pub input_digest: String,
    pub client_request_id: Option<String>,
    pub response_id: Option<String>,
    pub proxy_thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub previous_response_id: Option<String>,
    pub stop_requested: bool,
    pub dispatch_eligible: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub thread_id: String,
    pub project_id: String,
    pub paused: bool,
    pub pause_revision: i64,
    pub selected_model: String,
    pub effective_model: Option<String>,
    pub selected_reasoning_effort: String,
    pub effective_reasoning_effort: Option<String>,
    pub proxy_thread_id: Option<String>,
    pub last_response_id: Option<String>,
    pub continuation: String,
}

/// Never exposes an unfinished prefix of a known secret, even across delta boundaries.
pub struct Redactor {
    patterns: Vec<String>,
    pending: String,
}
impl Redactor {
    pub fn new(mut patterns: Vec<String>) -> Self {
        let escaped: Vec<_> = patterns
            .iter()
            .filter_map(|p| serde_json::to_string(p).ok())
            .map(|s| s[1..s.len() - 1].to_owned())
            .collect();
        patterns.extend(escaped);
        patterns.retain(|p| !p.is_empty());
        patterns.sort_by_key(|p| std::cmp::Reverse(p.len()));
        patterns.dedup();
        Self {
            patterns,
            pending: String::new(),
        }
    }
    pub fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let mut out = String::new();
        while !self.pending.is_empty() {
            if self
                .patterns
                .iter()
                .any(|p| p.len() > self.pending.len() && p.starts_with(&self.pending))
            {
                break;
            } else if let Some(p) = self
                .patterns
                .iter()
                .find(|p| self.pending.starts_with(p.as_str()))
            {
                let n = p.len();
                self.pending.drain(..n);
                out.push_str("[非公開]");
            } else if self.patterns.iter().any(|p| p.starts_with(&self.pending)) {
                break;
            } else {
                let n = self.pending.chars().next().unwrap().len_utf8();
                out.push_str(&self.pending[..n]);
                self.pending.drain(..n);
            }
        }
        out
    }
    pub fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            String::new()
        } else {
            self.pending.clear();
            "[非公開]".into()
        }
    }
}
