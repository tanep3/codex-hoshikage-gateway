mod common;
use std::os::unix::fs::PermissionsExt;
#[test]
fn cli_checks_secrets_initializes_once_and_preserves_existing_database() {
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    for path in [&cfg.discord.token_file, &cfg.proxy.api_key_file] {
        std::fs::write(path, "fake-credential-only-for-local-test").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let path = t.path().join("config.toml");
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    let run = |cmd: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_codex-hoshikage-gateway"))
            .arg("--config")
            .arg(&path)
            .arg(cmd)
            .output()
            .unwrap()
    };
    let checked = run("check");
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(run("init").status.success());
    let db = codex_hoshikage_gateway::storage::db_path(&cfg);
    let before = std::fs::read(&db).unwrap();
    assert!(!run("init").status.success());
    assert_eq!(before, std::fs::read(&db).unwrap());
    std::fs::set_permissions(
        &cfg.proxy.api_key_file,
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let bad = run("check");
    assert!(!bad.status.success());
    assert!(!String::from_utf8_lossy(&bad.stderr).contains("fake-credential"));
}
