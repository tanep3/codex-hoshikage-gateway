use codex_hoshikage_gateway::direct_workspace::ensure_conversation_workspace;
use std::os::unix::fs::symlink;

#[test]
fn creates_a_private_workspace_and_rejects_path_or_link_tricks() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let workspace = ensure_conversation_workspace(&state, "123456789").unwrap();
    assert_eq!(workspace, state.join("workspaces/123456789"));
    assert_eq!(
        ensure_conversation_workspace(&state, "123456789").unwrap(),
        workspace
    );
    assert!(ensure_conversation_workspace(&state, "../evil").is_err());
    assert!(ensure_conversation_workspace(&state, "0").is_err());
    symlink(root.path(), state.join("workspaces/222")).unwrap();
    assert!(ensure_conversation_workspace(&state, "222").is_err());
}
