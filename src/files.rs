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
    io::{Cursor, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
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
    pub content_type: Option<String>,
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
    workspace_files: Vec<(PathBuf, Vec<u8>)>,
}
impl PreparedInput {
    /// Publish binary inputs only after the second Discord fetch has matched
    /// the digest recorded at admission.
    pub fn materialize_workspace_files(&self, workspace: &Path) -> Result<()> {
        for (relative, bytes) in &self.workspace_files {
            store_workspace_attachment(workspace, relative, bytes)?;
        }
        Ok(())
    }
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
        let mut workspace_files = vec![];
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
            let (kind, relative) =
                append_attachment_content(&mut content, &m.id, a, &bytes, limits)?;
            let byte_count = bytes.len();
            if let Some(relative) = relative {
                workspace_files.push((relative, bytes));
            }
            records.push((a.id.clone(), byte_count, hash, kind.into()));
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
            workspace_files,
        })
    }
}

fn append_attachment_content(
    content: &mut Vec<Value>,
    message_id: &str,
    attachment: &Attachment,
    bytes: &[u8],
    limits: &Limits,
) -> Result<(&'static str, Option<PathBuf>)> {
    if let Ok(format) = image::guess_format(bytes) {
        ensure!(
            matches!(
                format,
                image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP
            ),
            "unsupported image format"
        );
        // Conservatively reject animation markers; no frame extraction or transcoding.
        ensure!(
            !bytes
                .windows(4)
                .any(|part| part == b"acTL" || part == b"ANIM"),
            "animated image unsupported"
        );
        let (width, height) =
            image::ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()?;
        ensure!(
            u64::from(width) * u64::from(height) <= limits.image_pixels,
            "image pixel limit"
        );
        let mime = match format {
            image::ImageFormat::Png => "image/png",
            image::ImageFormat::Jpeg => "image/jpeg",
            _ => "image/webp",
        };
        content.push(json!({"type":"input_image","image_url":format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes)),"detail":"high"}));
        return Ok(("image", None));
    }
    if is_inline_text(attachment, bytes, limits.text_bytes)
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        content.push(json!({"type":"input_text","text":format!("\n--- 添付 {:?} ---\n{}\n--- 添付終了 ---",attachment.filename,text)}));
        return Ok(("text", None));
    }
    let relative = attachment_relative_path(message_id, attachment)?;
    content.push(json!({
        "type":"input_text",
        "text":format!(
            "\n添付ファイル {:?} は作業フォルダー内の {:?} に保存されています。必要なツールでこのファイルを処理してください。",
            attachment.filename,
            relative.to_string_lossy()
        )
    }));
    Ok(("file", Some(relative)))
}

fn is_inline_text(attachment: &Attachment, bytes: &[u8], limit: usize) -> bool {
    if bytes.len() > limit || std::str::from_utf8(bytes).is_err() || bytes.contains(&0) {
        return false;
    }
    if let Some(content_type) = attachment.content_type.as_deref() {
        let content_type = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim();
        return content_type.starts_with("text/")
            || matches!(
                content_type,
                "application/json"
                    | "application/ld+json"
                    | "application/xml"
                    | "application/javascript"
                    | "application/toml"
                    | "application/yaml"
            );
    }
    matches!(
        Path::new(&attachment.filename)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "txt"
                | "md"
                | "csv"
                | "json"
                | "jsonl"
                | "xml"
                | "toml"
                | "yaml"
                | "yml"
                | "html"
                | "css"
                | "js"
                | "ts"
                | "rs"
                | "py"
                | "sh"
        )
    )
}

fn attachment_relative_path(message_id: &str, attachment: &Attachment) -> Result<PathBuf> {
    for id in [message_id, attachment.id.as_str()] {
        ensure!(
            (1..=20).contains(&id.len())
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && id.parse::<u64>().is_ok_and(|value| value != 0),
            "invalid attachment identity"
        );
    }
    let mut filename = attachment
        .filename
        .chars()
        .take(96)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if filename.is_empty() {
        filename = "attachment".into();
    }
    Ok(PathBuf::from(".hoshikage-inputs")
        .join(message_id)
        .join(format!("{}-{filename}", attachment.id)))
}

