//! Durable resource deliveries. Proxy owns the immutable bytes; Gateway owns delivery intent.
use crate::{
    application::{App, Output},
    domain,
    proxy::{field, path_id},
};
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest::Method;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};
impl App {
    pub(crate) async fn artifact_command(
        &self,
        iid: &str,
        thread: &str,
        path: Option<&str>,
    ) -> Result<String> {
        let s = self.settings().await;
        let cv = s.proxy.ensure_conversation_v2(&self.store, thread).await?;
        if let Some(path) = path {
            ensure!(
                !path.is_empty()
                    && !path.starts_with('/')
                    && !path.contains('\\')
                    && path
                        .split('/')
                        .all(|s| !s.is_empty() && s != "." && s != "..")
                    && !path.contains('\0'),
                "invalid relative path"
            );
            let key = format!("capture-{iid}");
            let result = s
                .proxy
                .metadata_operation(
                    &self.store,
                    &key,
                    "artifact.create",
                    &format!("/v2/codex/conversations/{}/artifacts", path_id(&cv)?),
                    json!({"path":path}),
                )
                .await?;
            let artifact = field(&result["resource"], "id")?;
            self.queue_resource(iid, thread, "artifact", &artifact)
                .await?;
            return Ok(
                "ファイルの取得を受け付けました。保存版が準備できたら、この会話へ届けます。".into(),
            );
        }
        self.selection_page(iid, thread, "artifact", &cv, None)
            .await
    }

