mod common;
use codex_hoshikage_gateway::{
    codex_execution::TurnIdentity, direct_content::DirectContent, domain::RequestState,
    storage::Store,
};

#[tokio::test]
async fn a_second_turn_in_the_same_conversation_cannot_cross_the_send_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let first = common::queued(&store, &cfg, "701").await;
    let second = common::queued(&store, &cfg, "702").await;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let first_intent = store
        .prepare_direct(
            first.clone(),
            "4".into(),
            workspace.clone(),
            "openai".into(),
        )
        .await
        .unwrap();
    let second_intent = store
        .prepare_direct(second.clone(), "4".into(), workspace, "openai".into())
        .await
        .unwrap();
    store
        .begin_direct_send(first.clone(), first_intent)
        .await
        .unwrap();
    assert!(
        store
            .begin_direct_send(second.clone(), second_intent.clone())
            .await
            .is_err()
    );
    store
        .mark_direct_unknown(first, "upstream_unavailable".into())
        .await
        .unwrap();
    assert!(
        store
            .begin_direct_send(second.clone(), second_intent)
            .await
            .is_err()
    );
    assert_eq!(
        store.request(&second).await.unwrap().state,
        RequestState::Queued
    );
}

#[tokio::test]
async fn two_distinct_conversations_can_run_but_a_third_waits() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    for thread in ["5", "6"] {
        store
            .add_conversation(thread.into(), cfg.projects[0].id.clone())
            .await
            .unwrap();
    }
    for (thread, message) in [("4", "801"), ("5", "802"), ("6", "803")] {
        let request = store
            .reserve(
                message.into(),
                thread.into(),
                "meta".into(),
                cfg.limits.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        store
            .finalize(request.clone(), "meta".into(), "input".into(), vec![])
            .await
            .unwrap();
        let workspace = temp.path().join(format!("workspace-{thread}"));
        std::fs::create_dir(&workspace).unwrap();
        let intent = store
            .prepare_direct(request.clone(), thread.into(), workspace, "openai".into())
            .await
            .unwrap();
        if thread == "6" {
            assert!(
                store
                    .begin_direct_send(request.clone(), intent)
                    .await
                    .is_err()
            );
            assert_eq!(
                store.request(&request).await.unwrap().state,
                RequestState::Queued
            );
        } else {
            store.begin_direct_send(request, intent).await.unwrap();
        }
    }
}

