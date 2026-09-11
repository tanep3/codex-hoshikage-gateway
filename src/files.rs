use crate::{
    config::{Limits, Workspace},
    domain::digest,
};
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::CString,
    io::{Cursor, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub url: String,
    pub size: u64,
}
#[derive(Clone, Debug)]
pub struct InputMessage {
    pub id: String,
    pub thread_id: String,
    pub user_id: String,
    pub guild_id: String,
    pub content: String,
    pub edited_at: Option<i64>,
    pub attachments: Vec<Attachment>,
}
impl InputMessage {
    pub fn metadata_digest(&self) -> String {
        digest(
            &serde_json::to_vec(&(
                &self.id,
                &self.thread_id,
                &self.user_id,
                &self.guild_id,
                &self.content,
                self.edited_at,
                self.attachments
                    .iter()
                    .map(|a| (&a.id, &a.filename, a.size))
                    .collect::<Vec<_>>(),
            ))
            .unwrap(),
        )
    }
}
pub struct PreparedInput {
    pub input: Value,
    pub digest: String,
    pub metadata_digest: String,
    pub attachments: Vec<(String, usize, String, String)>,
    pub reservation: BudgetLease,
}
#[derive(Clone)]
pub struct Files {
    client: reqwest::Client,
    slots: Arc<tokio::sync::Semaphore>,
    budget: Arc<Budget>,
}
impl Files {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
            budget: Arc::new(Budget {
                used: AtomicU64::new(0),
                notify: tokio::sync::Notify::new(),
            }),
        })
    }
    pub fn used(&self) -> u64 {
        self.budget.used.load(Ordering::SeqCst)
    }
    pub async fn reserve(&self, amount: u64, limit: u64) -> Result<BudgetLease> {
        ensure!(
            amount <= limit,
            "temporary capacity smaller than transfer reservation"
        );
        loop {
            let notified = self.budget.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let allocated = self
                .budget
                .used
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                    used.checked_add(amount).filter(|next| *next <= limit)
                })
                .is_ok();
            if allocated {
                return Ok(BudgetLease {
                    budget: self.budget.clone(),
                    amount,
                });
            }
            notified.await;
        }
    }
    pub async fn prepare(&self, m: &InputMessage, limits: &Limits) -> Result<PreparedInput> {
        let _slot = self.slots.acquire().await?;
        let reservation = self
            .reserve(limits.input_bytes as u64, limits.temp_bytes)
            .await?;
        ensure!(
            m.attachments.len() <= limits.attachments,
            "too many attachments"
        );
        ensure!(
            !m.content.trim().is_empty() || !m.attachments.is_empty(),
            "empty input"
        );
        ensure!(m.content.len() <= limits.text_bytes, "text too large");
        let mut content = vec![json!({"type":"input_text","text":m.content})];
        let mut records = vec![];
        let mut total = m.content.len();
        for a in &m.attachments {
            ensure!(
                a.size <= limits.attachment_bytes as u64,
                "attachment too large"
            );
            let url = reqwest::Url::parse(&a.url)?;
            ensure!(
                url.scheme() == "https"
                    && matches!(
                        url.host_str(),
                        Some("cdn.discordapp.com" | "media.discordapp.net")
                    )
                    && url.username().is_empty()
                    && url.password().is_none(),
                "attachment origin not allowed"
            );
            let response = self
                .client
                .get(url)
                .send()
                .await
                .context("attachment fetch failed")?;
            ensure!(response.status().is_success(), "attachment unavailable");
            let mut stream = response.bytes_stream();
            let mut bytes = vec![];
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("attachment read failure")?;
                ensure!(
                    bytes.len().saturating_add(chunk.len()) <= limits.attachment_bytes,
                    "attachment byte limit"
                );
                bytes.extend_from_slice(&chunk);
            }
            total = total.checked_add(bytes.len()).context("input overflow")?;
            ensure!(
                total <= limits.input_bytes && total as u64 <= limits.temp_bytes,
                "total input limit"
            );
            let hash = digest(&bytes);
            let kind = if let Ok(format) = image::guess_format(&bytes) {
                ensure!(
                    matches!(
                        format,
                        image::ImageFormat::Png
                            | image::ImageFormat::Jpeg
                            | image::ImageFormat::WebP
                    ),
                    "unsupported image format"
                );
                // Conservatively reject animation markers; no frame extraction or transcoding.
                ensure!(
                    !bytes.windows(4).any(|b| b == b"acTL" || b == b"ANIM"),
                    "animated image unsupported"
                );
                let (w, h) = image::ImageReader::with_format(Cursor::new(&bytes), format)
                    .into_dimensions()?;
                ensure!(
                    u64::from(w) * u64::from(h) <= limits.image_pixels,
                    "image pixel limit"
                );
                let mime = match format {
                    image::ImageFormat::Png => "image/png",
                    image::ImageFormat::Jpeg => "image/jpeg",
                    _ => "image/webp",
                };
                content.push(json!({"type":"input_image","image_url":format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(&bytes)),"detail":"high"}));
                "image"
            } else {
                ensure!(
                    bytes.len() <= limits.text_bytes,
                    "text attachment too large"
                );
                ensure!(
                    !bytes.starts_with(b"%PDF-") && !bytes.starts_with(b"PK\x03\x04"),
                    "document archives unsupported"
                );
                let text = std::str::from_utf8(&bytes)
                    .context("attachment is neither supported image nor UTF-8")?;
                ensure!(!text.contains('\0'), "binary attachment unsupported");
                content.push(json!({"type":"input_text","text":format!("\n--- 添付 {:?} ---\n{}\n--- 添付終了 ---",a.filename,text)}));
                "text"
            };
            records.push((a.id.clone(), bytes.len(), hash, kind.into()));
        }
        let input = json!([{"role":"user","content":content}]);
        let encoded = serde_json::to_vec(&input)?;
        ensure!(
            encoded.len() <= limits.input_bytes,
            "encoded request size exceeded"
        );
        let metadata_digest = m.metadata_digest();
        let mut hash_material = metadata_digest.clone().into_bytes();
        hash_material.extend_from_slice(&serde_json::to_vec(&records)?);
        Ok(PreparedInput {
            input,
            digest: digest(&hash_material),
            metadata_digest,
            attachments: records,
            reservation,
        })
    }
}
/// Descriptor-relative traversal; never opens an unvalidated special file for reading.
pub fn artifact(workspace: &Workspace, relative: &str, limit: usize) -> Result<Vec<u8>> {
    workspace.verify()?;
    let path = Path::new(relative);
    ensure!(!path.is_absolute(), "absolute paths prohibited");
    let parts = path
        .components()
        .map(|c| match c {
            Component::Normal(x) => Ok(x.to_owned()),
            _ => Err(anyhow::anyhow!("invalid relative path")),
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!parts.is_empty(), "missing path");
    let root = CString::new(workspace.path.as_os_str().as_bytes())?;
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    ensure!(fd >= 0, "workspace open failed");
    let mut current = unsafe { std::fs::File::from_raw_fd(fd) };
    let md = current.metadata()?;
    ensure!(
        md.dev() == workspace.dev && md.ino() == workspace.ino,
        "workspace changed"
    );
    for (i, part) in parts.iter().enumerate() {
        let name = CString::new(part.as_bytes())?;
        let flags = libc::O_PATH
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if i + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        ensure!(fd >= 0, "artifact path unavailable");
        current = unsafe { std::fs::File::from_raw_fd(fd) };
        let md = current.metadata()?;
        ensure!(
            md.dev() == workspace.dev && !md.file_type().is_symlink(),
            "symlink or mount traversal prohibited"
        );
    }
    let before = current.metadata()?;
    ensure!(
        before.is_file() && before.nlink() == 1 && before.len() <= limit as u64,
        "artifact must be a bounded regular non-hardlinked file"
    );
    let stable = format!("/proc/self/fd/{}", current.as_raw_fd());
    let mut reader = std::fs::File::open(stable)?.take(limit.saturating_add(1) as u64);
    let mut bytes = vec![];
    reader.read_to_end(&mut bytes)?;
    let after = current.metadata()?;
    ensure!(
        bytes.len() <= limit
            && before.len() == after.len()
            && after.nlink() == 1
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "artifact changed during read"
    );
    if bytes.len() != before.len() as usize {
        bail!("artifact incomplete");
    }
    Ok(bytes)
}

struct Budget {
    used: AtomicU64,
    notify: tokio::sync::Notify,
}
pub struct BudgetLease {
    budget: Arc<Budget>,
    amount: u64,
}
impl Drop for BudgetLease {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.amount, Ordering::SeqCst);
        self.budget.notify.notify_waiters();
    }
}
