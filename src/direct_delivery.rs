//! Delivery of the same saved answer after a Discord failure or restart.
use crate::{
    delivery::{Delivery, chunks, nonce},
    direct_content::{DirectContent, StoredImage},
    direct_image_store::ImageRecordState,
    discord::snowflake,
    domain,
    domain::RequestState,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::json;

#[derive(Default, Debug, PartialEq, Eq)]
pub struct DirectImageDelivery {
    pub delivered: usize,
    pub pending: usize,
    pub failed: usize,
    pub inventory_unknown: bool,
}
struct ImageTarget<'a> {
    request_id: &'a str,
    thread: &'a str,
    guild: &'a str,
    source_message: &'a str,
    ordinal: usize,
    saved: &'a StoredImage,
    content: &'a DirectContent,
    max_bytes: usize,
}
struct ExistingDelivery {
    id: String,
    state: String,
    message: Option<String>,
    confirmed: Option<String>,
    pending: Option<String>,
}
struct ImageExpected<'a> {
    thread: &'a str,
    filename: &'a str,
    caption: &'a str,
    bytes: usize,
    digest: &'a str,
}

impl Delivery {
    /// A bounded restart scan. Pending Discord sends are reconciled by nonce;
    /// absence of a matching receipt never authorizes a second upload.
    pub async fn recover_direct_images(&self, guild: &str, max_bytes: usize) -> Result<usize> {
        let targets = self.store.direct_images_to_deliver(50).await?;
        for (request, thread) in &targets {
            let _ = self.direct_images(request, thread, guild, max_bytes).await;
        }
        Ok(targets.len())
    }
    pub async fn direct_images(
        &self,
        request_id: &str,
        thread: &str,
        guild: &str,
        max_bytes: usize,
    ) -> Result<DirectImageDelivery> {
        let request = self.store.request(request_id).await?;
        ensure!(
            request.thread_id == thread && request.state.terminal(),
            "image destination or execution state mismatch"
        );
        let (state, images) = self
            .store
            .direct_image_inventory(request_id.to_owned())
            .await?
            .context("Codex image inventory not recorded")?;
        let mut result = DirectImageDelivery {
            inventory_unknown: state == "UNKNOWN",
            ..Default::default()
        };
        let content = DirectContent::new(
            self.store
                .path
                .parent()
                .context("database has no state directory")?,
        )?;
        for image in images {
            match image.state {
                ImageRecordState::Failed => result.failed += 1,
                ImageRecordState::Unknown => result.inventory_unknown = true,
                ImageRecordState::Ready(saved) => {
                    if self
                        .direct_image_part(ImageTarget {
                            request_id,
                            thread,
                            guild,
                            source_message: &request.message_id,
                            ordinal: image.ordinal,
                            saved: &saved,
                            content: &content,
                            max_bytes,
                        })
                        .await?
                    {
                        result.delivered += 1;
                    } else {
                        result.pending += 1;
                    }
                }
            }
        }
        Ok(result)
    }

    async fn direct_image_part(&self, target: ImageTarget<'_>) -> Result<bool> {
        let ImageTarget {
            request_id,
            thread,
            guild,
            source_message,
            ordinal,
            saved,
            content,
            max_bytes,
        } = target;
        let filename = format!("generated-image-{}.png", ordinal + 1);
        let caption = format!("生成画像 {}", ordinal + 1);
        let digest = domain::digest(
            serde_json::to_vec(&(
                source_message,
                &filename,
                &caption,
                &saved.sha256,
                saved.bytes,
            ))?
            .as_slice(),
        );
        let request_key = request_id.to_owned();
        let thread_key = thread.to_owned();
        let current=self.store.call(true,move|connection|{
            let row:Option<ExistingDelivery>=connection.query_row(
                "SELECT id,state,message_id,confirmed_digest,pending_digest FROM deliveries WHERE target_id=?1 AND kind='direct-image' AND part=?2",
                params![request_key,ordinal as i64],|row|Ok(ExistingDelivery{id:row.get(0)?,state:row.get(1)?,message:row.get(2)?,confirmed:row.get(3)?,pending:row.get(4)?}),
            ).optional()?;
            if let Some(row)=&row {
                let bound:String=connection.query_row("SELECT thread_id FROM deliveries WHERE id=?1",[&row.id],|r|r.get(0))?;
                ensure!(bound==thread_key,"image delivery destination changed");
            }
            Ok(row)
        }).await?;
        if let Some(ExistingDelivery {
            id,
            state,
            message,
            confirmed,
            pending,
        }) = current
        {
            if state == "CONFIRMED" {
                ensure!(
                    confirmed.as_deref() == Some(digest.as_str()),
                    "delivered image identity changed"
                );
                return Ok(true);
            }
            ensure!(
                state == "POST_PENDING" && pending.as_deref() == Some(digest.as_str()),
                "pending image delivery identity changed"
            );
            return self
                .reconcile_direct_image(
                    &id,
                    message.as_deref(),
                    ImageExpected {
                        thread,
                        filename: &filename,
                        caption: &caption,
                        bytes: saved.bytes,
                        digest: &digest,
                    },
                )
                .await;
        }
        self.discord
            .check_attachment_permissions(thread, guild)
            .await?;
        let bytes = content.read_image(saved, max_bytes)?;
        self.discord
            .check_attachment_permissions(thread, guild)
            .await?;
        let id = domain::id();
        let (req, thread_saved, hash) = (request_id.to_owned(), thread.to_owned(), digest.clone());
        self.store.call(true,move|connection|{
            connection.execute("INSERT INTO deliveries(id,target_id,thread_id,kind,part,state,pending_revision,pending_digest,created_at) VALUES(?1,?2,?3,'direct-image',?4,'POST_PENDING',1,?5,?6)",params![id,req,thread_saved,ordinal as i64,hash,domain::now_ms()])?;
            Ok(())
        }).await?;
        let id=self.store.call(true,{let request=request_id.to_owned();move|connection|Ok(connection.query_row("SELECT id FROM deliveries WHERE target_id=?1 AND kind='direct-image' AND part=?2",params![request,ordinal as i64],|row|row.get::<_,String>(0))?)}).await?;
        let receipt = self
            .discord
            .upload_attachment(
                thread,
                filename.clone(),
                bytes,
                &nonce(&id),
                Some((source_message.into(), caption.clone())),
            )
            .await;
        let Ok(receipt) = receipt else {
            return Ok(false);
        };
        let message = receipt["id"]
            .as_str()
            .context("Discord image receipt missing message ID")?;
        ensure!(
            receipt["channel_id"] == thread
                && receipt["attachments"]
                    .as_array()
                    .is_some_and(|items| items.len() == 1
                        && items[0]["filename"] == filename
                        && items[0]["size"] == saved.bytes as u64),
            "Discord image receipt does not match saved content"
        );
        self.confirm_direct_image(&id, message, &digest).await?;
        Ok(true)
    }