/// Store an immutable snapshot below the already-validated conversation
/// workspace. Every traversal is descriptor-relative and refuses symlinks.
fn store_workspace_attachment(workspace: &Path, relative: &Path, bytes: &[u8]) -> Result<()> {
    let canonical = workspace.canonicalize()?;
    ensure!(canonical == workspace, "workspace path changed");
    let metadata = std::fs::symlink_metadata(workspace)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() },
        "workspace identity invalid"
    );
    let parts = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => Ok(CString::new(value.as_bytes())?),
            _ => bail!("invalid attachment path"),
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(parts.len() == 3, "invalid attachment path depth");
    let root = CString::new(workspace.as_os_str().as_bytes())?;
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    ensure!(root_fd >= 0, "workspace open failed");
    let mut directory = unsafe { std::fs::File::from_raw_fd(root_fd) };
    for part in &parts[..2] {
        let made = unsafe { libc::mkdirat(directory.as_raw_fd(), part.as_ptr(), 0o700) };
        if made != 0 {
            let error = std::io::Error::last_os_error();
            ensure!(
                error.raw_os_error() == Some(libc::EEXIST),
                "attachment directory creation failed: {error}"
            );
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                part.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(fd >= 0, "attachment directory is unsafe");
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    let temporary = CString::new(format!(".upload-{}", crate::domain::id()))?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    ensure!(fd >= 0, "attachment snapshot creation failed");
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        ensure!(
            unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    temporary.as_ptr(),
                    directory.as_raw_fd(),
                    parts[2].as_ptr(),
                )
            } == 0,
            "attachment snapshot publication failed"
        );
        directory.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0);
        }
    }
    result
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn attachment(filename: &str) -> Attachment {
        Attachment {
            id: "456".into(),
            filename: filename.into(),
            url: "https://cdn.discordapp.com/attachments/example".into(),
            size: 4,
            content_type: Some("video/mp4".into()),
        }
    }

    #[test]
    fn binary_attachment_path_is_deterministic_and_cannot_traverse() {
        assert_eq!(
            attachment_relative_path("123", &attachment("../movie 1.mp4")).unwrap(),
            PathBuf::from(".hoshikage-inputs/123/456-.._movie_1.mp4")
        );
        assert!(attachment_relative_path("../123", &attachment("movie.mp4")).is_err());
    }

    #[test]
    fn workspace_attachment_is_atomic_and_does_not_follow_destination_symlink() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let relative = Path::new(".hoshikage-inputs/123/456-movie.mp4");
        store_workspace_attachment(&workspace, relative, b"first").unwrap();
        assert_eq!(std::fs::read(workspace.join(relative)).unwrap(), b"first");

        let outside = temporary.path().join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        std::fs::remove_file(workspace.join(relative)).unwrap();
        symlink(&outside, workspace.join(relative)).unwrap();
        store_workspace_attachment(&workspace, relative, b"second").unwrap();
        assert_eq!(std::fs::read(workspace.join(relative)).unwrap(), b"second");
        assert_eq!(std::fs::read(outside).unwrap(), b"outside");
    }

    #[test]
    fn mp4_is_saved_as_a_workspace_file_and_referenced_by_relative_path() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let file = attachment("holiday.mp4");
        let mut bytes = vec![0; 3_744_464];
        bytes[4..12].copy_from_slice(b"ftypmp42");
        let mut content = Vec::new();
        let limits = crate::config::Limits {
            attachments: 4,
            attachment_bytes: 8 * 1024 * 1024,
            input_bytes: 16 * 1024 * 1024,
            text_bytes: 4,
            image_pixels: 20_000_000,
            artifact_bytes: 8 * 1024 * 1024,
            temp_bytes: 32 * 1024 * 1024,
            output_bytes: 1024 * 1024,
            output_total_bytes: 8 * 1024 * 1024,
            delivery_retention_secs: 60,
            queue_conversation: 5,
            queue_global: 20,
            validation_secs: 120,
        };
        let (kind, relative) =
            append_attachment_content(&mut content, "123", &file, &bytes, &limits).unwrap();
        assert_eq!(kind, "file");
        let relative = relative.unwrap();
        store_workspace_attachment(&workspace, &relative, &bytes).unwrap();
        let relative = Path::new(".hoshikage-inputs/123/456-holiday.mp4");
        assert_eq!(std::fs::read(workspace.join(relative)).unwrap(), bytes);
        let prompt = content[0]["text"].as_str().unwrap();
        assert!(prompt.contains("holiday.mp4"));
        assert!(prompt.contains(relative.to_str().unwrap()));
        assert!(!prompt.contains(workspace.to_str().unwrap()));
    }

    #[test]
    fn a_small_pdf_is_a_workspace_file_not_inline_text() {
        let mut file = attachment("report.pdf");
        file.content_type = Some("application/pdf".into());
        let limits = crate::config::Limits {
            attachments: 4,
            attachment_bytes: 1024,
            input_bytes: 4096,
            text_bytes: 1024,
            image_pixels: 1000,
            artifact_bytes: 1024,
            temp_bytes: 4096,
            output_bytes: 1024,
            output_total_bytes: 4096,
            delivery_retention_secs: 60,
            queue_conversation: 5,
            queue_global: 20,
            validation_secs: 120,
        };
        let mut content = Vec::new();
        let (kind, relative) = append_attachment_content(
            &mut content,
            "123",
            &file,
            b"%PDF-1.7 text-looking header",
            &limits,
        )
        .unwrap();
        assert_eq!(kind, "file");
        assert_eq!(
            relative.unwrap(),
            PathBuf::from(".hoshikage-inputs/123/456-report.pdf")
        );
    }
}
