//! Automatic conversation workspace below the Gateway state directory.
use crate::storage::private_dir;
use anyhow::{Result, ensure};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub fn ensure_conversation_workspace(state_dir: &Path, discord_thread_id: &str) -> Result<PathBuf> {
    ensure!(
        (1..=20).contains(&discord_thread_id.len())
            && discord_thread_id.bytes().all(|byte| byte.is_ascii_digit())
            && discord_thread_id.parse::<u64>().is_ok_and(|id| id != 0),
        "invalid Discord conversation ID"
    );
    private_dir(state_dir)?;
    let root = state_dir.join("workspaces");
    private_dir(&root)?;
    let workspace = root.join(discord_thread_id);
    private_dir(&workspace)?;
    let canonical = workspace.canonicalize()?;
    ensure!(canonical == workspace, "workspace path changed");
    let md = workspace.metadata()?;
    ensure!(
        md.is_dir() && md.uid() == unsafe { libc::geteuid() },
        "workspace identity invalid"
    );
    Ok(workspace)
}
