use codex_hoshikage_gateway::{
    codex_execution::TurnIdentity,
    codex_transport::Event,
    direct_approval::{DirectInteraction, InteractionKind, ManualDecision},
};
use serde_json::json;

fn active() -> TurnIdentity {
    TurnIdentity {
        thread_id: "thread-a".into(),
        turn_id: "turn-a".into(),
    }
}

fn request(method: &str, params: serde_json::Value) -> Event {
    Event::ServerRequest {
        id: json!("upstream-id"),
        method: method.into(),
        params,
    }
}

#[test]
fn command_approval_preserves_exact_identity_and_only_single_decisions() {
    let event = request(
        "item/commandExecution/requestApproval",
        json!({"threadId":"thread-a","turnId":"turn-a","itemId":"item-a",
               "command":"cat report.txt","availableDecisions":["accept","decline"]}),
    );
    let approval = DirectInteraction::from_event(&event, &active())
        .unwrap()
        .unwrap();
    assert_eq!(approval.kind, InteractionKind::CommandApproval);
    assert_eq!(approval.rpc_id, json!("upstream-id"));
    assert_eq!(approval.item_id.as_deref(), Some("item-a"));
    assert_eq!(approval.params["command"], "cat report.txt");
    assert_eq!(
        approval
            .manual_decision(ManualDecision::AcceptOnce)
            .unwrap(),
        json!({"decision":"accept"})
    );
    assert!(approval.manual_decision(ManualDecision::Cancel).is_err());
    assert_eq!(
        DirectInteraction::from_event(&event, &active())
            .unwrap()
            .unwrap()
            .fingerprint,
        approval.fingerprint
    );
}

#[test]
fn another_turn_and_missing_item_are_not_approved() {
    let wrong = request(
        "item/fileChange/requestApproval",
        json!({"threadId":"thread-a","turnId":"turn-b","itemId":"item-a"}),
    );
    assert!(DirectInteraction::from_event(&wrong, &active()).is_err());
    let no_item = request(
        "item/fileChange/requestApproval",
        json!({"threadId":"thread-a","turnId":"turn-a"}),
    );
    assert!(DirectInteraction::from_event(&no_item, &active()).is_err());
}

#[test]
fn mcp_elicitation_without_turn_is_bound_to_this_runs_thread() {
    let event = request(
        "mcpServer/elicitation/request",
        json!({"threadId":"thread-a","serverName":"playwright","mode":"form"}),
    );
    let prompt = DirectInteraction::from_event(&event, &active())
        .unwrap()
        .unwrap();
    assert_eq!(prompt.kind, InteractionKind::McpElicitation);
    assert_eq!(prompt.turn_id, "turn-a");
    assert!(prompt.manual_decision(ManualDecision::AcceptOnce).is_err());
    let other = request(
        "mcpServer/elicitation/request",
        json!({"threadId":"thread-b","serverName":"playwright","mode":"form"}),
    );
    assert!(DirectInteraction::from_event(&other, &active()).is_err());
}
