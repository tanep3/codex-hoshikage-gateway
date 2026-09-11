//! API v2 transport. No implicit v1 fallback and no replay of uncertain AI execution.
use crate::{
    domain,
    proxy::{Proxy, field, path_id},
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use reqwest::{Method, RequestBuilder};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::RwLock;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub instance_id: String,
    pub generation: String,
    pub base_url: String,
}
#[derive(Default)]
pub struct V2State {
    pub binding: RwLock<Option<Binding>>,
    pub store: RwLock<Option<Store>>,
}
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub retry: String,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Proxy API error {} ({})", self.status, self.code)
    }
}
impl std::error::Error for ApiError {}

pub fn validate(c: &Value) -> Result<()> {
    ensure!(c["contract_version"] == "2.0", "Proxy API v2 required");
    path_id(&field(c, "instance_id")?)?;
    path_id(&field(c, "recovery_generation")?)?;
    ensure!(c["recovery_state"] == "ready", "Proxy recovery pending");
    for flag in [
        "managed_conversations",
        "durable_execution",
        "stop_by_request",
        "stop_before_acceptance",
        "workspace_selection",
        "artifact_capture",
        "artifact_registration_tool",
        "artifact_listing",
        "retention_leases",
        "response_output_retrieval",
    ] {
        ensure!(
            c["features"][flag] == true,
            "required v2 capability missing: {flag}"
        );
    }
    ensure!(
        c["limits"]["auth_scope"] == "shared_operator",
        "unsupported auth scope"
    );
    ensure!(
        c["limits"]["execution_disconnect_interrupts"] == false,
        "unsupported execution ownership"
    );
    Ok(())
}
impl Proxy {
    pub fn with_store(self, store: Store) -> Self {
        *self.v2.store.write().unwrap() = Some(store);
        self
    }
    pub async fn bind_v2(&self, caps: &Value) -> Result<()> {
        let b = Binding {
            instance_id: field(caps, "instance_id")?,
            generation: field(caps, "recovery_generation")?,
            base_url: self.base.clone(),
        };
        let store = self.v2.store.read().unwrap().clone();
        if let Some(store) = store {
            let current = b.clone();
            let matches = store
                .call(true, move |c| {
                    let tx = c.transaction()?;
                    let old: Option<(String, String, String, bool)> = tx
                        .query_row(
                            "SELECT instance_id,generation,base_url,blocked FROM proxy_binding",
                            [],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .optional()?;
                    let matches = match old {
                        None => {
                            tx.execute(
                                "INSERT INTO proxy_binding VALUES(1,?1,?2,?3,0)",
                                params![current.instance_id, current.generation, current.base_url],
                            )?;
                            true
                        }
                        Some((i, g, u, blocked)) => {
                            !blocked
                                && i == current.instance_id
                                && g == current.generation
                                && u == current.base_url
                        }
                    };
                    if !matches {
                        tx.execute("UPDATE proxy_binding SET blocked=1", [])?;
                    }
                    tx.commit()?;
                    Ok(matches)
                })
                .await?;
            ensure!(
                matches,
                "Proxy identity changed; administrative reconciliation required"
            );
        }
        let mut slot = self.v2.binding.write().unwrap();
        ensure!(
            slot.as_ref().is_none_or(|old| old == &b),
            "Proxy generation changed"
        );
        *slot = Some(b);
        Ok(())
    }
    pub(crate) fn bound(&self, req: RequestBuilder) -> Result<RequestBuilder> {
        let b = self
            .v2
            .binding
            .read()
            .unwrap()
            .clone()
            .context("Proxy binding not verified")?;
        Ok(req
            .header("X-Proxy-Instance-Id", b.instance_id)
            .header("X-Proxy-Recovery-Generation", b.generation))
    }
    pub(crate) fn check_response_binding(&self, response: &reqwest::Response) -> Result<()> {
        let b = self
            .v2
            .binding
            .read()
            .unwrap()
            .clone()
            .context("Proxy binding not verified")?;
        let matches = response
            .headers()
            .get("X-Proxy-Instance-Id")
            .and_then(|v| v.to_str().ok())
            == Some(b.instance_id.as_str())
            && response
                .headers()
                .get("X-Proxy-Recovery-Generation")
                .and_then(|v| v.to_str().ok())
                == Some(b.generation.as_str());
        if !matches {
            self.gate.invalidate();
        }
        ensure!(matches, "Proxy response generation mismatch");
        Ok(())
    }
    pub async fn v2_json(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Value> {
        ensure!(path.starts_with("/v2/codex/"), "invalid v2 endpoint");
        let mut request = self.bound(
            self.control
                .request(method.clone(), format!("{}{}", self.base, path))
                .bearer_auth(self.key.as_str()),
        )?;
        if method == Method::POST {
            request = request.header(
                "Idempotency-Key",
                path_id(key.context("operation key required")?)?,
            );
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request
            .send()
            .await
            .context("Proxy operation result unknown")?;
        self.check_response_binding(&response)?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.context("Proxy JSON read failure")? {
            ensure!(
                bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                "Proxy JSON exceeds limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).context("Proxy JSON invalid")?;
        if !status.is_success() {
            if matches!(status.as_u16(), 401 | 428)
                || matches!(
                    value["error"]["code"].as_str(),
                    Some("instance_mismatch" | "recovery_generation_mismatch" | "recovery_blocked")
                )
            {
                self.gate.invalidate();
            }
            // Only machine tokens are retained; never store/display the remote message.
            let token = |name: &str| {
                value["error"][name]
                    .as_str()
                    .filter(|s| {
                        s.len() <= 80 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    })
                    .unwrap_or("unknown")
                    .to_owned()
            };
            let retry = value["error"]["retry"]["action"]
                .as_str()
                .filter(|s| {
                    matches!(
                        *s,
                        "none"
                            | "poll_operation"
                            | "repeat_same_request"
                            | "new_operation"
                            | "operator_action"
                    )
                })
                .unwrap_or("none")
                .to_owned();
            return Err(ApiError {
                status: status.as_u16(),
                code: token("code"),
                retry,
            }
            .into());
        }
        Ok(value)
    }
    pub async fn operation(&self, key: &str) -> Result<Value> {
        self.v2_json(
            Method::GET,
            &format!("/v2/codex/operations/by-key/{}", path_id(key)?),
            None,
            None,
        )
        .await
    }
    /// Persist side-effect intent before sending; subsequent invocations only poll.
    /// Payloads here must be metadata only. AI input follows the requests table boundary.
    pub async fn metadata_operation(
        &self,
        store: &Store,
        key: &str,
        kind: &str,
        path: &str,
        body: Value,
    ) -> Result<Value> {
        let (k, t, p, j) = (
            key.to_owned(),
            kind.to_owned(),
            path.to_owned(),
            serde_json::to_string(&body)?,
        );
        let send=store.call(true,move|c|{
            let tx=c.transaction()?;
            let old:Option<(String,String,String,String)>=tx.query_row("SELECT kind,target,request_digest,state FROM remote_operations WHERE request_key=?1",[&k],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            if let Some((ot,op,hash,state))=old {ensure!(ot==t&&op==p&&hash==domain::digest(j.as_bytes()),"operation identity conflict");if state=="RETRYABLE" {tx.execute("UPDATE remote_operations SET state='SENDING' WHERE request_key=?1",[&k])?;tx.commit()?;return Ok(true);}return Ok(false);}
            tx.execute("INSERT INTO remote_operations(request_key,kind,target,request_digest,request_json,send_started,state) VALUES(?1,?2,?3,?4,?5,1,'SENDING')",params![k,t,p,domain::digest(j.as_bytes()),j])?;
            tx.commit()?;Ok(true)
        }).await?;
        let value = if send {
            match self
                .v2_json(Method::POST, path, Some(key), Some(&body))
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    if error.downcast_ref::<ApiError>().is_some_and(|e| {
                        e.status == 429
                            && e.code == "capture_capacity_busy"
                            && e.retry == "repeat_same_request"
                    }) {
                        let k = key.to_owned();
                        store.call(true,move|c|{c.execute("UPDATE remote_operations SET state='RETRYABLE' WHERE request_key=?1",[k])?;Ok(())}).await?;
                    }
                    return Err(error);
                }
            }
        } else {
            self.operation(key).await?
        };
        let (k, v) = (key.to_owned(), value.clone());
        store.call(true,move|c|{c.execute("UPDATE remote_operations SET operation_id=coalesce(operation_id,?2),resource_id=coalesce(resource_id,?3),state=?4 WHERE request_key=?1",params![k,v["operation_id"].as_str(),v["resource"]["id"].as_str(),v["state"].as_str().unwrap_or("accepted")])?;Ok(())}).await?;
        Ok(value)
    }
    pub async fn ensure_conversation_v2(&self, store: &Store, thread: &str) -> Result<String> {
        let t = thread.to_owned();
        let (key,body)=store.call(true,move|c|{
            let tx=c.transaction()?;
            let old:Option<(String,String)>=tx.query_row("SELECT request_key,request_json FROM proxy_conversations WHERE thread_id=?1",[&t],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some(old)=old{return Ok(old);}
            let model:String=tx.query_row("SELECT selected_model FROM conversations WHERE thread_id=?1",[&t],|r|r.get(0))?;
            ensure!(!model.is_empty(),"initial model missing");
            let key=format!("conversation-{}",domain::id());let body=serde_json::to_string(&json!({"workspace":{"mode":"automatic"},"model":model}))?;
            tx.execute("INSERT INTO proxy_conversations(thread_id,request_key,request_json) VALUES(?1,?2,?3)",params![t,key,body])?;tx.commit()?;Ok((key,body))
        }).await?;
        let result = self
            .metadata_operation(
                store,
                &key,
                "conversation.create",
                "/v2/codex/conversations",
                serde_json::from_str(&body)?,
            )
            .await?;
        ensure!(
            result["resource"]["type"] == "conversation",
            "conversation operation target mismatch"
        );
        let id = field(&result["resource"], "id")?;
        let record = self
            .v2_json(
                Method::GET,
                &format!("/v2/codex/conversations/{}", path_id(&id)?),
                None,
                None,
            )
            .await?;
        ensure!(
            record["conversation_id"] == id,
            "conversation identity mismatch"
        );
        ensure!(record["state"] == "ready", "Proxy conversation not ready");
        let workspace = field(&record, "workspace_id")?;
        let (t, id2) = (thread.to_owned(), id.clone());
        store.call(true,move|c|{
            let tx=c.transaction()?;
            let old:Option<String>=tx.query_row("SELECT conversation_id FROM proxy_conversations WHERE thread_id=?1",[&t],|r|r.get(0))?;
            ensure!(old.is_none_or(|old|old==id2),"conversation binding changed");
            tx.execute("UPDATE proxy_conversations SET conversation_id=?2,workspace_id=?3,state='READY' WHERE thread_id=?1",params![t,id2,workspace])?;tx.commit()?;Ok(())
        }).await?;
        Ok(id)
    }
    pub async fn start_v2(
        &self,
        r: &crate::domain::Request,
        conversation: &str,
        input: Value,
    ) -> Result<Value> {
        self.v2_json(Method::POST,&format!("/v2/codex/conversations/{}/responses",path_id(conversation)?),r.client_request_id.as_deref(),Some(&json!({"input":input,"model":r.model,"metadata":{"codex.approval_capability":"interactive","codex.auto_approve_workspace":"false"}}))).await
    }
}

impl Proxy {
    pub async fn reconcile_v2(
        &self,
        store: &Store,
        id: &str,
    ) -> Result<crate::domain::RequestState> {
        use crate::domain::RequestState as S;
        let r = store.request(id).await?;
        if r.state.terminal() {
            return Ok(r.state);
        }
        let result=async {
            let t=r.thread_id.clone();
            let (conversation,workspace):(String,String)=store.call(true,move|c|Ok(c.query_row("SELECT conversation_id,workspace_id FROM proxy_conversations WHERE thread_id=?1 AND state='READY'",[t],|r|Ok((r.get(0)?,r.get(1)?)))?)).await?;
            let response=if let Some(id)=r.response_id.clone(){id}else{
                let op=self.operation(r.client_request_id.as_deref().context("request key missing")?).await?;
                ensure!(op["resource"]["type"]=="response","execution operation identity mismatch");
                field(&op["resource"],"id")?
            };
            let v=self.v2_json(Method::GET,&format!("/v2/codex/responses/{}",path_id(&response)?),None,None).await?;
            ensure!(v["response_id"]==response&&v["conversation_id"]==conversation&&v["workspace_id"]==workspace,"response binding mismatch");
            let (rid,resp)=(r.id.clone(),response.clone());
            let turn=v["turn_id"].as_str().map(str::to_owned);
            store.call(true,move|c|{
                let old:Option<String>=c.query_row("SELECT response_id FROM requests WHERE id=?1",[&rid],|r|r.get(0))?;
                ensure!(old.is_none_or(|old|old==resp),"response identity changed");
                c.execute("UPDATE requests SET response_id=?2,turn_id=coalesce(turn_id,?3) WHERE id=?1",params![rid,resp,turn])?;Ok(())
            }).await?;
            let state=match (v["phase"].as_str(),v["execution_status"].as_str()) {
                (Some("cancelled"),Some("not_started"))=>S::Cancelled,
                (Some("rejected"),_)=>S::Failed,
                (_,Some("completed"))=>S::Completed,
                (_,Some("failed"))=>S::Failed,
                (_,Some("interrupted"))=>S::Cancelled,
                (_,Some("in_progress"))=>S::Running,
                (Some("accepted"|"dispatching"),Some("not_started"))=>r.state,
                _=>S::Unknown,
            };
            if let Some(turn)=v["turn_id"].as_str() {
                // v2 response need not expose App Server Thread ID; obtain it from the control record.
                if let Ok(status)=self.get(&format!("/v1/codex/turns/{}/status",path_id(turn)?)).await {
                    ensure!(status["response_id"]==response&&status["turn_id"]==turn,"control target mismatch");
                    store.identify(r.id.clone(),response.clone(),field(&status,"thread_id")?,turn.into()).await?;
                }
            }
            let cv=self.v2_json(Method::GET,&format!("/v2/codex/conversations/{}",path_id(&conversation)?),None,None).await?;
            ensure!(cv["conversation_id"]==conversation&&cv["workspace_id"]==workspace,"conversation lookup mismatch");
            if cv["state"]!="ready" {
                let t=r.thread_id.clone();
                store.call(true,move|c|{c.execute("UPDATE conversations SET continuation='NEW_CONVERSATION_REQUIRED',paused=1 WHERE thread_id=?1",[t])?;Ok(())}).await?;
            }
            Ok::<_,anyhow::Error>((state,cv["state"]=="ready",v["error"]["code"].as_str().map(str::to_owned)))
        }.await;
        let (next, ready, code) = result.unwrap_or((S::Unknown, false, None));
        let reason = match code.as_deref() {
            Some("provider_busy" | "workspace_busy" | "conversation_busy") => "proxy_busy",
            Some("storage_capacity_exceeded") => "proxy_capacity",
            Some("workspace_access_revoked") => "workspace_access_revoked",
            _ => "v2_current_query",
        };
        if next == S::Sending {
            return Ok(next);
        }
        store.observe(r.id.clone(), next, reason, ready).await?;
        Ok(next)
    }
    pub async fn stop_v2(&self, store: &Store, r: &crate::domain::Request) -> Result<Value> {
        let t = r.thread_id.clone();
        let conversation: String = store
            .call(true, move |c| {
                Ok(c.query_row(
                    "SELECT conversation_id FROM proxy_conversations WHERE thread_id=?1",
                    [t],
                    |r| r.get(0),
                )?)
            })
            .await?;
        let key = format!("stop-{}", r.id);
        let value=self.metadata_operation(store,&key,"stop","/v2/codex/stops",json!({"target":{"conversation_id":conversation,"request_key":r.client_request_id.as_deref().context("stop request key missing")?}})).await?;
        let stop = value["stop_id"]
            .as_str()
            .or_else(|| value["resource"]["id"].as_str());
        if let Some(stop) = stop {
            self.v2_json(
                Method::GET,
                &format!("/v2/codex/stops/{}", path_id(stop)?),
                None,
                None,
            )
            .await
        } else {
            Ok(value)
        }
    }
    pub async fn monitor_v2(&self, response: &str) -> Result<reqwest::Response> {
        let r = self
            .bound(
                self.monitor
                    .get(format!(
                        "{}/v2/codex/responses/{}/events",
                        self.base,
                        path_id(response)?
                    ))
                    .bearer_auth(self.key.as_str()),
            )?
            .send()
            .await?;
        self.check_response_binding(&r)?;
        ensure!(r.status().is_success(), "v2 event stream unavailable");
        Ok(r)
    }
}

impl Proxy {
    pub async fn content_v2(
        &self,
        path: &str,
        size: u64,
        hash: &str,
        limit: usize,
    ) -> Result<Vec<u8>> {
        ensure!(
            size <= limit as u64
                && hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "invalid resource size or hash"
        );
        ensure!(path.starts_with("/v2/codex/"), "invalid content path");
        tokio::time::timeout(std::time::Duration::from_secs(300), async {
            let mut bytes = Vec::with_capacity(size as usize);
            let mut etag: Option<String> = None;
            for attempt in 0..4 {
                let offset = bytes.len() as u64;
                let mut request = self.bound(
                    self.stream
                        .get(format!("{}{}", self.base, path))
                        .bearer_auth(self.key.as_str()),
                )?;
                if offset > 0 {
                    request = request
                        .header("Range", format!("bytes={offset}-"))
                        .header("If-Range", etag.as_deref().context("resume ETag missing")?);
                }
                let mut response = match request.send().await {
                    Ok(r) => r,
                    Err(e) => {
                        if attempt == 3 {
                            return Err(e.into());
                        }
                        continue;
                    }
                };
                self.check_response_binding(&response)?;
                if !response.status().is_success() {
                    let status = response.status().as_u16();
                    let code = match status {
                        410 => "content_expired",
                        403 => "access_revoked",
                        429 => "download_capacity_busy",
                        503 => "content_unavailable",
                        _ => "content_unavailable",
                    };
                    return Err(ApiError {
                        status,
                        code: code.into(),
                        retry: "repeat_same_request".into(),
                    }
                    .into());
                }
                let got = response
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                if offset > 0 {
                    ensure!(
                        response.status().as_u16() == 206 && got == etag,
                        "Range identity mismatch"
                    );
                    ensure!(
                        response
                            .headers()
                            .get("content-range")
                            .and_then(|v| v.to_str().ok())
                            == Some(format!("bytes {offset}-{}/{size}", size - 1).as_str()),
                        "Range bounds mismatch"
                    );
                } else {
                    ensure!(
                        response.status().as_u16() == 200,
                        "unexpected resource response"
                    );
                    etag = got;
                }
                ensure!(
                    response.content_length() == Some(size - offset),
                    "resource length mismatch"
                );
                let mut interrupted = false;
                loop {
                    match response.chunk().await {
                        Ok(Some(chunk)) => {
                            ensure!(
                                bytes.len() + chunk.len() <= size as usize,
                                "resource exceeds declared size"
                            );
                            bytes.extend_from_slice(&chunk);
                        }
                        Ok(None) => break,
                        Err(_) => {
                            interrupted = true;
                            break;
                        }
                    }
                }
                if !interrupted && bytes.len() as u64 == size {
                    ensure!(
                        domain::digest(&bytes) == hash,
                        "resource integrity mismatch"
                    );
                    return Ok(bytes);
                }
                ensure!(bytes.len() < size as usize, "resource framing invalid");
                ensure!(
                    bytes.is_empty() || etag.as_ref().is_some_and(|e| !e.starts_with("W/")),
                    "immutable ETag required for resume"
                );
            }
            anyhow::bail!("resource transfer interrupted")
        })
        .await
        .context("resource read timeout")?
    }
}
