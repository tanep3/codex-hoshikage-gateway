use codex_hoshikage_gateway::{codex_transport::LaunchConfig, direct_models::DirectModelCatalog};
use std::{path::PathBuf, time::Duration};

#[tokio::test]
async fn catalog_uses_its_own_child_outside_run_capacity() {
    let catalog = DirectModelCatalog {
        launch: LaunchConfig {
            command: "python3".into(),
            args: vec![format!(
                "{}/tests/fixtures/mock_app_server.py",
                env!("CARGO_MANIFEST_DIR")
            )],
            codex_home: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            initialize_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(2),
            experimental_api: true,
        },
    };
    let models = catalog.list().await.unwrap();
    assert_eq!(models[0].id, "gpt-5.6-luna");
    assert!(models.iter().any(|model| model.id == "gpt-6-luna"));
    assert_eq!(models[0].default_reasoning_effort, "medium");
    assert!(
        models[0]
            .supported_reasoning_efforts
            .iter()
            .any(|effort| effort.id == "high")
    );
    catalog.validate("gpt-5.6-luna").await.unwrap();
    catalog
        .validate_effort("gpt-5.6-luna", "high")
        .await
        .unwrap();
    assert!(
        catalog
            .validate_effort("gpt-5.6-luna", "ultra")
            .await
            .is_err()
    );
    assert!(catalog.validate("unknown").await.is_err());
}
