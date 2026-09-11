mod common;
#[test]
fn registration_default_reuses_config_and_requires_unambiguous_choice() {
    let t = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&t);
    assert_eq!(cfg.registration_model(), Some("chatgpt/test"));
    let mut second = cfg.projects[0].clone();
    second.id = "another".into();
    cfg.projects.push(second);
    assert_eq!(cfg.registration_model(), Some("chatgpt/test"));
    cfg.projects[1].default_model = "chatgpt/other".into();
    assert_eq!(cfg.registration_model(), None);
    cfg.registration.default_model = Some("chatgpt/explicit".into());
    assert_eq!(cfg.registration_model(), Some("chatgpt/explicit"));
    cfg.registration.default_model = None;
    cfg.projects[1].lifecycle = "RETIRED".into();
    assert_eq!(cfg.registration_model(), Some("chatgpt/test"));
    cfg.projects.clear();
    assert_eq!(cfg.registration_model(), None);
}

#[test]
fn gateway_never_requires_a_local_workspace() {
    let t = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&t);
    cfg.projects[0].cwd = std::path::PathBuf::from("/not/a/local/path");
    assert!(cfg.validate().is_ok());
    cfg.default_model = Some("chatgpt/test".into());
    cfg.projects.clear();
    assert!(cfg.validate().is_ok());
    assert_eq!(cfg.registration_model(), Some("chatgpt/test"));
}
