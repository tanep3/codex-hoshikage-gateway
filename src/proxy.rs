use crate::{
    domain::{Request, RequestState},
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use reqwest::{Client, Response};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Proxy {
    base: String,
    key: Arc<String>,
    stream: Client,
    control: Client,
    monitor: Client,
    pub gate: Arc<Gate>,
}
pub struct Gate {
    pub ready: AtomicBool,
    pub epoch: AtomicU64,
    verified: std::sync::Mutex<Option<Instant>>,
}
impl Gate {
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
            && self
                .verified
                .lock()
                .unwrap()
                .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
    }
    pub fn invalidate(&self) {
        self.ready.store(false, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}
pub struct Ticket {
    epoch: u64,
    created: Instant,
    digest: String,
}
pub struct DispatchPermit {
    pub(crate) request_id: String,
    pub(crate) epoch: u64,
    pub(crate) revision: i64,
    pub(crate) digest: String,
    issued: Instant,
    gate: Arc<Gate>,
}
impl DispatchPermit {
    pub(crate) fn valid(&self, id: &str) -> bool {
        self.request_id == id
            && self.issued.elapsed() < Duration::from_secs(5)
            && self.gate.is_ready()
            && self.gate.epoch.load(Ordering::SeqCst) == self.epoch
    }
}
impl Proxy {
    pub fn new(base: String, key: String) -> Result<Self> {
        let builder = || {
            Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .no_proxy()
        };
        Ok(Self {
            base: base.trim_end_matches('/').into(),
            key: Arc::new(key),
            stream: builder().build()?,
            control: builder().timeout(Duration::from_secs(15)).build()?,
            monitor: builder().build()?,
            gate: Arc::new(Gate {
                ready: AtomicBool::new(false),
                epoch: AtomicU64::new(0),
                verified: std::sync::Mutex::new(None),
            }),
        })
    }
    pub fn secret(&self) -> String {
        (*self.key).clone()
    }
    async fn get_with(&self, client: &Client, path: &str) -> Result<Value> {
        let result = client
            .get(format!("{}{}", self.base, path))
            .bearer_auth(self.key.as_str())
            .send()
            .await
            .context("Proxy GET transport error");
        let r = match result {
            Ok(r) => r,
            Err(e) => {
                self.gate.invalidate();
                return Err(e);
            }
        };
        ensure!(
            r.status().is_success(),
            "Proxy GET rejected: {}",
            r.status().as_u16()
        );
        let mut stream = r.bytes_stream();
        let mut bytes = vec![];
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Proxy GET read failure")?;
            ensure!(
                bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                "Proxy response too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).context("Proxy JSON invalid")
    }
    pub async fn get(&self, path: &str) -> Result<Value> {
        self.get_with(&self.control, path).await
    }
    pub async fn check(&self) -> Result<Ticket> {
        let epoch = self.gate.epoch.load(Ordering::SeqCst);
        let result = async {
            ensure!(
                self.get("/readyz").await?["status"] == "ready",
                "Proxy not ready"
            );
            let caps = self.get("/v1/codex/capabilities").await?;
            validate_capabilities(&caps)?;
            ensure!(
                epoch == self.gate.epoch.load(Ordering::SeqCst),
                "stale capability check"
            );
            Ok(Ticket {
                epoch,
                created: Instant::now(),
                digest: crate::domain::digest(&serde_json::to_vec(&caps)?),
            })
        }
        .await;
        if result.is_ok() {
            *self.gate.verified.lock().unwrap() = Some(Instant::now());
            self.gate.ready.store(true, Ordering::SeqCst);
        } else {
            self.gate.invalidate();
        }
        result
    }
    pub async fn authorize(&self, request_id: String, revision: i64) -> Result<DispatchPermit> {
        let ticket = self.check().await?;
        Ok(DispatchPermit {
            request_id,
            epoch: ticket.epoch,
            revision,
            digest: ticket.digest,
            issued: ticket.created,
            gate: self.gate.clone(),
        })
    }
    pub fn valid_ticket(&self, ticket: &Ticket) -> bool {
        self.gate.is_ready()
            && ticket.epoch == self.gate.epoch.load(Ordering::SeqCst)
            && ticket.created.elapsed() < Duration::from_secs(5)
    }
    pub async fn start(&self, r: &Request, input: Value, cwd: &str) -> Result<Response> {
        let mut body = json!({"model":r.model,"input":input,"stream":true,"metadata":{"codex.cwd":cwd,"codex.approval_capability":"interactive","codex.auto_approve_workspace":"false"}});
        if let Some(prev) = &r.previous_response_id {
            body["previous_response_id"] = json!(prev);
        }
        // The caller MUST durably commit SENDING before entering this function. No retry loop.
        let send = self
            .stream
            .post(format!("{}/v1/responses", self.base))
            .bearer_auth(self.key.as_str())
            .header(
                "Idempotency-Key",
                r.client_request_id
                    .as_deref()
                    .context("missing request identity")?,
            )
            .json(&body)
            .send();
        let response = tokio::time::timeout(Duration::from_secs(30), send)
            .await
            .context("Proxy start headers timed out; result unknown")?
            .context("Proxy start transport result unknown")?;
        ensure!(
            response.status().is_success(),
            "Proxy start rejected: {}",
            response.status().as_u16()
        );
        ensure!(
            response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|s| s.starts_with("text/event-stream")),
            "Proxy start did not return SSE; lookup required"
        );
        Ok(response)
    }
    pub async fn control(&self, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self
            .control
            .post(format!("{}{}", self.base, path))
            .bearer_auth(self.key.as_str());
        if let Some(v) = body {
            req = req.json(&v);
        }
        let response = req.send().await.context("control delivery unknown")?;
        let status = response.status();
        ensure!(status.is_success(), "control rejected: {}", status.as_u16());
        response.json().await.context("control response invalid")
    }
    pub async fn monitor(&self, turn: &str) -> Result<Response> {
        let r = self
            .monitor
            .get(format!(
                "{}/v1/codex/turns/{}/events/stream",
                self.base,
                path_id(turn)?
            ))
            .bearer_auth(self.key.as_str())
            .send()
            .await?;
        ensure!(r.status().is_success(), "monitor unavailable");
        Ok(r)
    }
    pub async fn reconcile(&self, store: &Store, id: &str) -> Result<RequestState> {
        let mut r = store.request(id).await?;
        if r.state.terminal() && r.state != RequestState::Completed {
            return Ok(r.state);
        }
        let resolved = async {
            let key = r
                .client_request_id
                .as_deref()
                .context("request identity unavailable")?;
            let record = self
                .get(&format!("/v1/codex/requests/{}", path_id(key)?))
                .await?;
            ensure!(
                record["client_request_id"].as_str() == Some(key),
                "request lookup identity mismatch"
            );
            if record["phase"] == "rejected" {
                return Ok((RequestState::Failed, false));
            }
            let response = field(&record, "response_id")?;
            let thread = field(&record, "thread_id")?;
            let turn = field(&record, "turn_id")?;
            store
                .identify(id.into(), response.clone(), thread.clone(), turn.clone())
                .await?;
            r = store.request(id).await?;
            let status = self
                .get(&format!("/v1/codex/turns/{}/status", path_id(&turn)?))
                .await?;
            ensure!(
                status["turn_id"] == turn
                    && status["thread_id"] == thread
                    && status["response_id"] == response,
                "turn lookup identity mismatch"
            );
            let state = match status["status"].as_str() {
                Some("inProgress") => {
                    if status["pending_approvals"]
                        .as_array()
                        .is_some_and(|a| !a.is_empty())
                    {
                        RequestState::ApprovalRequired
                    } else {
                        RequestState::Running
                    }
                }
                Some("completed") => RequestState::Completed,
                Some("failed") => RequestState::Failed,
                Some("interrupted") => RequestState::Cancelled,
                _ => RequestState::Unknown,
            };
            let can = if state == RequestState::Completed {
                self.get(&format!("/v1/codex/responses/{}", path_id(&response)?))
                    .await
                    .ok()
                    .is_some_and(|v| v["continuable"] == true)
            } else {
                false
            };
            Ok::<_, anyhow::Error>((state, can))
        }
        .await;
        let (state, can) = resolved.unwrap_or((RequestState::Unknown, false));
        store
            .observe(id.into(), state, "current_query", can)
            .await?;
        Ok(state)
    }
}
pub fn path_id(id: &str) -> Result<&str> {
    ensure!(
        !id.is_empty()
            && id.len() <= 256
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        "invalid Proxy identifier"
    );
    Ok(id)
}
pub fn field(v: &Value, name: &str) -> Result<String> {
    Ok(v[name]
        .as_str()
        .context("missing Proxy identity field")?
        .into())
}
pub fn validate_capabilities(c: &Value) -> Result<()> {
    ensure!(
        c["contract_version"] == "1.0",
        "incompatible Proxy contract"
    );
    for flag in [
        "responses",
        "streaming",
        "conversation_resume",
        "conversation_model_change",
        "identity_on_start",
        "request_lookup",
        "persistent_turn_status",
        "turn_status",
        "turn_events",
        "turn_interrupt",
        "turn_steer",
        "interactive_approval",
        "auto_approval_suppression",
        "event_reconnect",
    ] {
        ensure!(c[flag] == true, "required Proxy capability missing: {flag}");
    }
    for (k, v) in [
        ("auth_scope", json!("shared_operator")),
        ("continuation", json!("successful_response_only")),
        ("disconnect_interrupts", json!(true)),
        ("event_history_replay", json!(false)),
        ("event_reconnect", json!("snapshot_only")),
        ("steer_idempotency", json!(false)),
        ("model_change_scope", json!("same_provider")),
    ] {
        ensure!(c["limits"][k] == v, "unsupported Proxy limit: {k}");
    }
    ensure!(c["output_retrieval"] == false, "unverified output contract");
    Ok(())
}

#[derive(Debug)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}
#[derive(Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
}
impl SseDecoder {
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>> {
        self.buffer.extend_from_slice(chunk);
        ensure!(
            self.buffer.len() <= 4 * 1024 * 1024,
            "SSE buffer limit exceeded"
        );
        let mut out = vec![];
        loop {
            let boundary = self
                .buffer
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|i| (i, 2))
                .into_iter()
                .chain(
                    self.buffer
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|i| (i, 4)),
                )
                .min_by_key(|x| x.0);
            let Some((pos, len)) = boundary else { break };
            let bytes: Vec<_> = self.buffer.drain(..pos + len).collect();
            let text = std::str::from_utf8(&bytes).context("invalid SSE UTF-8")?;
            let mut event = String::new();
            let mut data = vec![];
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim_start_matches(' ').into();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push(v.strip_prefix(' ').unwrap_or(v));
                }
            }
            if !data.is_empty() {
                out.push(SseEvent {
                    event,
                    data: data.join("\n"),
                });
            }
        }
        Ok(out)
    }
}
pub async fn read_sse(response: Response, tx: tokio::sync::mpsc::Sender<SseEvent>) -> Result<()> {
    let mut decoder = SseDecoder::default();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(90), stream.next())
        .await
        .context("SSE idle timeout")?
    {
        for event in decoder.feed(&chunk.context("SSE disconnected")?)? {
            tx.try_send(event)
                .map_err(|_| anyhow::anyhow!("SSE consumer unavailable or overloaded"))?;
        }
    }
    Ok(())
}
