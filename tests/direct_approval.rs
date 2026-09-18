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

#[test]
fn mcp_tool_elicitation_accepts_only_the_exact_empty_form_once() {
    let params = json!({
        "threadId":"thread-a","turnId":"turn-a","serverName":"playwright",
        "mode":"form","message":"Allow the playwright MCP server to run tool \"browser_run_code_unsafe\"?",
        "requestedSchema":{"type":"object","properties":{}},
        "_meta":{"codex_approval_kind":"mcp_tool_call","persist":["session","always"],
            "tool_params":{"code":"async (page) => await page.title()"}}
    });
    let prompt = DirectInteraction::from_event(
        &request("mcpServer/elicitation/request", params.clone()),
        &active(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(prompt.kind, InteractionKind::McpElicitation);
    assert_eq!(
        prompt
            .mcp_tool_decision(ManualDecision::AcceptOnce)
            .unwrap(),
        json!({"action":"accept","content":{}})
    );
    assert_eq!(
        prompt.mcp_tool_decision(ManualDecision::Decline).unwrap(),
        json!({"action":"decline","content":null})
    );
    assert!(prompt.manual_decision(ManualDecision::AcceptOnce).is_err());

    let mut missing_arguments = params.clone();
    missing_arguments["_meta"]
        .as_object_mut()
        .unwrap()
        .remove("tool_params");
    let incomplete = DirectInteraction::from_event(
        &request("mcpServer/elicitation/request", missing_arguments),
        &active(),
    )
    .unwrap()
    .unwrap();
    assert!(
        incomplete
            .mcp_tool_decision(ManualDecision::AcceptOnce)
            .is_err()
    );
    assert_eq!(
        incomplete
            .mcp_tool_decision(ManualDecision::Decline)
            .unwrap(),
        json!({"action":"decline","content":null})
    );
    let mut nonempty_schema = params;
    nonempty_schema["requestedSchema"]["properties"]["secret"] = json!({"type":"string"});
    let incompatible = DirectInteraction::from_event(
        &request("mcpServer/elicitation/request", nonempty_schema),
        &active(),
    )
    .unwrap()
    .unwrap();
    assert!(
        incompatible
            .mcp_tool_decision(ManualDecision::AcceptOnce)
            .is_err()
    );
    assert_eq!(
        incompatible
            .mcp_tool_decision(ManualDecision::Decline)
            .unwrap(),
        json!({"action":"decline","content":null})
    );
}

#[test]
fn dynamic_tool_uses_call_id_from_the_installed_app_server_schema() {
    let event = request(
        "item/tool/call",
        json!({"threadId":"thread-a","turnId":"turn-a","callId":"call-a",
               "namespace":"browser","tool":"browser_tabs","arguments":{"action":"list"}}),
    );
    let operation = DirectInteraction::from_event(&event, &active())
        .unwrap()
        .unwrap();
    assert_eq!(operation.kind, InteractionKind::DynamicTool);
    assert_eq!(operation.item_id.as_deref(), Some("call-a"));
    assert!(
        operation
            .manual_decision(ManualDecision::AcceptOnce)
            .is_err()
    );
    let missing_call = request(
        "item/tool/call",
        json!({"threadId":"thread-a","turnId":"turn-a","tool":"browser_tabs","arguments":{}}),
    );
    assert!(DirectInteraction::from_event(&missing_call, &active()).is_err());
}

#[test]
fn mcp_tool_confirmation_uses_exact_question_and_never_command_decision() {
    let event = request(
        "item/tool/requestUserInput",
        json!({
            "threadId":"thread-a","turnId":"turn-a","itemId":"item-a",
            "questions":[{"id":"mcp_tool_call_approval_item-a","question":"Allow playwright browser_tabs?",
                "isOther":false,"isSecret":false,
                "options":[{"label":"Allow"},{"label":"Cancel"}]}]
        }),
    );
    let mut prompt = DirectInteraction::from_event(&event, &active())
        .unwrap()
        .unwrap();
    prompt.bind_mcp_evidence(json!({"type":"mcpToolCall","id":"item-a","server":"playwright","tool":"browser_tabs","arguments":{"action":"list"}})).unwrap();
    assert_eq!(prompt.kind, InteractionKind::UserInput);
    assert_eq!(
        prompt
            .mcp_tool_decision(ManualDecision::AcceptOnce)
            .unwrap(),
        json!({"answers":{"mcp_tool_call_approval_item-a":{"answers":["Allow"]}}})
    );
    assert_eq!(
        prompt.mcp_tool_decision(ManualDecision::Decline).unwrap(),
        json!({"answers":{"mcp_tool_call_approval_item-a":{"answers":["Cancel"]}}})
    );
    assert!(prompt.manual_decision(ManualDecision::AcceptOnce).is_err());
    let secret = request(
        "item/tool/requestUserInput",
        json!({
            "threadId":"thread-a","turnId":"turn-a","itemId":"item-a",
            "questions":[{"id":"mcp_tool_call_approval_item-a","isOther":false,"isSecret":true,
                "options":[{"label":"Allow"},{"label":"Cancel"}]}]
        }),
    );
    let prompt = DirectInteraction::from_event(&secret, &active())
        .unwrap()
        .unwrap();
    assert!(
        prompt
            .mcp_tool_decision(ManualDecision::AcceptOnce)
            .is_err()
    );
}
