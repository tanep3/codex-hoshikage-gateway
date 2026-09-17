//! Gateway-owned Codex App Server process and JSON-RPC transport.
//!
//! Protocol framing follows the independently licensed Hoshikage Proxy
//! `src/runtime.rs` and `src/runtime/wire.rs`. Policy, HTTP, Discord, and
//! workspace semantics deliberately do not live in this module.
mod pool;
mod wire;
pub use pool::{CodexRuntimePool, RuntimeLease};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex as StdMutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, broadcast, oneshot},
};

#[derive(Debug, Clone)]
pub struct LaunchConfig {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub codex_home: PathBuf,
    pub initialize_timeout: Duration,
    pub request_timeout: Duration,
    pub experimental_api: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The JSON-RPC request was definitely not written to the child.
    NotSent(String),
    /// Bytes may have reached the child. The caller must reconcile, never retry blindly.
    ResultUnknown(String),
    Remote {
        code: i64,
        message: String,
    },
    Protocol(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSent(s) => write!(f, "not sent: {s}"),
            Self::ResultUnknown(s) => write!(f, "result unknown: {s}"),
            Self::Remote { code, message } => write!(f, "Codex error {code}: {message}"),
            Self::Protocol(s) => write!(f, "Codex protocol error: {s}"),
        }
    }
}

impl std::error::Error for TransportError {}

#[derive(Debug, Clone)]
pub enum Event {
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
    InvalidServerRequest {
        id: Value,
        reason: String,
    },
    ProtocolError(String),
    Closed(String),
}

#[derive(Serialize)]
struct Request<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Deserialize)]
struct Envelope<'a> {
    id: Option<Value>,
    method: Option<String>,
    #[serde(borrow)]
    params: Option<&'a serde_json::value::RawValue>,
    #[serde(borrow, default, deserialize_with = "present_result")]
    result: Option<&'a serde_json::value::RawValue>,
    error: Option<RpcError>,
}

fn present_result<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<&'de serde_json::value::RawValue>, D::Error> {
    <&serde_json::value::RawValue>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

struct PendingRequest {
    sender: oneshot::Sender<Result<Value, TransportError>>,
    catalog: bool,
}
type Pending = Arc<StdMutex<HashMap<u64, PendingRequest>>>;

struct PendingGuard {
    pending: Pending,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}

struct Inner {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Pending,
    next_id: AtomicU64,
    closed: AtomicBool,
    events: broadcast::Sender<Event>,
    request_timeout: Duration,
    pid: u32,
}

#[derive(Clone)]
pub struct CodexTransport(Arc<Inner>);

impl CodexTransport {
    pub async fn launch(config: &LaunchConfig) -> Result<Self, TransportError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .env("CODEX_HOME", &config.codex_home)
            .current_dir(&config.codex_home)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command
            .spawn()
            .map_err(|e| TransportError::NotSent(format!("spawn: {e}")))?;
        let pid = child
            .id()
            .ok_or_else(|| TransportError::NotSent("missing child PID".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| TransportError::NotSent("missing stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TransportError::NotSent("missing stdout".into()))?;
        let (events, _) = broadcast::channel(512);
        let inner = Arc::new(Inner {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            pending: Arc::new(StdMutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            events,
            request_timeout: config.request_timeout,
            pid,
        });
        Self::spawn_reader(stdout, Arc::downgrade(&inner));
        Self::spawn_monitor(Arc::downgrade(&inner));
        let transport = Self(inner);
        let params = json!({
            "clientInfo": {
                "name": "codex-hoshikage-gateway",
                "title": "Codex Hoshikage Gateway",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {"experimentalApi": config.experimental_api}
        });
        let initialized = tokio::time::timeout(
            config.initialize_timeout,
            transport.request("initialize", params),
        )
        .await;
        match initialized {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                let _ = transport.shutdown().await;
                return Err(e);
            }
            Err(_) => {
                let _ = transport.shutdown().await;
                return Err(TransportError::ResultUnknown("initialize timed out".into()));
            }
        }
        transport.notify("initialized", json!({})).await?;
        Ok(transport)
    }

    pub fn pid(&self) -> u32 {
        self.0.pid
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }
    pub fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Acquire)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, TransportError> {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let bytes = serde_json::to_vec(&Request {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })
        .map_err(|e| TransportError::NotSent(e.to_string()))?;
        let (receiver, _guard) = {
            let mut stdin = self.0.stdin.lock().await;
            if self.is_closed() {
                return Err(TransportError::NotSent("transport closed".into()));
            }
            let (sender, receiver) = oneshot::channel();
            self.0.pending.lock().unwrap().insert(
                id,
                PendingRequest {
                    sender,
                    catalog: method == "mcpServerStatus/list",
                },
            );
            let guard = PendingGuard {
                pending: Arc::clone(&self.0.pending),
                id,
            };
            let mut write_guard = WriteFence {
                inner: &self.0,
                finished: false,
            };
            let write = async {
                stdin.write_all(&bytes).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await
            }
            .await;
            if let Err(e) = write {
                return Err(TransportError::ResultUnknown(format!(
                    "write {method}: {e}"
                )));
            }
            write_guard.finished = true;
            (receiver, guard)
        };
        match tokio::time::timeout(self.0.request_timeout, receiver).await {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => Err(TransportError::ResultUnknown(format!(
                "response channel closed: {method}"
            ))),
            Err(_) => Err(TransportError::ResultUnknown(format!(
                "request timed out: {method}"
            ))),
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), TransportError> {
        self.write_response(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<(), TransportError> {
        self.write_response(json!({"jsonrpc":"2.0","id":id,"result":result}))
            .await
    }

    pub async fn reject(&self, id: Value, code: i64, message: &str) -> Result<(), TransportError> {
        self.write_response(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
        )
        .await
    }

    async fn write_response(&self, value: Value) -> Result<(), TransportError> {
        let bytes =
            serde_json::to_vec(&value).map_err(|e| TransportError::NotSent(e.to_string()))?;
        let mut stdin = self.0.stdin.lock().await;
        if self.is_closed() {
            return Err(TransportError::NotSent("transport closed".into()));
        }
        // A cancelled or partial write must fence the transport. It might already
        // have reached Codex, so the owner must reconcile the operation.
        let mut write_guard = WriteFence {
            inner: &self.0,
            finished: false,
        };
        stdin
            .write_all(&bytes)
            .await
            .map_err(|e| TransportError::ResultUnknown(e.to_string()))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| TransportError::ResultUnknown(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| TransportError::ResultUnknown(e.to_string()))?;
        write_guard.finished = true;
        Ok(())
    }

    fn spawn_reader(stdout: ChildStdout, owner: Weak<Inner>) {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                let line = match wire::frame(&mut reader, wire::MAX_FRAME_BYTES).await {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        close_owner(&owner, "stdout EOF".into());
                        break;
                    }
                    Err(e) => {
                        close_owner(&owner, format!("stdout: {e}"));
                        break;
                    }
                };
                let Some(inner) = owner.upgrade() else { break };
                dispatch(&inner, &line);
            }
        });
    }

    fn spawn_monitor(owner: Weak<Inner>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let Some(inner) = owner.upgrade() else { break };
                if inner.closed.load(Ordering::Acquire) {
                    break;
                }
                let status = inner.child.lock().await.try_wait();
                match status {
                    Ok(Some(status)) => {
                        close(&inner, format!("child exited: {status}"));
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        close(&inner, format!("child wait failed: {e}"));
                        break;
                    }
                }
            }
        });
    }

    fn close(&self, reason: String) {
        close(&self.0, reason);
    }

    pub async fn shutdown(&self) -> Result<(), TransportError> {
        self.close("shutdown".into());
        let mut child = self.0.child.lock().await;
        if child
            .try_wait()
            .map_err(|e| TransportError::Protocol(e.to_string()))?
            .is_none()
        {
            child
                .kill()
                .await
                .map_err(|e| TransportError::Protocol(e.to_string()))?;
        }
        child
            .wait()
            .await
            .map_err(|e| TransportError::Protocol(e.to_string()))?;
        Ok(())
    }
}

