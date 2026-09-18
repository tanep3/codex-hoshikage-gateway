mod common;
use codex_hoshikage_gateway::{
    codex_execution::TurnIdentity, direct_content::DirectContent, domain::RequestState,
    storage::Store,
};

#[test]
fn schema_nine_upgrades_without_reinterpreting_proxy_records() {
    use codex_hoshikage_gateway::storage;
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    storage::initialize(&cfg).unwrap();
    let c = rusqlite::Connection::open(storage::db_path(&cfg)).unwrap();
    c.execute_batch("DROP TABLE runtime_mode;DELETE FROM schema_migrations WHERE version=12;DROP TRIGGER direct_interaction_no_rewind;DROP TABLE direct_interactions;DELETE FROM schema_migrations WHERE version=11;DROP TRIGGER direct_dispatch_no_rewind; DROP TABLE direct_answers; DROP TABLE direct_dispatches; DROP TABLE direct_conversations; DELETE FROM schema_migrations WHERE version=10; UPDATE schema_meta SET schema_version=9;").unwrap();
    drop(c);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let (_store, _) = Store::open(&cfg).unwrap();
    assert_eq!(
        storage::validate_database(&storage::db_path(&cfg))
            .unwrap()
            .0,
        storage::SCHEMA
    );
}

#[tokio::test]
async fn direct_send_commit_survives_restart_without_replay() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "100").await;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let intent = store
        .prepare_direct(
            request.clone(),
            "4".into(),
            workspace.clone(),
            "openai".into(),
        )
        .await
        .unwrap();
    let dispatch = store
        .begin_direct_send(request.clone(), intent.clone())
        .await
        .unwrap();
    assert_eq!(dispatch.workspace_path, workspace);
    assert_eq!(dispatch.codex_thread_id, None);
    assert!(
        store
            .begin_direct_send(request.clone(), intent)
            .await
            .is_err()
    );
    let reopened = Store::open(&cfg).unwrap().0;
    assert_eq!(reopened.fence_direct_after_restart().await.unwrap(), 1);
    assert_eq!(
        reopened.request(&request).await.unwrap().state,
        RequestState::Unknown
    );
    assert!(
        reopened
            .begin_direct_send(request.clone(), dispatch.intent_id)
            .await
            .is_err()
    );
    assert_eq!(reopened.fence_direct_after_restart().await.unwrap(), 0);
}

#[tokio::test]
async fn acknowledgement_is_bound_to_the_original_request_and_thread() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "101").await;
    let workspace = temp.path().join("workspace");
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
        .record_direct_thread(request.clone(), "codex-thread".into())
        .await
        .unwrap();
    assert!(
        store
            .record_direct_thread(request.clone(), "other-thread".into())
            .await
            .is_err()
    );
    store
        .acknowledge_direct_turn(request.clone(), "codex-thread".into(), "codex-turn".into())
        .await
        .unwrap();
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::Running
    );
    assert!(
        store
            .acknowledge_direct_turn(request.clone(), "other-thread".into(), "other-turn".into())
            .await
            .is_err()
    );
    store
        .mark_direct_unknown(request.clone(), "transport_closed".into())
        .await
        .unwrap();
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::Unknown
    );
    assert!(
        store
            .prepare_direct(
                request.clone(),
                "4".into(),
                temp.path().join("workspace"),
                "openai".into()
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn replaced_workspace_after_commit_does_not_requeue_the_request() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "102").await;
    let workspace = temp.path().join("workspace");
    let other = temp.path().join("other");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&other).unwrap();
    let intent = store
        .prepare_direct(
            request.clone(),
            "4".into(),
            workspace.clone(),
            "openai".into(),
        )
        .await
        .unwrap();
    std::fs::rename(&workspace, temp.path().join("old-workspace")).unwrap();
    symlink(&other, &workspace).unwrap();
    assert!(
        store
            .begin_direct_send(request.clone(), intent)
            .await
            .is_err()
    );
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::Sending
    );
    assert_eq!(store.fence_direct_after_restart().await.unwrap(), 1);
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::Unknown
    );
}

#[tokio::test]
async fn completed_turn_requires_durable_answer_and_matching_identity() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "103").await;
    let workspace = temp.path().join("workspace");
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
        .acknowledge_direct_turn(request.clone(), "codex-thread".into(), "codex-turn".into())
        .await
        .unwrap();
    let identity = TurnIdentity {
        thread_id: "codex-thread".into(),
        turn_id: "codex-turn".into(),
    };
    assert!(
        store
            .finish_direct(request.clone(), identity.clone(), "completed".into(), None)
            .await
            .is_err()
    );
    let content = DirectContent::new(&cfg.storage.state_dir).unwrap();
    let saved = content
        .save_answer(&request, "final answer", cfg.limits.output_bytes)
        .unwrap();
    let wrong = TurnIdentity {
        thread_id: "other".into(),
        turn_id: "codex-turn".into(),
    };
    assert!(
        store
            .finish_direct(
                request.clone(),
                wrong,
                "completed".into(),
                Some(saved.clone())
            )
            .await
            .is_err()
    );
    store
        .finish_direct(
            request.clone(),
            identity,
            "completed".into(),
            Some(saved.clone()),
        )
        .await
        .unwrap();
    assert_eq!(
        store.request(&request).await.unwrap().state,
        RequestState::Completed
    );
    assert_eq!(
        content
            .read_answer(&saved, cfg.limits.output_bytes)
            .unwrap(),
        "final answer"
    );
    let reopened = Store::open(&cfg).unwrap().0;
    let from_db = reopened.direct_answer(request).await.unwrap().unwrap();
    assert_eq!(
        content
            .read_answer(&from_db, cfg.limits.output_bytes)
            .unwrap(),
        "final answer"
    );
    let bundle = temp.path().join("backup-bundle");
    let manifest = codex_hoshikage_gateway::backup::create(&store.path, &bundle).unwrap();
    assert_eq!(manifest.content.len(), 1);
    codex_hoshikage_gateway::backup::verify(&bundle).unwrap();
    assert_eq!(
        std::fs::read_to_string(bundle.join(&saved.relative_path)).unwrap(),
        "final answer"
    );
}