#[test]
fn schema_nine_upgrades_without_reinterpreting_proxy_records() {
    use codex_hoshikage_gateway::storage;
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    storage::initialize(&cfg).unwrap();
    let c = rusqlite::Connection::open(storage::db_path(&cfg)).unwrap();
    c.execute_batch("DELETE FROM schema_migrations WHERE version=15;ALTER TABLE operations DROP COLUMN desired_reasoning_effort;ALTER TABLE requests DROP COLUMN reasoning_effort_revision;ALTER TABLE requests DROP COLUMN reasoning_effort;ALTER TABLE conversations DROP COLUMN effort_revision;ALTER TABLE conversations DROP COLUMN effective_reasoning_effort;ALTER TABLE conversations DROP COLUMN selected_reasoning_effort;ALTER TABLE conversations DROP COLUMN latest_effort_sequence;ALTER TABLE projects DROP COLUMN default_reasoning_effort;DROP TABLE direct_artifacts;DELETE FROM schema_migrations WHERE version=14;DROP TABLE direct_generated_images;DROP TABLE direct_image_inventories;DELETE FROM schema_migrations WHERE version=13;DROP TABLE runtime_mode;DELETE FROM schema_migrations WHERE version=12;DROP TRIGGER direct_interaction_no_rewind;DROP TABLE direct_interactions;DELETE FROM schema_migrations WHERE version=11;DROP TRIGGER direct_dispatch_no_rewind; DROP TABLE direct_answers; DROP TABLE direct_dispatches; DROP TABLE direct_conversations; DELETE FROM schema_migrations WHERE version=10; UPDATE schema_meta SET schema_version=9;").unwrap();
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
async fn unknown_hold_requires_explicit_abandon_before_next_direct_turn() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let first = common::queued(&store, &cfg, "102").await;
    let second = common::queued(&store, &cfg, "103").await;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let intent = store
        .prepare_direct(first.clone(), "4".into(), workspace, "openai".into())
        .await
        .unwrap();
    store
        .begin_direct_send(first.clone(), intent)
        .await
        .unwrap();
    store.fence_direct_after_restart().await.unwrap();
    assert!(store.direct_unknown_blocker("4").await.unwrap());
    assert!(
        store
            .candidates()
            .await
            .unwrap()
            .iter()
            .all(|r| r.id != second)
    );
    assert!(
        store
            .abandon_direct_unknown(first.clone(), 1, "operator recovery".into())
            .await
            .is_err()
    );
    assert!(store.direct_unknown_blocker("4").await.unwrap());
    store
        .abandon_direct_unknown(first.clone(), 0, "operator recovery".into())
        .await
        .unwrap();
    assert!(!store.direct_unknown_blocker("4").await.unwrap());
    assert_eq!(
        store.request(&first).await.unwrap().state,
        RequestState::Unknown
    );
    assert!(
        store
            .candidates()
            .await
            .unwrap()
            .iter()
            .any(|r| r.id == second)
    );
    assert!(
        store
            .abandon_direct_unknown(first, 0, "duplicate".into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn user_recovery_keeps_unknown_but_cancels_unsent_work_and_reopens_conversation() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let old = common::queued(&store, &cfg, "1200").await;
    let waiting = common::queued(&store, &cfg, "1201").await;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let intent = store
        .prepare_direct(old.clone(), "4".into(), workspace.clone(), "openai".into())
        .await
        .unwrap();
    store.begin_direct_send(old.clone(), intent).await.unwrap();
    store.fence_direct_after_restart().await.unwrap();
    store.stop("stop-1202".into(), "4".into()).await.unwrap();
    let offer = store.direct_recovery_offer("4").await.unwrap().unwrap();
    assert_eq!(offer.request_id, old);
    assert_eq!(offer.waiting, 1);
    let mut wrong_generation = offer.clone();
    wrong_generation.generation += 1;
    assert!(
        store
            .recover_direct_unknown(
                "5".into(),
                offer.clone(),
                "confirm-wrong-thread".into(),
                "backup-1".into()
            )
            .await
            .is_err()
    );
    assert!(
        store
            .recover_direct_unknown(
                "4".into(),
                wrong_generation,
                "confirm-wrong-generation".into(),
                "backup-1".into()
            )
            .await
            .is_err()
    );
    let extra = common::queued(&store, &cfg, "1205").await;
    assert!(
        store
            .recover_direct_unknown(
                "4".into(),
                offer.clone(),
                "confirm-stale-queue".into(),
                "backup-1".into()
            )
            .await
            .is_err()
    );
    let offer = store.direct_recovery_offer("4").await.unwrap().unwrap();
    assert_eq!(offer.waiting, 2);
    let backup = temp.path().join("before-recovery");
    let manifest = codex_hoshikage_gateway::backup::create(
        &cfg.storage.state_dir.join("gateway.sqlite3"),
        &backup,
    )
    .unwrap();
    assert_eq!(
        codex_hoshikage_gateway::backup::verify(&backup)
            .unwrap()
            .backup_id,
        manifest.backup_id
    );
    assert!(
        store
            .recover_direct_unknown(
                "4".into(),
                offer.clone(),
                "confirm-1203".into(),
                manifest.backup_id.clone()
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .recover_direct_unknown(
                "4".into(),
                offer.clone(),
                "confirm-1203".into(),
                manifest.backup_id.clone()
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .recover_direct_unknown(
                "4".into(),
                offer,
                "confirm-stale".into(),
                manifest.backup_id.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(
        store.request(&old).await.unwrap().state,
        RequestState::Unknown
    );
    assert_eq!(
        store.request(&waiting).await.unwrap().state,
        RequestState::Cancelled
    );
    assert_eq!(
        store.request(&extra).await.unwrap().state,
        RequestState::Cancelled
    );
    assert!(!store.direct_unknown_blocker("4").await.unwrap());
    let cv = store.conversation("4").await.unwrap();
    assert!(!cv.paused);
    assert_eq!(cv.continuation, "NEW");
    let next = common::queued(&store, &cfg, "1204").await;
    let next_intent = store
        .prepare_direct(next.clone(), "4".into(), workspace, "openai".into())
        .await
        .unwrap();
    store.begin_direct_send(next, next_intent).await.unwrap();
}

#[tokio::test]
async fn stop_pauses_an_unknown_run_and_cancel_removes_its_unsent_followup() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let first = common::queued(&store, &cfg, "104").await;
    let second = common::queued(&store, &cfg, "105").await;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let intent = store
        .prepare_direct(first.clone(), "4".into(), workspace, "openai".into())
        .await
        .unwrap();
    store
        .begin_direct_send(first.clone(), intent)
        .await
        .unwrap();
    store.fence_direct_after_restart().await.unwrap();
    let target = store.stop("stop-1".into(), "4".into()).await.unwrap();
    assert_eq!(target.unwrap().id, first);
    assert!(store.conversation("4").await.unwrap().paused);
    assert_eq!(
        store.request(&first).await.unwrap().state,
        RequestState::Unknown
    );
    let (decision, target) = store
        .cancel_latest("cancel-1".into(), "4".into())
        .await
        .unwrap();
    assert_eq!(decision, "waiting");
    assert_eq!(target.as_deref(), Some(second.as_str()));
    assert_eq!(
        store.request(&second).await.unwrap().state,
        RequestState::Cancelled
    );
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
async fn direct_control_is_bound_to_the_exact_turn_and_never_replayed() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "107").await;
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
        .record_direct_thread(request.clone(), "thread-a".into())
        .await
        .unwrap();
    store
        .acknowledge_direct_turn(request.clone(), "thread-a".into(), "turn-a".into())
        .await
        .unwrap();
    let exact = TurnIdentity {
        thread_id: "thread-a".into(),
        turn_id: "turn-a".into(),
    };
    let wrong = TurnIdentity {
        thread_id: "thread-a".into(),
        turn_id: "turn-b".into(),
    };
    assert!(
        store
            .begin_direct_steer(
                "900".into(),
                request.clone(),
                wrong.clone(),
                "digest".into()
            )
            .await
            .is_err()
    );
    store
        .begin_direct_steer(
            "900".into(),
            request.clone(),
            exact.clone(),
            "digest".into(),
        )
        .await
        .unwrap();
    store
        .finish_direct_control("900".into(), false)
        .await
        .unwrap();
    assert!(
        store
            .begin_direct_steer(
                "900".into(),
                request.clone(),
                exact.clone(),
                "digest".into()
            )
            .await
            .is_err()
    );
    let (kind, target) = store.cancel_latest("901".into(), "4".into()).await.unwrap();
    assert_eq!(kind, "active");
    assert_eq!(target.as_deref(), Some(request.as_str()));
    assert!(
        store
            .begin_direct_interrupt("901".into(), request.clone(), wrong)
            .await
            .is_err()
    );
    store
        .begin_direct_interrupt("901".into(), request.clone(), exact.clone())
        .await
        .unwrap();
    store
        .finish_direct_control("901".into(), true)
        .await
        .unwrap();
    assert!(
        store
            .begin_direct_interrupt("901".into(), request, exact)
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
    let conversation = store.conversation("4").await.unwrap();
    assert_eq!(conversation.continuation, "READY");
    assert_eq!(conversation.effective_model, Some("chatgpt/test".into()));
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
