mod common;
use codex_hoshikage_gateway::{
    codex_execution::TurnIdentity,
    codex_transport::Event,
    direct_approval::{DirectInteraction, ManualDecision},
};
use serde_json::json;

#[tokio::test]
async fn approval_reply_is_bound_to_the_exact_call_and_fenced_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "910").await;
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
        .acknowledge_direct_turn(request.clone(), "thread-a".into(), "turn-a".into())
        .await
        .unwrap();
    let identity = TurnIdentity {
        thread_id: "thread-a".into(),
        turn_id: "turn-a".into(),
    };
    let event = Event::ServerRequest {
        id: json!("rpc-7"),
        method: "item/commandExecution/requestApproval".into(),
        params: json!({"threadId":"thread-a","turnId":"turn-a","itemId":"item-a",
                      "command":"cat report.txt","availableDecisions":["accept","decline"]}),
    };
    let call = DirectInteraction::from_event(&event, &identity)
        .unwrap()
        .unwrap();
    let id = store
        .record_direct_interaction(request.clone(), call.clone())
        .await
        .unwrap();
    assert_eq!(
        store
            .record_direct_interaction(request.clone(), call.clone())
            .await
            .unwrap(),
        id
    );
    assert!(
        store
            .begin_direct_approval_reply(
                request.clone(),
                id.clone(),
                "wrong".into(),
                ManualDecision::AcceptOnce
            )
            .await
            .is_err()
    );
    assert!(
        store
            .begin_direct_approval_reply(
                "another-request".into(),
                id.clone(),
                call.fingerprint.clone(),
                ManualDecision::AcceptOnce
            )
            .await
            .is_err()
    );
    assert!(
        store
            .begin_direct_approval_reply(
                request.clone(),
                id.clone(),
                call.fingerprint.clone(),
                ManualDecision::Cancel
            )
            .await
            .is_err()
    );
    let rpc = store
        .begin_direct_approval_reply(
            request.clone(),
            id.clone(),
            call.fingerprint.clone(),
            ManualDecision::AcceptOnce,
        )
        .await
        .unwrap();
    assert_eq!(rpc, json!("rpc-7"));
    assert!(
        store
            .begin_direct_approval_reply(
                request,
                id.clone(),
                call.fingerprint,
                ManualDecision::AcceptOnce
            )
            .await
            .is_err()
    );
    assert_eq!(
        store.fence_direct_approvals_after_restart().await.unwrap(),
        1
    );
    assert!(store.mark_direct_approval_sent(id).await.is_err());
}

#[tokio::test]
async fn approval_call_id_reuse_with_different_content_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "911").await;
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
        .acknowledge_direct_turn(request.clone(), "thread-a".into(), "turn-a".into())
        .await
        .unwrap();
    let identity = TurnIdentity {
        thread_id: "thread-a".into(),
        turn_id: "turn-a".into(),
    };
    let event = |command| Event::ServerRequest {
        id: json!("rpc-7"),
        method: "item/commandExecution/requestApproval".into(),
        params: json!({"threadId":"thread-a","turnId":"turn-a","itemId":"item-a",
                      "command":command}),
    };
    let first = DirectInteraction::from_event(&event("cat a"), &identity)
        .unwrap()
        .unwrap();
    let second = DirectInteraction::from_event(&event("cat b"), &identity)
        .unwrap()
        .unwrap();
    store
        .record_direct_interaction(request.clone(), first)
        .await
        .unwrap();
    assert!(
        store
            .record_direct_interaction(request, second)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn structured_execpolicy_offer_does_not_block_plain_accept_reply() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = common::config(&temp);
    let (store, _lock) = common::store(&cfg).await;
    let request = common::queued(&store, &cfg, "912").await;
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
        .acknowledge_direct_turn(request.clone(), "thread-a".into(), "turn-a".into())
        .await
        .unwrap();
    let identity = TurnIdentity {
        thread_id: "thread-a".into(),
        turn_id: "turn-a".into(),
    };
    let event = Event::ServerRequest {
        id: json!("rpc-policy-offer"),
        method: "item/commandExecution/requestApproval".into(),
        params: json!({
            "threadId":"thread-a",
            "turnId":"turn-a",
            "itemId":"item-a",
            "command":"python3 install-skill.py",
            "availableDecisions":[
                "accept",
                {"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["python3","install-skill.py"]}},
                "cancel"
            ]
        }),
    };
    let call = DirectInteraction::from_event(&event, &identity)
        .unwrap()
        .unwrap();
    let id = store
        .record_direct_interaction(request.clone(), call.clone())
        .await
        .unwrap();
    let rpc = store
        .begin_direct_approval_reply(request, id, call.fingerprint, ManualDecision::AcceptOnce)
        .await
        .unwrap();
    assert_eq!(rpc, json!("rpc-policy-offer"));
}
