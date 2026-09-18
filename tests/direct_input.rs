use codex_hoshikage_gateway::direct_input::app_server_input;
use serde_json::json;

#[test]
fn converts_text_and_image_to_installed_codex_schema() {
    let input = json!([{"role":"user","content":[
        {"type":"input_text","text":"こんにちは"},
        {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8=","detail":"high"}
    ]}]);
    assert_eq!(
        app_server_input(&input).unwrap(),
        vec![
            json!({"type":"text","text":"こんにちは"}),
            json!({"type":"image","url":"data:image/png;base64,aGVsbG8=","detail":"high"}),
        ]
    );
}

#[test]
fn refuses_unrecognized_or_multiple_messages() {
    assert!(
        app_server_input(&json!([{"role":"system","content":[{"type":"input_text","text":"x"}]}]))
            .is_err()
    );
    assert!(
        app_server_input(
            &json!([{"role":"user","content":[{"type":"input_file","path":"/secret"}]}])
        )
        .is_err()
    );
    assert!(app_server_input(&json!([{"role":"user","content":[{"type":"input_image","image_url":"https://other.example/img.png"}]}])).is_err());
}
