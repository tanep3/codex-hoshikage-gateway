use codex_hoshikage_gateway::direct_content::DirectContent;

#[test]
fn final_answer_is_immutable_and_checked_before_redelivery() {
    let root = tempfile::tempdir().unwrap();
    let content = DirectContent::new(root.path()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let saved = content.save_answer(&id, "hello", 1024).unwrap();
    assert_eq!(content.read_answer(&saved, 1024).unwrap(), "hello");
    assert_eq!(
        content.save_answer(&id, "hello", 1024).unwrap().sha256,
        saved.sha256
    );
    assert!(content.save_answer(&id, "different", 1024).is_err());
    assert!(content.save_answer("../not-an-id", "text", 1024).is_err());
    assert!(
        content
            .save_answer(&uuid::Uuid::new_v4().to_string(), "too big", 2)
            .is_err()
    );
    std::fs::write(root.path().join(&saved.relative_path), "tampered").unwrap();
    assert!(content.read_answer(&saved, 1024).is_err());
}
