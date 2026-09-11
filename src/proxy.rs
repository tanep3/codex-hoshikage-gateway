use crate::{domain::RequestState, storage::Store};
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use reqwest::{Client, Response};
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Proxy {
    pub(crate) base: String,
    pub(crate) key: Arc<String>,
    pub(crate) stream: Client,
    pub(crate) control: Client,
    pub(crate) monitor: Client,
    pub gate: Arc<Gate>,
    pub(crate) v2: Arc<crate::proxy_v2::V2State>,
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
            v2: Arc::new(crate::proxy_v2::V2State::default()),
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
        let mut request = client
            .get(format!("{}{}", self.base, path))
            .bearer_auth(self.key.as_str());
        if path.starts_with("/v1/codex/") && self.v2.binding.read().unwrap().is_some() {
            request = self.bound(request)?;
        }
        let result = request.send().await.context("Proxy GET transport error");
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
            let caps = self.get("/v2/codex/capabilities").await?;
            crate::proxy_v2::validate(&caps)?;
            self.bind_v2(&caps).await?;
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
    pub async fn control(&self, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self
            .control
            .post(format!("{}{}", self.base, path))
            .bearer_auth(self.key.as_str());
        if let Some(v) = body {
            req = req.json(&v);
        }
        if self.v2.binding.read().unwrap().is_some() {
            req = self.bound(req)?;
        }
        let response = req.send().await.context("control delivery unknown")?;
        let status = response.status();
        ensure!(status.is_success(), "control rejected: {}", status.as_u16());
        response.json().await.context("control response invalid")
    }
    pub async fn reconcile(&self, store: &Store, id: &str) -> Result<RequestState> {
        self.reconcile_v2(store, id).await
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
