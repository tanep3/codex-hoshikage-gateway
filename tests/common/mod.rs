#![allow(dead_code)]
use codex_hoshikage_gateway::{
    config::*,
    storage::{self, StateLock, Store},
};
pub fn config(t: &tempfile::TempDir) -> Config {
    let root = t.path();
    std::fs::create_dir_all(root.join("work")).unwrap();
    Config {
        default_model: None,
        registration: Registration::default(),
        discord: Discord {
            guild_id: "1".into(),
            allowed_user_id: "2".into(),
            token_file: root.join("token"),
            response_mode: ResponseMode::All,
        },
        proxy: Proxy {
            base_url: "http://127.0.0.1:4040".into(),
            api_key_file: root.join("key"),
            contract_version: "2.0".into(),
        },
        storage: Storage {
            state_dir: root.join("state"),
            temp_dir: root.join("temp"),
            socket_path: root.join("admin.sock"),
        },
        limits: Limits {
            attachments: 2,
            attachment_bytes: 1024,
            input_bytes: 8192,
            text_bytes: 1024,
            image_pixels: 10000,
            artifact_bytes: 4096,
            temp_bytes: 8192,
            output_bytes: 4096,
            output_total_bytes: 16384,
            delivery_retention_secs: 60,
            queue_conversation: 5,
            queue_global: 20,
            validation_secs: 120,
        },
        projects: vec![Project {
            id: "00000000-0000-4000-8000-000000000001".into(),
            name: "test".into(),
            channel_id: "3".into(),
            cwd: root.join("work"),
            default_model: "chatgpt/test".into(),
            lifecycle: "ACTIVE".into(),
        }],
    }
}
pub async fn store(c: &Config) -> (Store, StateLock) {
    storage::initialize(c).unwrap();
    let lock = StateLock::acquire(&c.storage.state_dir).unwrap();
    let (s, _) = Store::open(c).unwrap();
    s.add_conversation("4".into(), c.projects[0].id.clone())
        .await
        .unwrap();
    (s, lock)
}
pub async fn queued(s: &Store, c: &Config, message: &str) -> String {
    let id = s
        .reserve(message.into(), "4".into(), "meta".into(), c.limits.clone())
        .await
        .unwrap()
        .unwrap();
    s.finalize(id.clone(), "meta".into(), "input".into(), vec![])
        .await
        .unwrap();
    id
}
pub fn caps_v2() -> serde_json::Value {
    serde_json::json!({"contract_version":"2.0","instance_id":"pxy_test","recovery_generation":"gen_test","recovery_state":"ready","features":{"managed_conversations":true,"durable_execution":true,"stop_by_request":true,"stop_before_acceptance":true,"workspace_selection":true,"artifact_capture":true,"artifact_registration_tool":true,"artifact_listing":true,"retention_leases":true,"response_output_retrieval":true},"limits":{"auth_scope":"shared_operator","execution_disconnect_interrupts":false}})
}