    async fn confirm_direct_image(&self, id: &str, message: &str, digest: &str) -> Result<()> {
        let (id, message, digest) = (id.to_owned(), message.to_owned(), digest.to_owned());
        self.store.call(true,move|connection|{
            ensure!(connection.execute("UPDATE deliveries SET message_id=?2,state='CONFIRMED',confirmed_revision=pending_revision,confirmed_digest=pending_digest,pending_revision=NULL,pending_digest=NULL WHERE id=?1 AND state='POST_PENDING' AND pending_digest=?3",params![id,message,digest])?==1,"image delivery acknowledgement changed");
            Ok(())
        }).await
    }

    async fn reconcile_direct_image(
        &self,
        id: &str,
        message: Option<&str>,
        expected: ImageExpected<'_>,
    ) -> Result<bool> {
        let ImageExpected {
            thread,
            filename,
            caption,
            bytes,
            digest,
        } = expected;
        let found = if let Some(message) = message {
            self.discord
                .get(&format!(
                    "/channels/{}/messages/{}",
                    snowflake(thread)?,
                    snowflake(message)?
                ))
                .await
                .ok()
        } else {
            self.discord
                .get(&format!(
                    "/channels/{}/messages?limit=100",
                    snowflake(thread)?
                ))
                .await
                .ok()
                .and_then(|value| {
                    value.as_array().and_then(|items| {
                        items
                            .iter()
                            .find(|item| item["nonce"].as_str() == Some(&nonce(id)))
                            .cloned()
                    })
                })
        };
        if let Some(value) = found
            && self.discord.owns_message(&value)
            && value["channel_id"] == thread
            && value["nonce"].as_str() == Some(&nonce(id))
            && value["content"] == caption
            && value["attachments"].as_array().is_some_and(|items| {
                items.len() == 1
                    && items[0]["filename"] == filename
                    && items[0]["size"] == bytes as u64
            })
            && let Some(message) = value["id"].as_str()
        {
            self.confirm_direct_image(id, message, digest).await?;
            return Ok(true);
        }
        Ok(false)
    }
    pub async fn direct_answer(
        &self,
        request_id: &str,
        discord_thread_id: &str,
        max_bytes: usize,
    ) -> Result<bool> {
        let request = self.store.request(request_id).await?;
        ensure!(
            request.thread_id == discord_thread_id,
            "answer destination mismatch"
        );
        ensure!(
            request.state == RequestState::Completed,
            "Codex answer is not confirmed complete"
        );
        let saved = self
            .store
            .direct_answer(request_id.to_owned())
            .await?
            .context("confirmed answer has no saved content")?;
        let state_dir = self
            .store
            .path
            .parent()
            .context("database has no state directory")?;
        let content = DirectContent::new(state_dir)?.read_answer(&saved, max_bytes)?;
        let parts = chunks(&content);
        for (index, part) in parts.iter().enumerate() {
            if !self
                .text(
                    request_id,
                    discord_thread_id,
                    "answer",
                    index as i64,
                    part,
                    json!([]),
                )
                .await?
            {
                return Ok(false);
            }
        }
        self.trim_answer(request_id, discord_thread_id, parts.len())
            .await
    }
}
