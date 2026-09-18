//! Immutable final-answer bytes owned by Gateway, independent of Discord delivery.
use crate::{domain, storage::private_dir};
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct StoredAnswer {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredImage {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: usize,
}

#[derive(Clone)]
pub struct DirectContent {
    root: PathBuf,
}

impl DirectContent {
    pub fn save_image(
        &self,
        request_id: &str,
        item_id: &str,
        bytes: &[u8],
        max_bytes: usize,
    ) -> Result<StoredImage> {
        let request = uuid::Uuid::parse_str(request_id).context("invalid image request ID")?;
        ensure!(
            !item_id.is_empty() && item_id.len() <= 256,
            "invalid image item ID"
        );
        ensure!(
            bytes.len() <= max_bytes,
            "generated image exceeds storage limit"
        );
        let root = self
            .root
            .parent()
            .context("state directory unavailable")?
            .join("direct-images");
        private_dir(&root)?;
        ensure!(root.canonicalize()? == root, "image store path changed");
        let item_hash = domain::digest(item_id.as_bytes());
        let name = format!("{request}-{item_hash}.png");
        let relative_path = format!("direct-images/{name}");
        let saved = StoredImage {
            relative_path,
            sha256: domain::digest(bytes),
            bytes: bytes.len(),
        };
        let final_path = root.join(name);
        if final_path.exists() {
            ensure!(
                self.read_image(&saved, max_bytes)? == bytes,
                "stored image differs from Codex output"
            );
            return Ok(saved);
        }
        let temporary = root.join(format!(".{request}.{}.tmp", uuid::Uuid::new_v4()));
        let _cleanup = TempFile(temporary.clone());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary, &final_path) {
            Ok(()) => File::open(&root)?.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    self.read_image(&saved, max_bytes)? == bytes,
                    "stored image differs from Codex output"
                );
            }
            Err(error) => return Err(error.into()),
        }
        Ok(saved)
    }

    pub fn read_image(&self, saved: &StoredImage, max_bytes: usize) -> Result<Vec<u8>> {
        let relative = Path::new(&saved.relative_path);
        ensure!(
            relative.components().count() == 2
                && relative.parent() == Some(Path::new("direct-images")),
            "invalid image path"
        );
        let root = self
            .root
            .parent()
            .context("state directory unavailable")?
            .join("direct-images");
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join(relative.file_name().context("image filename missing")?))?;
        let md = file.metadata()?;
        ensure!(
            md.is_file() && md.len() as usize == saved.bytes && saved.bytes <= max_bytes,
            "image size or type changed"
        );
        let mut bytes = Vec::with_capacity(saved.bytes);
        Read::by_ref(&mut file)
            .take((saved.bytes + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() == saved.bytes && domain::digest(&bytes) == saved.sha256,
            "image content changed"
        );
        Ok(bytes)
    }
    pub fn new(state_dir: &Path) -> Result<Self> {
        private_dir(state_dir)?;
        let root = state_dir.join("direct-answers");
        private_dir(&root)?;
        ensure!(root.canonicalize()? == root, "answer store path changed");
        Ok(Self { root })
    }

    pub fn save_answer(
        &self,
        request_id: &str,
        text: &str,
        max_bytes: usize,
    ) -> Result<StoredAnswer> {
        let id = uuid::Uuid::parse_str(request_id).context("invalid request ID")?;
        ensure!(text.len() <= max_bytes, "answer exceeds storage limit");
        let name = format!("{id}.txt");
        let final_path = self.root.join(&name);
        let digest = domain::digest(text.as_bytes());
        let saved = StoredAnswer {
            relative_path: format!("direct-answers/{name}"),
            sha256: digest,
            bytes: text.len(),
        };
        if final_path.exists() {
            ensure!(
                self.read_answer(&saved, max_bytes)? == text,
                "stored answer differs from final Codex answer"
            );
            return Ok(saved);
        }
        let temp = self
            .root
            .join(format!(".{id}.{}.tmp", uuid::Uuid::new_v4()));
        let _cleanup = TempFile(temp.clone());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temp, &final_path) {
            Ok(()) => {
                File::open(&self.root)?.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    self.read_answer(&saved, max_bytes)? == text,
                    "stored answer differs from final Codex answer"
                );
            }
            Err(e) => return Err(e.into()),
        }
        Ok(saved)
    }

    pub fn read_answer(&self, saved: &StoredAnswer, max_bytes: usize) -> Result<String> {
        let path = Path::new(&saved.relative_path);
        ensure!(
            path.components().count() == 2 && path.parent() == Some(Path::new("direct-answers")),
            "invalid answer path"
        );
        let name = path.file_name().context("answer filename missing")?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join(name))?;
        let md = file.metadata()?;
        ensure!(
            md.is_file() && md.len() as usize == saved.bytes && saved.bytes <= max_bytes,
            "answer size or type changed"
        );
        let mut bytes = Vec::with_capacity(saved.bytes);
        Read::by_ref(&mut file)
            .take((saved.bytes + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() == saved.bytes && domain::digest(&bytes) == saved.sha256,
            "answer content changed"
        );
        Ok(String::from_utf8(bytes)?)
    }
}

struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
