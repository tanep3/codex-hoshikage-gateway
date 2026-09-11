mod common;
use codex_hoshikage_gateway::{
    backup,
    domain::{Redactor, RequestState as S},
    storage::{self, Store},
};
#[tokio::test]
async fn attachment_reservation_cannot_be_overtaken() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    let (s, _lock) = common::store(&c).await;
    let first = s
        .reserve("10".into(), "4".into(), "meta".into(), c.limits.clone())
        .await
        .unwrap()
        .unwrap();
    let second = common::queued(&s, &c, "11").await;
    assert!(s.candidates().await.unwrap().is_empty());
    assert!(s.begin_send(second.clone()).await.is_err());
    s.reject_admission(first, "validation_failed")
        .await
        .unwrap();
    assert_eq!(s.candidates().await.unwrap()[0].id, second);
}
#[tokio::test]
async fn send_boundary_survives_restart_and_forbids_rewind() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    let (s, _lock) = common::store(&c).await;
    let id = common::queued(&s, &c, "10").await;
    let sent = s.begin_send(id.clone()).await.unwrap();
    assert!(sent.client_request_id.is_some());
    assert!(s.begin_send(id.clone()).await.is_err());
    assert!(
        s.reserve("10".into(), "4".into(), "meta".into(), c.limits.clone())
            .await
            .unwrap()
            .is_none()
    );
    let other = Store::open(&c).unwrap().0;
    assert_eq!(other.request(&id).await.unwrap().state, S::Sending);
    let key = id.clone();
    assert!(
        s.call(true, move |c| {
            c.execute("UPDATE requests SET state='QUEUED' WHERE id=?1", [key])?;
            Ok(())
        })
        .await
        .is_err()
    );
    s.observe(id.clone(), S::Unknown, "offline", false)
        .await
        .unwrap();
    assert!(s.begin_send(id).await.is_err());
}
#[tokio::test]
async fn later_stop_invalidates_old_resume() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    let (s, _lock) = common::store(&c).await;
    s.stop("100".into(), "4".into()).await.unwrap();
    let (op, _) = s
        .reserve_resume("101".into(), "4".into())
        .await
        .unwrap()
        .unwrap();
    s.stop("102".into(), "4".into()).await.unwrap();
    assert!(!s.apply_resume(op).await.unwrap());
    assert!(s.conversation("4").await.unwrap().paused);
    assert!(
        s.reserve_resume("101".into(), "4".into())
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn first_turn_failure_releases_unsubmitted_queue() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    let (s, _lock) = common::store(&c).await;
    let a = common::queued(&s, &c, "10").await;
    let b = common::queued(&s, &c, "11").await;
    s.begin_send(a.clone()).await.unwrap();
    s.observe(a, S::Failed, "rejected", false).await.unwrap();
    assert_eq!(s.request(&b).await.unwrap().state, S::Failed);
    assert!(s.candidates().await.unwrap().is_empty());
    assert!(
        s.reserve("12".into(), "4".into(), "meta".into(), c.limits.clone())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(s.conversation("4").await.unwrap().continuation, "NEW");
}
#[tokio::test]
async fn sweep_rejects_late_worker() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    let (s, _lock) = common::store(&c).await;
    let id = s
        .reserve("10".into(), "4".into(), "meta".into(), c.limits.clone())
        .await
        .unwrap()
        .unwrap();
    let key = id.clone();
    s.call(true, move |c| {
        c.execute(
            "UPDATE admissions SET expires_at=0 WHERE request_id=?1",
            [key],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(s.sweep().await.unwrap(), vec![id.clone()]);
    assert!(
        s.finalize(id, "meta".into(), "input".into(), vec![])
            .await
            .is_err()
    );
}
#[test]
fn redacts_every_split_without_revealing_prefix() {
    for split in 1.."SECRET_12345".len() {
        let mut f = Redactor::new(vec!["SECRET_12345".into()]);
        assert_eq!(f.push(&"SECRET_12345"[..split]), "");
        assert_eq!(f.push(&"SECRET_12345"[split..]), "[非公開]");
        assert_eq!(f.finish(), "");
    }
    let mut f = Redactor::new(vec!["SECRET_12345".into()]);
    assert_eq!(f.push("hello SECR"), "hello ");
    assert_eq!(f.finish(), "[非公開]");
}
#[test]
fn unknown_schema_is_not_modified() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    storage::initialize(&c).unwrap();
    let path = storage::db_path(&c);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE schema_meta SET schema_version=999", [])
        .unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert!(storage::validate_database(&path).is_err());
    assert_eq!(before, std::fs::read(path).unwrap());
}
#[test]
fn backup_manifest_detects_corruption() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    storage::initialize(&c).unwrap();
    let bundle = t.path().join("backup");
    backup::create(&storage::db_path(&c), &bundle).unwrap();
    backup::verify(&bundle).unwrap();
    std::fs::write(bundle.join("gateway.sqlite3"), b"bad").unwrap();
    assert!(backup::verify(&bundle).is_err());
}

#[tokio::test]
async fn stale_stop_button_cannot_pause_or_interrupt_a_later_request() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(&t);
    let (s, _) = common::store(&c).await;
    let first = common::queued(&s, &c, "10").await;
    s.begin_send(first.clone()).await.unwrap();
    s.identify(
        first.clone(),
        "response1".into(),
        "thread1".into(),
        "turn1".into(),
    )
    .await
    .unwrap();
    s.observe(first.clone(), S::Completed, "finished", true)
        .await
        .unwrap();
    let second = common::queued(&s, &c, "11").await;
    s.begin_send(second.clone()).await.unwrap();
    assert!(
        s.stop_target("100".into(), "4".into(), Some(first))
            .await
            .is_err()
    );
    assert!(!s.conversation("4").await.unwrap().paused);
    assert!(!s.request(&second).await.unwrap().stop_requested);
}

