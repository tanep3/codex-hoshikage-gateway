use codex_hoshikage_gateway::direct_workspace::{
    ensure_conversation_workspace, ensure_conversation_workspace_at,
};
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

#[test]
fn configured_root_is_visible_and_cannot_be_a_symlink() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("visible-workspaces");
    let workspace = ensure_conversation_workspace_at(&root, "123456789").unwrap();
    assert_eq!(workspace, root.join("123456789"));
    let alias = base.path().join("alias");
    symlink(&root, &alias).unwrap();
    assert!(ensure_conversation_workspace_at(&alias, "987654321").is_err());
}
