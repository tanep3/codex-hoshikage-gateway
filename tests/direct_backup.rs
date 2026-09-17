mod common;
use codex_hoshikage_gateway::{
    backup,
    codex_execution::TurnIdentity,
    direct_content::DirectContent,
    storage::{self, StateLock, Store},
};

#[tokio::test]
async fn restored_database_can_read_the_same_saved_answer() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    storage::initialize(&cfg).unwrap();
    let lock = StateLock::acquire(&cfg.storage.state_dir).unwrap();
    let (store, done) = Store::open(&cfg).unwrap();
    store
        .add_conversation("4".into(), cfg.projects[0].id.clone())
        .await
        .unwrap();
    let request = common::queued(&store, &cfg, "100").await;
    let workspace = temp.path().join("work2");
    std::fs::create_dir(&workspace).unwrap();
    let intent = store
        .prepare_direct(request.clone(), "4".into(), workspace, "openai".into())
        .await
        .unwrap();
    store
        .begin_direct_send(request.clone(), intent)
        .await
        .unwrap();
    store
        .acknowledge_direct_turn(request.clone(), "thread".into(), "turn".into())
        .await
        .unwrap();
    let content = DirectContent::new(&cfg.storage.state_dir).unwrap();
    let saved = content
        .save_answer(&request, "restorable answer", cfg.limits.output_bytes)
        .unwrap();
    store
        .finish_direct(
            request.clone(),
            TurnIdentity {
                thread_id: "thread".into(),
                turn_id: "turn".into(),
            },
            "completed".into(),
            Some(saved.clone()),
        )
        .await
        .unwrap();
    let bundle = temp.path().join("bundle");
    let manifest = backup::create(&store.path, &bundle).unwrap();
    assert_eq!(manifest.format_version, 2);
    assert_eq!(manifest.content.len(), 1);
    backup::verify(&bundle).unwrap();
    let bundled_answer = bundle.join(&saved.relative_path);
    std::fs::write(&bundled_answer, "tampered answer").unwrap();
    assert!(backup::verify(&bundle).is_err());
    std::fs::write(&bundled_answer, "restorable answer").unwrap();
    backup::verify(&bundle).unwrap();
    drop(store);
    let _ = done.await;
    drop(lock);
    std::fs::remove_file(cfg.storage.state_dir.join(&saved.relative_path)).unwrap();
    let config_dir = temp.path().join("config");
    std::fs::create_dir(&config_dir).unwrap();
    backup::restore(&cfg, &config_dir.join("config.toml"), &bundle).unwrap();
    assert_eq!(
        content
            .read_answer(&saved, cfg.limits.output_bytes)
            .unwrap(),
        "restorable answer"
    );
}
