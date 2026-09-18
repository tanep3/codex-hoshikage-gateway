mod common;
use codex_hoshikage_gateway::{
    config::ResponseMode,
    direct_config::{Codex, DirectConfig},
};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn direct_cli_never_falls_through_to_proxy_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let legacy = common::config(&temp);
    let home = temp.path().join("codex-home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let command = temp.path().join("codex");
    fs::write(&command, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&legacy.discord.token_file, "test-token").unwrap();
    fs::set_permissions(
        &legacy.discord.token_file,
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut discord = legacy.discord;
    discord.response_mode = ResponseMode::All;
    let config = DirectConfig {
        discord,
        codex: Codex {
            command,
            home,
            model_provider: "openai".into(),
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            network_access: false,
        },
        storage: legacy.storage,
        limits: legacy.limits,
        default_model: "gpt-5.6-luna".into(),
    };
    let path = temp.path().join("direct.toml");
    fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    let binary = env!("CARGO_BIN_EXE_codex-hoshikage-gateway");
    let run = |command: &str| {
        Command::new(binary)
            .arg("--config")
            .arg(&path)
            .arg("direct")
            .arg(command)
            .output()
            .unwrap()
    };
    let check = run("check");
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let init = run("init");
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(!run("init").status.success());
    // The same direct configuration is rejected by the legacy Proxy command.
    assert!(
        !Command::new(binary)
            .arg("--config")
            .arg(&path)
            .arg("check")
            .output()
            .unwrap()
            .status
            .success()
    );
}
