use codex_hoshikage_gateway::{
    codex_execution::{TurnIdentity, TurnSnapshot},
    direct_images::{GeneratedImageStatus, inventory},
};
use serde_json::json;

fn snapshot(items: Vec<serde_json::Value>, full: bool) -> TurnSnapshot {
    TurnSnapshot {
        identity: TurnIdentity {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
        },
        status: "completed".into(),
        final_text: None,
        items_view_full: full,
        items,
    }
}

#[test]
fn only_a_full_turn_inventory_can_confirm_no_images() {
    assert!(inventory(&snapshot(vec![], false), 16, 1024).is_err());
    assert!(
        inventory(&snapshot(vec![], true), 16, 1024)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ready_failed_and_unknown_images_keep_their_order_and_identity() {
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jGZkAAAAASUVORK5CYII=";
    let found = inventory(
        &snapshot(
            vec![
                json!({"type":"agentMessage","text":"done"}),
                json!({"type":"imageGeneration","id":"one","status":"completed","result":png}),
                json!({"type":"imageGeneration","id":"two","status":"failed"}),
                json!({"type":"imageGeneration","id":"three","status":"inProgress"}),
            ],
            true,
        ),
        16,
        1024,
    )
    .unwrap();
    assert_eq!(
        found
            .iter()
            .map(|image| image.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["one", "two", "three"]
    );
    assert_eq!(found[0].ordinal, 0);
    assert!(
        matches!(&found[0].status,GeneratedImageStatus::Ready(bytes) if bytes.starts_with(b"\x89PNG"))
    );
    assert_eq!(found[1].status, GeneratedImageStatus::Failed);
    assert_eq!(found[2].status, GeneratedImageStatus::Unknown);
}

#[test]
fn duplicate_or_oversize_results_do_not_become_delivery_candidates() {
    let duplicate = snapshot(
        vec![
            json!({"type":"imageGeneration","id":"one","status":"failed"}),
            json!({"type":"imageGeneration","id":"one","status":"failed"}),
        ],
        true,
    );
    assert!(inventory(&duplicate, 16, 1024).is_err());
    assert!(inventory(&duplicate, 1, 1024).is_err());
    let invalid = snapshot(
        vec![
            json!({"type":"imageGeneration","id":"one","status":"completed","result":"bm90LXBuZw=="}),
        ],
        true,
    );
    assert!(inventory(&invalid, 16, 1024).is_err());
}
