mod common;
use codex_hoshikage_gateway::{
    direct_config::{Codex, DirectConfig},
    storage::{self, Store},
};
use std::{fs, os::unix::fs::PermissionsExt};

#[tokio::test]
async fn later_model_selection_wins_even_when_validation_completes_out_of_order() {
    let temp = tempfile::tempdir().unwrap();
    let old = common::config(&temp);
    let home = temp.path().join("codex-home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let cfg = DirectConfig {
        discord: old.discord,
        codex: Codex {
            command: std::env::current_exe().unwrap(),
            home,
            model_provider: "openai".into(),
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            network_access: false,
        },
        storage: old.storage,
        limits: old.limits,
        default_model: "gpt-5.6-luna".into(),
    };
    storage::initialize_direct(&cfg).unwrap();
    let (store, _) = Store::open_direct(&cfg).unwrap();
    store
        .add_conversation("4".into(), storage::PROXY_SCOPE.into())
        .await
        .unwrap();
    assert!(
        store
            .select_direct_model("4".into(), "102".into(), "gpt-5.6-terra".into())
            .await
            .unwrap()
    );
    assert!(
        !store
            .select_direct_model("4".into(), "101".into(), "gpt-5.6-luna".into())
            .await
            .unwrap()
    );
    assert!(
        store
            .select_direct_model("4".into(), "102".into(), "gpt-5.6-terra".into())
            .await
            .unwrap()
    );
    assert!(
        store
            .select_direct_model("4".into(), "102".into(), "different".into())
            .await
            .is_err()
    );
    assert_eq!(
        store.conversation("4").await.unwrap().selected_model,
        "gpt-5.6-terra"
    );
}