struct WriteFence<'a> {
    inner: &'a Inner,
    finished: bool,
}
impl Drop for WriteFence<'_> {
    fn drop(&mut self) {
        if !self.finished {
            close(self.inner, "write interrupted".into());
        }
    }
}

fn close_owner(owner: &Weak<Inner>, reason: String) {
    if let Some(inner) = owner.upgrade() {
        close(&inner, reason);
    }
}

fn close(inner: &Inner, reason: String) {
    if inner.closed.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = inner.events.send(Event::Closed(reason.clone()));
    for (_, pending) in inner.pending.lock().unwrap().drain() {
        let _ = pending
            .sender
            .send(Err(TransportError::ResultUnknown(reason.clone())));
    }
}

fn dispatch(inner: &Inner, line: &[u8]) {
    let parsed = match serde_json::from_slice::<Envelope>(line) {
        Ok(value) => value,
        Err(e) => {
            let _ = inner.events.send(Event::ProtocolError(e.to_string()));
            return;
        }
    };
    if let Some(method) = parsed.method {
        let params = match parsed.params {
            Some(raw) => match if parsed.id.is_some() || method == "item/started" {
                wire::exact_json(raw.get())
            } else {
                serde_json::from_str(raw.get()).map_err(|_| "notification_invalid")
            } {
                Ok(value) => value,
                Err(e) => {
                    if let Some(id) = parsed.id {
                        let _ = inner.events.send(Event::InvalidServerRequest {
                            id,
                            reason: e.into(),
                        });
                    } else {
                        let _ = inner.events.send(Event::ProtocolError(e.into()));
                    }
                    return;
                }
            },
            None => Value::Null,
        };
        let event = match parsed.id {
            Some(id) => Event::ServerRequest { id, method, params },
            None => Event::Notification { method, params },
        };
        let _ = inner.events.send(event);
        return;
    }
    let Some(id) = parsed.id.and_then(|id| id.as_u64()) else {
        let _ = inner.events.send(Event::ProtocolError(
            "response without numeric request ID".into(),
        ));
        return;
    };
    let Some(pending) = inner.pending.lock().unwrap().remove(&id) else {
        return;
    };
    let result = match (parsed.result, parsed.error) {
        (Some(raw), _) => {
            if pending.catalog {
                wire::catalog(raw.get()).map_err(|e| TransportError::Protocol(e.into()))
            } else {
                serde_json::from_str(raw.get()).map_err(|e| TransportError::Protocol(e.to_string()))
            }
        }
        (_, Some(e)) => Err(TransportError::Remote {
            code: e.code,
            message: e.message,
        }),
        _ => Err(TransportError::Protocol(
            "response missing result and error".into(),
        )),
    };
    let _ = pending.sender.send(result);
}
