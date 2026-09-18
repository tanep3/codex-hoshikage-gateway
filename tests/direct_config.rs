mod common;
use codex_hoshikage_gateway::direct_config::{Codex, DirectConfig};
use std::{fs, os::unix::fs::PermissionsExt};

fn fixture(temp: &tempfile::TempDir) -> DirectConfig {
    let legacy = common::config(temp);
    let home = temp.path().join("private-codex-home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    DirectConfig {
        discord: legacy.discord,
        codex: Codex {
            command: std::env::current_exe().unwrap(),
            home,
            workspace_root: None,
            model_provider: "openai".into(),
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            network_access: false,
        },
        storage: legacy.storage,
        limits: legacy.limits,
        default_model: "gpt-5.6-luna".into(),
    }
}

#[test]
fn direct_configuration_requires_no_proxy_and_launches_stdio_only() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = fixture(&temp);
    let path = temp.path().join("config.toml");
    fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    let loaded = DirectConfig::read(&path).unwrap();
    assert_eq!(loaded.launch().args, ["app-server", "--listen", "stdio://"]);
    assert_eq!(loaded.execution().approval_policy, "on-request");
    assert_eq!(loaded.execution().sandbox, "workspace-write");
}

#[test]
fn direct_configuration_rejects_legacy_proxy_and_public_codex_home() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = fixture(&temp);
    let path = temp.path().join("config.toml");
    let mut raw = toml::to_string(&cfg).unwrap();
    raw.push_str("\n[proxy]\nbase_url='http://127.0.0.1:9876'\n");
    fs::write(&path, raw).unwrap();
    assert!(DirectConfig::read(&path).is_err());
    fs::set_permissions(&cfg.codex.home, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(cfg.validate().is_err());
}

#[test]
fn workspace_root_can_be_configured_outside_private_state() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = fixture(&temp);
    assert_eq!(
        cfg.workspace_root(),
        cfg.storage.state_dir.join("workspaces")
    );
    let visible = temp.path().join("visible-workspaces");
    cfg.codex.workspace_root = Some(visible.clone());
    cfg.validate().unwrap();
    assert_eq!(cfg.workspace_root(), visible);
    cfg.codex.workspace_root = Some(cfg.storage.state_dir.join("nested"));
    assert!(cfg.validate().is_err());
}
