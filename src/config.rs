use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub discord: Discord,
    pub proxy: Proxy,
    pub storage: Storage,
    pub limits: Limits,
    pub projects: Vec<Project>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Discord {
    pub guild_id: String,
    pub allowed_user_id: String,
    pub token_file: PathBuf,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Proxy {
    pub base_url: String,
    pub api_key_file: PathBuf,
    pub contract_version: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    pub state_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub socket_path: PathBuf,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub attachments: usize,
    pub attachment_bytes: usize,
    pub input_bytes: usize,
    pub text_bytes: usize,
    pub image_pixels: u64,
    pub artifact_bytes: usize,
    pub temp_bytes: u64,
    pub output_bytes: usize,
    pub output_total_bytes: usize,
    pub delivery_retention_secs: u64,
    #[serde(default = "queue_conversation")]
    pub queue_conversation: usize,
    #[serde(default = "queue_global")]
    pub queue_global: usize,
    #[serde(default = "validation_secs")]
    pub validation_secs: u64,
}
fn queue_conversation() -> usize {
    5
}
fn queue_global() -> usize {
    20
}
fn validation_secs() -> u64 {
    120
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub channel_id: String,
    pub cwd: PathBuf,
    pub default_model: String,
    #[serde(default = "active")]
    pub lifecycle: String,
}
fn active() -> String {
    "ACTIVE".into()
}
#[derive(Clone, Debug)]
pub struct Workspace {
    pub project: Project,
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
}
impl Workspace {
    pub fn verify(&self) -> Result<()> {
        let path = self.project.cwd.canonicalize()?;
        let md = path.metadata()?;
        ensure!(
            path == self.path && md.dev() == self.dev && md.ino() == self.ino,
            "workspace identity changed"
        );
        Ok(())
    }
}
impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path).context("configuration unavailable")?;
        toml::from_str(&s).context("invalid configuration")
    }
    pub fn validate(&self) -> Result<Vec<Workspace>> {
        for id in [&self.discord.guild_id, &self.discord.allowed_user_id] {
            ensure!(
                id.parse::<u64>().ok().is_some_and(|n| n > 0),
                "Discord IDs must be nonzero"
            );
        }
        ensure!(
            self.proxy.contract_version == "1.0",
            "unsupported control contract"
        );
        let url = reqwest::Url::parse(&self.proxy.base_url)?;
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid Proxy URL"
        );
        ensure!(
            url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))),
            "Proxy requires HTTPS or loopback HTTP"
        );
        for p in [
            &self.storage.state_dir,
            &self.storage.temp_dir,
            &self.storage.socket_path,
            &self.discord.token_file,
            &self.proxy.api_key_file,
        ] {
            ensure!(p.is_absolute(), "paths must be absolute");
        }
        let l = &self.limits;
        ensure!(
            l.attachments > 0
                && l.attachment_bytes > 0
                && l.input_bytes > 0
                && l.text_bytes > 0
                && l.image_pixels > 0
                && l.artifact_bytes > 0
                && l.temp_bytes > 0
                && l.output_bytes > 0
                && l.output_total_bytes >= l.output_bytes
                && l.delivery_retention_secs > 0
                && l.queue_conversation > 0
                && l.queue_global >= l.queue_conversation
                && l.validation_secs > 0,
            "resource limits must be explicitly positive"
        );
        ensure!(
            l.temp_bytes >= l.input_bytes as u64 && l.temp_bytes >= l.artifact_bytes as u64,
            "temporary budget must cover one input and one artifact individually"
        );
        let mut ids = HashSet::new();
        let mut channels = HashSet::new();
        let mut workspaces: Vec<Workspace> = vec![];
        for p in &self.projects {
            ensure!(
                uuid::Uuid::parse_str(&p.id).is_ok() && ids.insert(&p.id),
                "project ID must be a unique UUID"
            );
            ensure!(
                !p.name.is_empty() && !p.default_model.is_empty(),
                "missing project name/model"
            );
            ensure!(
                p.channel_id.parse::<u64>().ok().is_some_and(|id| id > 0),
                "invalid channel ID"
            );
            ensure!(
                matches!(p.lifecycle.as_str(), "ACTIVE" | "RETIRED"),
                "invalid project lifecycle"
            );
            if p.lifecycle == "RETIRED" {
                continue;
            }
            ensure!(
                channels.insert(&p.channel_id),
                "duplicate active project channel"
            );
            ensure!(p.cwd.is_absolute(), "cwd must be absolute");
            let path = p.cwd.canonicalize().context("workspace unavailable")?;
            let md = path.metadata()?;
            ensure!(md.is_dir(), "workspace must be directory");
            for w in &workspaces {
                ensure!(
                    !path.starts_with(&w.path) && !w.path.starts_with(&path),
                    "overlapping workspaces"
                );
            }
            workspaces.push(Workspace {
                project: p.clone(),
                path,
                dev: md.dev(),
                ino: md.ino(),
            });
        }
        Ok(workspaces)
    }
    pub fn fixed_digest(&self) -> String {
        crate::domain::digest(
            serde_json::to_vec(&(
                &self.discord.guild_id,
                &self.discord.allowed_user_id,
                &self.proxy.base_url,
                &self.storage.state_dir,
            ))
            .unwrap()
            .as_slice(),
        )
    }
    pub fn check_reload(&self, next: &Self) -> Result<()> {
        ensure!(
            self.projects
                .iter()
                .all(|old| next.projects.iter().any(|new| new.id == old.id)),
            "project removal requires explicit RETIRED entry"
        );
        ensure!(
            self.fixed_digest() == next.fixed_digest(),
            "immutable setting changed"
        );
        ensure!(
            self.discord.token_file == next.discord.token_file && self.storage == next.storage,
            "restart required"
        );
        if self.projects.iter().any(|p| {
            next.projects
                .iter()
                .find(|n| n.id == p.id)
                .is_some_and(|n| n.cwd != p.cwd || n.channel_id != p.channel_id)
        }) {
            bail!("project identity is immutable");
        }
        Ok(())
    }
}
pub fn secret(path: &Path) -> Result<String> {
    let md = std::fs::symlink_metadata(path).context("credential unavailable")?;
    ensure!(
        md.file_type().is_file()
            && md.mode() & 0o077 == 0
            && md.uid() == unsafe { libc::geteuid() },
        "credential must be an owner-only regular file"
    );
    let s = std::fs::read_to_string(path).context("credential unreadable")?;
    let s = s.trim().to_owned();
    ensure!(
        !s.is_empty() && s.len() <= 8192,
        "invalid credential length"
    );
    Ok(s)
}