#[tokio::test]
async fn released_unknown_stays_monitored_and_late_running_reacquires_hold() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = common::config(&tmp);
    let (s, _lock) = common::store(&cfg).await;
    let id = common::queued(&s, &cfg, "10").await;
    s.begin_send(id.clone()).await.unwrap();
    s.observe(
        id.clone(),
        codex_hoshikage_gateway::domain::RequestState::Unknown,
        "lost",
        false,
    )
    .await
    .unwrap();
    let i = id.clone();
    s.call(true, move |c| {
        c.execute("UPDATE holds SET released=1 WHERE request_id=?1", [&i])?;
        c.execute("UPDATE requests SET dispatch_eligible=0 WHERE id=?1", [i])?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(s.active("4").await.unwrap().is_none());
    assert!(s.pending().await.unwrap().iter().any(|r| r.id == id));
    s.observe(
        id.clone(),
        codex_hoshikage_gateway::domain::RequestState::Running,
        "late_execution",
        false,
    )
    .await
    .unwrap();
    assert_eq!(s.active("4").await.unwrap().unwrap().id, id);
    assert!(!s.request(&id).await.unwrap().dispatch_eligible);
}

#[tokio::test]
async fn existing_discord_place_initializes_missing_proxy_conversation_without_replaying_history() {
    for scenario in ["completed", "unknown", "stop", "existing_proxy"] {
        let t = tempfile::tempdir().unwrap();
        let cfg = common::config(&t);
        let (s, _lock) = common::store(&cfg).await;
        let old = common::queued(&s, &cfg, "100").await;
        s.begin_send(old.clone()).await.unwrap();
        let state = if scenario == "unknown" {
            S::Unknown
        } else {
            S::Completed
        };
        s.observe(old.clone(), state, "test", true).await.unwrap();
        if scenario == "stop" {
            s.stop("stop-1".into(), "4".into()).await.unwrap();
        }
        let proxy_exists = scenario == "existing_proxy";
        s.call(true, move |c| {
            c.execute("UPDATE conversations SET continuation='NEW_CONVERSATION_REQUIRED',paused=1,last_response_id='old-response',proxy_thread_id='old-thread' WHERE thread_id='4'", [])?;
            if proxy_exists {
                c.execute("INSERT INTO proxy_conversations(thread_id,request_key,request_json) VALUES('4','creation-key','{}')", [])?;
            }
            Ok(())
        }).await.unwrap();
        // Duplicate Discord event never triggers initialization or another execution.
        assert!(
            s.reserve("100".into(), "4".into(), "meta".into(), cfg.limits.clone())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            s.conversation("4").await.unwrap().continuation,
            "NEW_CONVERSATION_REQUIRED"
        );
        let new = s
            .reserve("101".into(), "4".into(), "meta".into(), cfg.limits.clone())
            .await;
        if matches!(scenario, "unknown" | "existing_proxy") {
            assert!(new.is_err(), "{scenario}");
            assert_eq!(
                s.conversation("4").await.unwrap().continuation,
                "NEW_CONVERSATION_REQUIRED"
            );
        } else {
            assert!(new.unwrap().is_some());
            let cv = s.conversation("4").await.unwrap();
            assert_eq!(cv.continuation, "NEW");
            assert_eq!(cv.paused, scenario == "stop");
            assert!(cv.last_response_id.is_none());
            assert!(cv.proxy_thread_id.is_none());
        }
        assert_eq!(s.request(&old).await.unwrap().state, state);
    }
}
