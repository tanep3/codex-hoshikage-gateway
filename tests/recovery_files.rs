mod common;
use codex_hoshikage_gateway::{
    backup,
    domain::RequestState as S,
    files,
    storage::{self, Store},
};
#[tokio::test]
async fn restored_queued_request_is_never_dispatchable_even_after_release() {
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    storage::initialize(&cfg).unwrap();
    let (store, done) = Store::open(&cfg).unwrap();
    store
        .add_conversation("4".into(), cfg.projects[0].id.clone())
        .await
        .unwrap();
    let id = common::queued(&store, &cfg, "10").await;
    let bundle = t.path().join("snapshot");
    backup::create(&store.path, &bundle).unwrap();
    store.begin_send(id.clone()).await.unwrap();
    drop(store);
    let _ = done.await;
    let config_path = t.path().join("config.toml");
    let rid = backup::restore(&cfg, &config_path, &bundle).unwrap();
    let mut current = cfg.clone();
    let mut added = current.projects[0].clone();
    added.id = "00000000-0000-4000-8000-000000000002".into();
    added.channel_id = "5".into();
    added.cwd = t.path().join("later-project");
    std::fs::create_dir(&added.cwd).unwrap();
    current.projects.push(added);
    let (store, _) = Store::open_recovery(&current).unwrap();
    backup::register_recovery_projects(&store, &current)
        .await
        .unwrap();
    backup::quarantine(&store, rid.clone()).await.unwrap();
    store
        .call(true, |c| {
            let n: i64 = c.query_row("SELECT count(*) FROM restore_blocks", [], |r| r.get(0))?;
            assert_eq!(n, 2);
            Ok(())
        })
        .await
        .unwrap();
    let r = store.request(&id).await.unwrap();
    assert_eq!(r.state, S::Unknown);
    assert!(!r.dispatch_eligible);
    assert!(store.candidates().await.unwrap().is_empty());
    backup::release(
        &store,
        backup::marker(&config_path).unwrap(),
        rid,
        "test explicit recovery".into(),
    )
    .await
    .unwrap();
    assert!(store.begin_send(id).await.is_err());
    assert!(
        store
            .reserve("11".into(), "4".into(), "meta".into(), cfg.limits.clone())
            .await
            .is_err()
    );
    assert!(store.conversation("4").await.unwrap().paused);
}
#[test]
fn artifact_rejects_symlinks_hardlinks_special_files_and_escape() {
    let t = tempfile::tempdir().unwrap();
    let cfg = common::config(&t);
    let mut workspace = cfg.validate().unwrap().remove(0);
    use std::os::unix::fs::MetadataExt;
    workspace.path = workspace.project.cwd.canonicalize().unwrap();
    let md = workspace.path.metadata().unwrap();
    workspace.dev = md.dev();
    workspace.ino = md.ino();
    let root = &workspace.path;
    std::fs::write(root.join("ok.txt"), b"report").unwrap();
    assert_eq!(
        files::artifact(&workspace, "ok.txt", 100).unwrap(),
        b"report"
    );
    std::os::unix::fs::symlink("ok.txt", root.join("link")).unwrap();
    assert!(files::artifact(&workspace, "link", 100).is_err());
    assert!(files::artifact(&workspace, "../outside", 100).is_err());
    assert!(files::artifact(&workspace, "/etc/passwd", 100).is_err());
    assert!(files::artifact(&workspace, "ok.txt", 3).is_err());
    std::fs::hard_link(root.join("ok.txt"), root.join("hard")).unwrap();
    assert!(files::artifact(&workspace, "ok.txt", 100).is_err());
    let fifo = std::ffi::CString::new(root.join("pipe").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(files::artifact(&workspace, "pipe", 100).is_err());
}
#[test]
fn legacy_cwd_is_not_validated_by_gateway() {
    let t = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&t);
    let mut nested = cfg.projects[0].clone();
    nested.id = "00000000-0000-4000-8000-000000000002".into();
    nested.channel_id = "5".into();
    nested.cwd = nested.cwd.join("sub");
    std::fs::create_dir(&nested.cwd).unwrap();
    cfg.projects.push(nested);
    assert!(cfg.validate().is_ok());
}

#[test]
fn orphan_cleanup_removes_only_gateway_owned_regular_cache_files() {
    use std::os::unix::fs::symlink;
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("cache");
    std::fs::create_dir(&dir).unwrap();
    let owned = dir.join(format!("delivery-{}", uuid::Uuid::new_v4()));
    std::fs::write(&owned, "cache").unwrap();
    let source = t.path().join("keep");
    std::fs::write(&source, "user file").unwrap();
    let link = dir.join(format!("delivery-{}", uuid::Uuid::new_v4()));
    symlink(&source, &link).unwrap();
    std::fs::write(dir.join("notes.txt"), "keep").unwrap();
    assert_eq!(
        codex_hoshikage_gateway::resources::clean_orphan_cache(&dir).unwrap(),
        1
    );
    assert!(!owned.exists());
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert!(source.exists());
    assert!(dir.join("notes.txt").exists());
}
