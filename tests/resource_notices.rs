use codex_hoshikage_gateway::resources::resource_notice_message;

#[test]
fn uncertain_discord_send_overrides_stale_retrieval_failure() {
    for state in ["WAITING", "CACHED", "FAILED"] {
        let text = resource_notice_message(state, Some("resource_unavailable"), true).unwrap();
        assert!(text.contains("Discordへの送信"));
        assert!(text.contains("/retry"));
        assert!(!text.contains("自動で確認を続ける"));
    }
    assert!(
        resource_notice_message("POST_PENDING", None, false)
            .unwrap()
            .contains("重複")
    );
}

#[test]
fn confirmed_delivery_or_explicit_retry_retires_stale_warning() {
    for state in ["DELIVERED", "RELEASE_PENDING", "SUPERSEDED"] {
        assert_eq!(
            resource_notice_message(state, Some("resource_unavailable"), true),
            None
        );
    }
}

#[test]
fn retrieval_and_terminal_failures_offer_different_actions() {
    let waiting = resource_notice_message("WAITING", None, false).unwrap();
    assert!(waiting.contains("お待ちください"));
    assert!(waiting.contains("/status"));
    let blocked =
        resource_notice_message("BLOCKED", Some("discord_permission_denied"), false).unwrap();
    assert!(blocked.contains("管理者"));
    assert!(blocked.contains("修正後"));
    assert!(!blocked.contains("お待ちください"));
    let expired = resource_notice_message("EXPIRED", Some("output_expired"), false).unwrap();
    assert!(expired.contains("回答テキストは /get では復旧できない"));
}