    pub(crate) async fn select_artifact(
        &self,
        iid: &str,
        thread: &str,
        id: &str,
    ) -> Result<String> {
        self.authorized_thread(thread).await?;
        let s = self.settings().await;
        let cv = s.proxy.ensure_conversation_v2(&self.store, thread).await?;
        let meta = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/artifacts/{}", path_id(id)?),
                None,
                None,
            )
            .await?;
        ensure!(
            meta["artifact_id"] == id && meta["conversation_id"] == cv && meta["state"] == "ready",
            "artifact selection unavailable"
        );
        let delivery = self.queue_resource(iid, thread, "artifact", id).await?;
        self.artifact_delivery_status(&delivery).await
    }
    pub(crate) async fn artifact_delivery_status(&self, delivery: &str) -> Result<String> {
        let d = delivery.to_owned();
        let state: String = self
            .store
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT state FROM resource_deliveries WHERE id=?1",
                    [d],
                    |r| r.get(0),
                )?)
            })
            .await?;
        Ok(match state.as_str() {
            "DELIVERED"|"RELEASE_PENDING"=>"この保存版は配信済みです。もう一度添付する場合は /retry で確認して再送できます。",
            "WAITING"|"CACHED"=>"この保存版は配信待ちです。準備できたら、この会話へ届けます。",
            "POST_PENDING"=>"この保存版は送信結果を確認中です。重複を避けるため、別の添付は行いません。再送する場合は /retry で確認してください。",
            _=>"この保存版の配信は完了していません。/status で確認できます。再送は /retry から行えますが、期限切れや権限・容量の制限は再送だけでは解消しません。"
        }.into())
    }
    pub(crate) async fn queue_resource(
        &self,
        id: &str,
        thread: &str,
        kind: &str,
        resource: &str,
    ) -> Result<String> {
        let binding = self
            .settings()
            .await
            .proxy
            .v2
            .binding
            .read()
            .unwrap()
            .clone();
        let (id, t, k, r) = (
            id.to_owned(),
            thread.to_owned(),
            kind.to_owned(),
            resource.to_owned(),
        );
        self.store.call(true,move|c|{
            let tx=c.transaction()?;
            let old:Option<(String,String,String)>=tx.query_row("SELECT thread_id,resource_type,resource_id FROM resource_deliveries WHERE id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some(old)=old {ensure!(old==(t.clone(),k.clone(),r.clone()),"delivery identity conflict");return Ok(id);}
            if k=="artifact" {
                let b=binding.context("Proxy binding missing")?;
                let (delivery,_)=crate::generated_images::claim_artifact(&tx,&b,&id,&t,&r)?;
                tx.commit()?;return Ok(delivery);
            }
            tx.execute("INSERT INTO resource_deliveries(id,thread_id,resource_type,resource_id,request_key,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,t,k,r,format!("lease-{id}"),domain::now_ms()])?;tx.commit()?;Ok(id)
        }).await
    }
    pub async fn resource_loop(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=tick.tick()=>{}}
            if self.recovery.load(std::sync::atomic::Ordering::SeqCst)
                || !self.connected.load(std::sync::atomic::Ordering::SeqCst)
            {
                continue;
            }
            let s = self.settings().await;
            if !s.proxy.gate.is_ready() {
                continue;
            }
            let captures=self.store.call(false,|c|{
                let mut st=c.prepare("SELECT o.request_key,p.thread_id FROM remote_operations o JOIN proxy_conversations p ON o.target='/v2/codex/conversations/'||p.conversation_id||'/artifacts' WHERE o.kind='artifact.create' AND NOT EXISTS(SELECT 1 FROM resource_deliveries d WHERE d.id=substr(o.request_key,9)) LIMIT 20")?;
                Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
            }).await?;
            for (key, thread) in captures {
                let k = key.clone();
                let retry:Option<(String,String)>=self.store.call(false,move|c|Ok(c.query_row("SELECT target,request_json FROM remote_operations WHERE request_key=?1 AND state='RETRYABLE'",[k],|r|Ok((r.get(0)?,r.get(1)?))).optional()?)).await?;
                let op = if let Some((path, body)) = retry {
                    s.proxy
                        .metadata_operation(
                            &self.store,
                            &key,
                            "artifact.create",
                            &path,
                            serde_json::from_str(&body)?,
                        )
                        .await
                } else {
                    s.proxy.operation(&key).await
                };
                if let Ok(op) = op
                    && op["resource"]["type"] == "artifact"
                    && let (Some(id), Some(resource)) =
                        (key.strip_prefix("capture-"), op["resource"]["id"].as_str())
                {
                    self.queue_resource(id, &thread, "artifact", resource)
                        .await?;
                }
            }

            let outputs=self.store.call(false,|c|{
                let mut st=c.prepare("SELECT r.id,r.thread_id,r.response_id FROM requests r JOIN output_state o ON o.request_id=r.id JOIN proxy_conversations cv ON cv.thread_id=r.thread_id WHERE r.state='COMPLETED' AND o.state!='DELIVERED' AND r.response_id IS NOT NULL ORDER BY r.updated_at LIMIT 20")?;
                Ok(st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
            }).await?;
            for (id, t, r) in outputs {
                self.queue_resource(&id, &t, "response_output", &r).await?;
            }
            let rows=self.store.call(false,|c|{
                let mut st=c.prepare("SELECT id,thread_id,resource_type,resource_id FROM resource_deliveries WHERE state IN ('WAITING','CACHED','POST_PENDING','RELEASE_PENDING') AND next_attempt_at<=?1 ORDER BY next_attempt_at,created_at LIMIT 20")?;
                Ok(st.query_map([domain::now_ms()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
            }).await?;
            stream::iter(rows).map(|(id,t,k,r)|async move {
                let _guard = self.resource_mutation.read().await;
                if let Err(error) = self.deliver_resource(&id, &t, &k, &r).await {
                    let code = error
                        .downcast_ref::<crate::proxy_v2::ApiError>()
                        .map(|e| e.code.clone())
                        .unwrap_or_else(|| "resource_unavailable".into());
                    let terminal = match code.as_str() {
                        "resource_expired" | "content_expired" | "lease_expired" => Some("EXPIRED"),
                        "resource_corrupt" | "content_corrupt" | "resource_failed"
                        | "output_unavailable" => Some("FAILED"),
                        "workspace_access_revoked" | "access_revoked" | "discord_permission_denied" => Some("BLOCKED"),
                        "delivery_size_limit" | "generated_image_invalid" => Some("FAILED"),
                        _ => None,
                    };
                    if matches!(code.as_str(),"discord_attachment_too_large"|"discord_attachment_rejected"|"discord_permission_denied") {
                        let i=id.clone();let c=code.clone();self.store.call(true,move|db|{db.execute("UPDATE resource_deliveries SET state='FAILED',error_code=?2 WHERE id=?1 AND state='POST_PENDING'",params![i,c])?;Ok(())}).await?;
                    }
                    let (i, code2) = (id.clone(), code.clone());
                    self.store
                        .call(false, move |c| {
                            c.execute(
                                "UPDATE resource_deliveries SET error_code=?2,state=CASE WHEN state IN ('WAITING','CACHED') THEN coalesce(?4,state) ELSE state END, attempts=min(attempts+1,10),next_attempt_at=?3+min(300000,2000*(1 << min(attempts,7))) WHERE id=?1",
                                params![i, code2,domain::now_ms(),terminal],
                            )?;
                            Ok(())
                        })
                        .await?;
                    let i=id.clone();
                    let image_request:Option<String>=self.store.call(false,move|c|Ok(c.query_row("SELECT image_request_id FROM resource_deliveries WHERE id=?1",[i],|r|r.get(0))?)).await?;
                    if let Some(request)=image_request {self.image_progress(&request).await?;} else {
                        let message = resource_error_message(&code);
                        self.notice(format!("resource-error-{id}"), t, message).await?;
                    }
                } else {
                    let request:Option<String>=self.store.call(false,move|c|{c.execute("UPDATE resource_deliveries SET next_attempt_at=?2,attempts=0 WHERE id=?1",params![id,domain::now_ms()+2000])?;Ok(c.query_row("SELECT image_request_id FROM resource_deliveries WHERE id=?1",[id],|r|r.get(0))?)}).await?;
                    if let Some(request)=request {self.image_progress(&request).await?;}
                }
                Ok::<_,anyhow::Error>(())
            }).buffer_unordered(4).try_collect::<Vec<_>>().await?;
        }
    }

    async fn deliver_resource(
        &self,
        id: &str,
        thread: &str,
        kind: &str,
        resource: &str,
    ) -> Result<()> {
        self.authorized_thread(thread).await?;
        let s = self.settings().await;
        let i = id.to_owned();
        let (state, lease): (String, Option<String>) = self
            .store
            .call(true, move |c| {
                Ok(c.query_row(
                    "SELECT state,lease_id FROM resource_deliveries WHERE id=?1",
                    [i],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await?;
        if !matches!(
            state.as_str(),
            "WAITING" | "CACHED" | "POST_PENDING" | "RELEASE_PENDING"
        ) {
            return Ok(());
        }
        if state == "RELEASE_PENDING" {
            if let Some(lease) = lease {
                s.proxy
                    .metadata_operation(
                        &self.store,
                        &format!("release-{id}"),
                        "lease.release",
                        &format!("/v2/codex/leases/{}/release", path_id(&lease)?),
                        json!({}),
                    )
                    .await?;
                let current = s
                    .proxy
                    .v2_json(
                        Method::GET,
                        &format!("/v2/codex/leases/{}", path_id(&lease)?),
                        None,
                        None,
                    )
                    .await?;
                ensure!(
                    current["lease_id"] == lease
                        && current["resource"]["id"] == resource
                        && current["resource"]["type"] == kind,
                    "release lease mismatch"
                );
                if !matches!(current["state"].as_str(), Some("released" | "expired")) {
                    return Ok(());
                }
            }
            let i = id.to_owned();
            self.store
                .call(true, move |c| {
                    c.execute(
                        "UPDATE resource_deliveries SET state='DELIVERED' WHERE id=?1",
                        [i],
                    )?;
                    Ok(())
                })
                .await?;
            return Ok(());
        }
        if state == "POST_PENDING" {
            let i = id.to_owned();
            let (name, size): (String, i64) = self
                .store
                .call(true, move |c| {
                    Ok(c.query_row(
                        "SELECT display_name,size_bytes FROM resource_deliveries WHERE id=?1",
                        [i],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?)
                })
                .await?;
            let list = self
                .discord
                .get(&format!(
                    "/channels/{}/messages?limit=100",
                    crate::discord::snowflake(thread)?
                ))
                .await?;
            let nonce = crate::delivery::nonce(id);
            if let Some(v) = list.as_array().into_iter().flatten().find(|v| {
                v["nonce"].as_str() == Some(&nonce)
                    && self.discord.owns_message(v)
                    && v["channel_id"] == thread
                    && v["attachments"].as_array().is_some_and(|a| {
                        a.iter().any(|v| v["filename"] == name && v["size"] == size)
                    })
            }) {
                let (i, mid) = (id.to_owned(), field(v, "id")?);
                self.store.call(true,move|c|{c.execute("UPDATE resource_deliveries SET state='RELEASE_PENDING',message_id=?2 WHERE id=?1",params![i,mid])?;Ok(())}).await?;
            }
            return Ok(());
        }
        let t = thread.to_owned();
        let cv: String = self
            .store
            .call(true, move |c| {
                Ok(c.query_row(
                    "SELECT conversation_id FROM proxy_conversations WHERE thread_id=?1",
                    [t],
                    |r| r.get(0),
                )?)
            })
            .await?;
        let info_path = if kind == "artifact" {
            format!("/v2/codex/artifacts/{}", path_id(resource)?)
        } else {
            format!("/v2/codex/responses/{}", path_id(resource)?)
        };
        let info = s.proxy.v2_json(Method::GET, &info_path, None, None).await?;
        let delivery_id = id.to_owned();
        let image:Option<(String,String,String,String,i64)>=self.store.call(false,move|c|Ok(c.query_row("SELECT w.request_id,w.response_id,w.workspace_id,r.message_id,d.image_ordinal FROM resource_deliveries d JOIN generated_image_watches w ON w.request_id=d.image_request_id JOIN requests r ON r.id=w.request_id WHERE d.id=?1",[delivery_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?)).await?;
        if let Some((_, response, ws, _, _)) = &image {
            self.discord
                .check_attachment_permissions(thread, &s.cfg.discord.guild_id)
                .await?;
            ensure!(
                info["response_id"] == *response
                    && info["workspace_id"] == *ws
                    && info["media_type"] == "image/png",
                "generated image metadata mismatch"
            );
        }
        let rid = id.to_owned();
        let shared: Option<String> = self
            .store
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT shared_workspace FROM resource_deliveries WHERE id=?1",
                    [rid],
                    |r| r.get(0),
                )?)
            })
            .await?;
        let scope_matches = if let Some(ws) = shared {
            let t = thread.to_owned();
            let current: String = self
                .store
                .call(false, move |c| {
                    Ok(c.query_row(
                        "SELECT workspace_id FROM proxy_conversations WHERE thread_id=?1",
                        [t],
                        |r| r.get(0),
                    )?)
                })
                .await?;
            kind == "artifact" && ws == current && info["workspace_id"] == ws
        } else {
            info["conversation_id"] == cv
        };
        ensure!(
            scope_matches
                && info[if kind == "artifact" {
                    "artifact_id"
                } else {
                    "response_id"
                }] == resource,
            "resource conversation mismatch"
        );
        let meta = if kind == "artifact" {
            &info
        } else {
            &info["output"]
        };
        if matches!(
            meta["state"].as_str(),
            Some("creating" | "pending" | "saving")
        ) {
            return Ok(());
        }
        if meta["state"] != "ready" {
            let code = match meta["state"].as_str() {
                Some("expired") => "resource_expired",
                Some("corrupt") => "resource_corrupt",
                Some("failed") => "resource_failed",
                Some("unavailable") => "output_unavailable",
                _ => "resource_unavailable",
            };
            return Err(crate::proxy_v2::ApiError {
                status: 409,
                code: code.into(),
                retry: "none".into(),
            }
            .into());
        }
        let size = meta["size_bytes"]
            .as_u64()
            .context("resource size missing")?;
        let hash = field(meta, "sha256")?;
        let pinned_id = id.to_owned();
        let pinned: (Option<String>, Option<i64>) = self
            .store
            .call(false, move |c| {
                Ok(c.query_row(
                    "SELECT sha256,size_bytes FROM resource_deliveries WHERE id=?1",
                    [pinned_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await?;
        ensure!(
            pinned.0.is_none_or(|h| h == hash)
                && pinned.1.is_none_or(|n| n >= 0 && n as u64 == size),
            "saved resource identity changed"
        );
        let expires = field(meta, "expires_at")?;
        let key = format!("lease-{id}");
        let lease_id = if let Some(lease) = lease {
            lease
        } else {
            let original_key = key.clone();
            let original: Option<String> = self
                .store
                .call(false, move |c| {
                    Ok(c.query_row(
                        "SELECT request_json FROM remote_operations WHERE request_key=?1",
                        [original_key],
                        |r| r.get(0),
                    )
                    .optional()?)
                })
                .await?;
            let body = match original {
                Some(body) => serde_json::from_str(&body)?,
                None => json!({"resource":{"type":kind,"id":resource},"hold_until":expires}),
            };
            let lease = s
                .proxy
                .metadata_operation(&self.store, &key, "lease", "/v2/codex/leases", body)
                .await?;
            lease["lease_id"]
                .as_str()
                .or_else(|| lease["resource"]["id"].as_str())
                .context("lease identity missing")?
                .to_owned()
        };
        let current = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/leases/{}", path_id(&lease_id)?),
                None,
                None,
            )
            .await?;
        if current["state"] != "active" {
            return Err(crate::proxy_v2::ApiError {
                status: 410,
                code: "lease_expired".into(),
                retry: "none".into(),
            }
            .into());
        }
        ensure!(
            current["resource"]["type"] == kind && current["resource"]["id"] == resource,
            "lease mismatch or expired"
        );
        let (i, h, l, e) = (
            id.to_owned(),
            hash.clone(),
            lease_id.clone(),
            field(&current, "hold_until")?,
        );
        self.store.call(true,move|c|{c.execute("UPDATE resource_deliveries SET lease_id=?2,hold_until=?3,sha256=?4,size_bytes=?5 WHERE id=?1",params![i,l,e,h,i64::try_from(size)?])?;Ok(())}).await?;
        if kind == "response_output" && self.output.lock().await.get(id).is_some_and(|o| o.done) {
            return Ok(());
        }
        let limit = if kind == "artifact" {
            s.cfg.limits.artifact_bytes
        } else {
            s.cfg.limits.output_bytes
        };
        // One attachment per message; retain a conservative Bot upload ceiling.
        let limit = if kind == "artifact" {
            limit.min(20 * 1024 * 1024)
        } else {
            limit
        };
        if size > limit as u64 {
            return Err(crate::proxy_v2::ApiError {
                status: 413,
                code: "delivery_size_limit".into(),
                retry: "none".into(),
            }
            .into());
        }
        if let Some((request, _, _, _, ordinal)) = &image {
            // Lease is already acquired. Bound ordering wait independently from discovery.
            let (r, ord) = (request.clone(), *ordinal);
            let waiting:bool=self.store.call(false,move|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM generated_image_items earlier LEFT JOIN resource_deliveries d ON d.id=earlier.delivery_id WHERE earlier.request_id=?1 AND earlier.ordinal<?2 AND (earlier.state='creating' OR (earlier.state='ready' AND d.state IN ('WAITING','CACHED'))) AND ?3 < (SELECT first_seen_ms+30000 FROM generated_image_items WHERE request_id=?1 AND ordinal=?2))",params![r,ord,domain::now_ms()],|r|r.get(0))?)).await?;
            if waiting {
                return Ok(());
            }
        }
        let _reservation = tokio::time::timeout(
            Duration::from_secs(5),
            self.files.reserve(size, s.cfg.limits.temp_bytes),
        )
        .await??;
        let path = if kind == "artifact" {
            format!("{info_path}/content")
        } else {
            format!("{info_path}/output")
        };
        let bytes = s.proxy.content_v2(&path, size, &hash, limit).await?;
        if kind == "response_output" {
            let value: Value = serde_json::from_slice(&bytes)?;
            ensure!(value["response_id"] == resource, "output identity mismatch");
            let text = value["output"]
                .as_array()
                .context("output array missing")?
                .iter()
                .flat_map(|item| item["content"].as_array().into_iter().flatten())
                .filter(|v| v["type"] == "output_text")
                .filter_map(|v| v["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let text = self.redact(&s, &text);
            let mut cache = self.output.lock().await;
            ensure!(
                cache
                    .iter()
                    .filter(|(key, _)| key.as_str() != id)
                    .map(|(_, o)| o.text.len())
                    .sum::<usize>()
                    + text.len()
                    <= s.cfg.limits.output_total_bytes,
                "output cache full"
            );
            cache.insert(
                id.into(),
                Output {
                    thread: thread.into(),
                    text,
                    done: true,
                    lost: false,
                    created: Instant::now(),
                    last_progress: Instant::now(),
                    retention: Duration::from_secs(s.cfg.limits.delivery_retention_secs),
                },
            );
            return Ok(());
        }
        let filename = field(&info, "display_name")?;
        ensure!(
            !filename.is_empty()
                && filename.len() <= 255
                && !filename
                    .chars()
                    .any(|c| c.is_control() || c == '/' || c == '\\'),
            "unsafe display filename"
        );
        if image.is_some()
            && crate::generated_images::validate_png(&bytes, s.cfg.limits.image_pixels).is_err()
        {
            return Err(crate::proxy_v2::ApiError {
                status: 415,
                code: "generated_image_invalid".into(),
                retry: "none".into(),
            }
            .into());
        }
        crate::storage::private_dir(&s.cfg.storage.temp_dir)?;
        let cache_path = s
            .cfg
            .storage
            .temp_dir
            .join(format!("delivery-{}", domain::id()));
        let _cache = CacheFile(cache_path.clone());
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&cache_path)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        drop(f);
        self.authorized_thread(thread).await?;
        let image_caption = if let Some((request, response, _, message, ordinal)) = &image {
            self.discord
                .check_attachment_permissions(thread, &s.cfg.discord.guild_id)
                .await?;
            let current = s
                .proxy
                .v2_json(
                    Method::GET,
                    &format!("/v2/codex/responses/{}", path_id(response)?),
                    None,
                    None,
                )
                .await?;
            ensure!(
                current["response_id"] == *response && current["conversation_id"] == cv,
                "image response changed"
            );
            let state = self.store.request(request).await?.state;
            let partial = matches!(
                current["execution_status"].as_str(),
                Some("failed" | "interrupted")
            ) || matches!(
                state,
                crate::domain::RequestState::Failed | crate::domain::RequestState::Cancelled
            );
            Some((
                message.clone(),
                format!(
                    "{}画像{}",
                    if partial { "作業途中の" } else { "" },
                    ordinal + 1
                ),
            ))
        } else {
            None
        };
        let i = id.to_owned();
        let name = filename.clone();
        let send=self.store.call(true,move|c|Ok(c.execute("UPDATE resource_deliveries SET state='POST_PENDING',display_name=?2 WHERE id=?1 AND state IN ('WAITING','CACHED')",params![i,name])?==1)).await?;
        let nonce = crate::delivery::nonce(id);
        let receipt = if send {
            self.discord
                .upload_attachment(thread, filename.clone(), bytes, &nonce, image_caption)
                .await?
        } else {
            let list = self
                .discord
                .get(&format!(
                    "/channels/{}/messages?limit=100",
                    crate::discord::snowflake(thread)?
                ))
                .await?;
            match list
                .as_array()
                .into_iter()
                .flatten()
                .find(|v| v["nonce"].as_str() == Some(&nonce) && self.discord.owns_message(v))
            {
                Some(v) => v.clone(),
                None => return Ok(()),
            }
        };
        ensure!(
            receipt["channel_id"] == thread
                && receipt["attachments"].as_array().is_some_and(|a| a
                    .iter()
                    .any(|v| v["filename"] == filename && v["size"] == size)),
            "artifact delivery receipt mismatch"
        );
        let (i, mid) = (id.to_owned(), field(&receipt, "id")?);
        self.store.call(true,move|c|{c.execute("UPDATE resource_deliveries SET state='RELEASE_PENDING',message_id=?2 WHERE id=?1",params![i,mid])?;Ok(())}).await?;
        let _ = s
            .proxy
            .metadata_operation(
                &self.store,
                &format!("release-{id}"),
                "lease.release",
                &format!("/v2/codex/leases/{}/release", path_id(&lease_id)?),
                json!({}),
            )
            .await;
        Ok(())
    }
}
struct CacheFile(std::path::PathBuf);
impl Drop for CacheFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn resource_error_message(code: &str) -> &'static str {
    match code {
        "delivery_size_limit" | "discord_attachment_too_large" => {
            "画像・ファイルが送信容量の上限を超えています。保存版はProxyの保持期限内に取得可能ですが、/getでも同じ送信制限が適用されます。"
        }
        "generated_image_invalid" => {
            "画像を安全に表示できる形式・画素数として検証できなかったため、添付しませんでした。AIは再実行していません。"
        }
        "discord_permission_denied" => {
            "Discordへの添付権限がありません。Botの送信・ファイル添付権限を確認してください。"
        }
        "discord_attachment_rejected" => {
            "Discordが添付を受け付けませんでした。再送前にファイルと送信先を確認してください。"
        }
        "content_expired" | "resource_expired" | "artifact_expired" | "output_expired"
        | "lease_expired" => {
            "保存期限が切れたため、この保存版は取得できません。元ファイルが残っている場合は /get の path 欄から新しい保存版を作れます。AIは再実行していません。"
        }
        "content_corrupt" | "resource_corrupt" | "artifact_corrupt" | "output_corrupt" => {
            "保存データの破損を検出しました。このデータは送信していません。Proxyの保存領域を確認してください。AIは再実行していません。"
        }
        "workspace_access_revoked" | "access_revoked" => {
            "作業先へのアクセスが許可されていないため、取得できません。Proxyの権限設定を確認してください。"
        }
        "storage_capacity_exceeded" | "storage_full" | "capacity_exceeded" | "retention_limit" => {
            "保存容量または保持期間の制限に達しています。Proxyの容量・保持設定を確認してください。AIは再実行していません。"
        }
        "output_unavailable" | "resource_failed" => {
            "実行結果とは別に、回答または成果物の保存に失敗しています。/status で実行状態を確認できます。AIは再実行していません。"
        }
        _ => {
            "ファイルまたは回答の取得をまだ確認できません。接続とProxyの状態を確認しながら、同じ保存版を照会します。AIは再実行していません。"
        }
    }
}

/// Called once after acquiring the instance lock, before starting any transfer worker.
pub fn clean_orphan_cache(dir: &std::path::Path) -> Result<usize> {
    use std::os::unix::fs::MetadataExt;
    crate::storage::private_dir(dir)?;
    let mut removed = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(id) = name.strip_prefix("delivery-") else {
            continue;
        };
        if uuid::Uuid::parse_str(id).is_err() {
            continue;
        }
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
        {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}
