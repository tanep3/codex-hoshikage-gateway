mod common;
use codex_hoshikage_gateway::{
    admin::{self, Command},
    application::App,
    discord::Discord,
    proxy::Proxy,
};
#[tokio::test]
async fn private_admin_socket_serves_status_and_verified_backup() {
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let (store, _lock) = common::store(&cfg).await;
    let path = t.path().join("config.toml");
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    let app = App::new(
        cfg.clone(),
        store,
        Discord::new("fake".into()).unwrap(),
        Proxy::new("http://127.0.0.1:1".into(), "fake".into()).unwrap(),
    )
    .unwrap();
    let a = app.clone();
    let server = tokio::spawn(async move { admin::serve(a, path).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !cfg.storage.socket_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let status = admin::call(&cfg.storage.socket_path, Command::Status)
        .await
        .unwrap();
    assert_eq!(status["ok"], true);
    assert_eq!(status["result"]["connected"], false);
    let bundle = t.path().join("backup");
    let created = admin::call(
        &cfg.storage.socket_path,
        Command::Backup { to: bundle.clone() },
    )
    .await
    .unwrap();
    assert_eq!(created["ok"], true);
    codex_hoshikage_gateway::backup::verify(&bundle).unwrap();
    assert_eq!(
        admin::call(&cfg.storage.socket_path, Command::Backup { to: bundle })
            .await
            .unwrap()["ok"],
        false
    );
    app.cancel.cancel();
    server.await.unwrap().unwrap();
    assert!(!cfg.storage.socket_path.exists());
}
