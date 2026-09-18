//! Configuration for the Gateway-owned Codex runtime. This type deliberately
//! has no Proxy endpoint or API key, and it does not parse legacy settings.
use crate::{
    codex_execution::ExecutionOptions,
    codex_transport::LaunchConfig,
    config::{Discord, Limits, Storage},
    domain,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectConfig {
    pub discord: Discord,
    pub codex: Codex,
    pub storage: Storage,
    pub limits: Limits,
    pub default_model: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Codex {
    pub command: PathBuf,
    pub home: PathBuf,
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
    #[serde(default = "default_provider")]
    pub model_provider: String,
    #[serde(default = "default_sandbox")]
    pub sandbox: String,
    #[serde(default = "default_approval")]
    pub approval_policy: String,
    #[serde(default)]
    pub network_access: bool,
}

fn default_provider() -> String {
    "openai".into()
}
fn default_sandbox() -> String {
    "workspace-write".into()
}
fn default_approval() -> String {
    "on-request".into()
}

impl DirectConfig {
    pub fn fixed_digest(&self) -> String {
        domain::digest(
            serde_json::to_vec(&(
                &self.discord.guild_id,
                &self.discord.allowed_user_id,
                &self.storage.state_dir,
                &self.codex.home,
            ))
            .expect("fixed identity is serializable")
            .as_slice(),
        )
    }
    pub fn workspace_root(&self) -> PathBuf {
        self.codex
            .workspace_root
            .clone()
            .unwrap_or_else(|| self.storage.state_dir.join("workspaces"))
    }
    pub fn read(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).context("Gateway configuration unavailable")?;
        let cfg: Self = toml::from_str(&raw).context("invalid direct Codex configuration")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        for id in [&self.discord.guild_id, &self.discord.allowed_user_id] {
            ensure!(
                id.parse::<u64>().ok().is_some_and(|value| value > 0),
                "Discord IDs must be nonzero"
            );
        }
        for path in [
            &self.discord.token_file,
            &self.codex.command,
            &self.codex.home,
            &self.storage.state_dir,
            &self.storage.temp_dir,
            &self.storage.socket_path,
        ] {
            ensure!(path.is_absolute(), "Gateway paths must be absolute");
        }
        if let Some(root) = &self.codex.workspace_root {
            ensure!(root.is_absolute(), "Codex workspace root must be absolute");
            ensure!(
                !root
                    .components()
                    .any(|part| matches!(part, Component::CurDir | Component::ParentDir)),
                "Codex workspace root must be normalized"
            );
            ensure!(
                !root.starts_with(&self.codex.home)
                    && !root.starts_with(&self.storage.state_dir)
                    && !self.storage.state_dir.starts_with(root),
                "Codex workspace root overlaps private runtime state"
            );
        }
        let executable = fs::metadata(&self.codex.command).context("Codex command unavailable")?;
        ensure!(
            executable.is_file() && executable.mode() & 0o111 != 0,
            "Codex command is not executable"
        );
        let home = fs::symlink_metadata(&self.codex.home).context("Codex home unavailable")?;
        ensure!(
            home.is_dir()
                && !home.file_type().is_symlink()
                && home.uid() == unsafe { libc::geteuid() }
                && home.mode() & 0o077 == 0,
            "Codex home must be a private directory owned by the Gateway user"
        );
        ensure!(
            !self.default_model.trim().is_empty()
                && self.default_model.len() <= 128
                && !self.codex.model_provider.trim().is_empty()
                && self.codex.model_provider.len() <= 128,
            "Codex model or provider is invalid"
        );
        ensure!(
            matches!(self.codex.sandbox.as_str(), "workspace-write" | "read-only"),
            "unsupported Codex sandbox"
        );
        ensure!(
            self.codex.approval_policy == "on-request",
            "interactive Codex approval policy is required"
        );
        let l = &self.limits;
        ensure!(
            l.attachments > 0
                && l.attachment_bytes > 0
                && l.input_bytes > 0
                && l.text_bytes > 0
                && l.image_pixels > 0
                && l.artifact_bytes > 0
                && l.temp_bytes >= l.input_bytes as u64
                && l.temp_bytes >= l.artifact_bytes as u64
                && l.output_bytes > 0
                && l.output_total_bytes >= l.output_bytes
                && l.delivery_retention_secs > 0
                && l.queue_conversation > 0
                && l.queue_global >= l.queue_conversation
                && l.validation_secs > 0,
            "resource limits must be positive and internally consistent"
        );
        Ok(())
    }

    pub fn launch(&self) -> LaunchConfig {
        LaunchConfig {
            command: self.codex.command.clone(),
            args: vec!["app-server".into(), "--listen".into(), "stdio://".into()],
            codex_home: self.codex.home.clone(),
            initialize_timeout: Duration::from_secs(15),
            request_timeout: Duration::from_secs(30),
            experimental_api: true,
        }
    }

    pub fn execution(&self) -> ExecutionOptions {
        ExecutionOptions {
            cwd: PathBuf::new(),
            model: self.default_model.clone(),
            model_provider: self.codex.model_provider.clone(),
            sandbox: self.codex.sandbox.clone(),
            approval_policy: self.codex.approval_policy.clone(),
            network_access: self.codex.network_access,
        }
    }
}
