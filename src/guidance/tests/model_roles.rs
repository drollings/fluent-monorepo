use super::super::config::{
    calibration_path, default_decision_role, default_generative_role, load_config, ModelRole,
    ModelTransport, ProjectConfig,
};

#[test]
fn decision_role_defaults_match_contract() {
    let role = default_decision_role();
    assert_eq!(role.base_url, "http://localhost:8080");
    assert_eq!(role.model, "liquidai/d1-2b");
    assert!(role.enabled);
    assert!(role.timeout_ms > 0);
    assert_eq!(role.transport, ModelTransport::SystemOne);
}

#[test]
fn generative_role_defaults_match_contract() {
    let role = default_generative_role();
    assert_eq!(role.base_url, "http://localhost:8080");
    assert_eq!(role.model, "code");
    assert!(role.enabled);
    assert!(role.timeout_ms > 0);
    assert_eq!(role.transport, ModelTransport::Chat);
    assert_eq!(role.max_tokens, Some(2048));
}

#[test]
fn roles_are_independently_addressable_by_default() {
    let config = ProjectConfig::default();
    assert_eq!(config.decision.base_url, config.generative.base_url);
    assert_ne!(config.decision.model, config.generative.model);
}

#[test]
fn disabled_role_resolves_to_no_endpoint() {
    let role = ModelRole {
        enabled: false,
        ..default_decision_role()
    };
    assert!(role.endpoint().is_none());
    let role = default_decision_role();
    let (url, model) = role.endpoint().expect("enabled role resolves");
    assert_eq!(url, "http://localhost:8080");
    assert_eq!(model, "liquidai/d1-2b");
}

#[test]
fn role_block_parses_and_round_trips() {
    let raw = r#"{
        "decision": {"base_url": "http://decision-model.test:8080", "model": "other", "timeout_ms": 5000, "enabled": false},
        "generative": {"model": "code"},
        "decision_thresholds": {"intent": {"act": 0.9, "review": 0.7}},
        "calibration_registry": ".guidance/calibration.json"
    }"#;
    let config: ProjectConfig = serde_json::from_str(raw).expect("parse");
    assert!(!config.decision.enabled);
    assert_eq!(config.generative.base_url, "http://localhost:8080");
    let thresholds = config
        .decision_thresholds
        .get("intent")
        .expect("threshold entry");
    assert!((thresholds.act - 0.9).abs() < f64::EPSILON);
    let back = serde_json::to_value(&config).expect("serialize");
    assert_eq!(back["decision"]["model"], serde_json::json!("other"));
}

#[test]
fn legacy_models_map_resolves_into_roles() {
    let dir = fluent_wvr_testutil::tempdir();
    std::fs::create_dir_all(dir.path().join(".guidance")).expect("dir");
    std::fs::write(
        dir.path().join(".guidance/guidance-config.json"),
        r#"{
            "providers": {"llama": {"base_url": "http://localhost:8080", "chat_endpoint": "/v1/chat/completions"}},
            "models": {"decision": "llama:decision", "default": "llama:code"}
        }"#,
    )
    .expect("config");
    let config = load_config(dir.path()).expect("load");
    assert_eq!(config.decision.model, "decision");
    assert_eq!(config.decision.transport, ModelTransport::SystemOne);
    assert_eq!(config.decision.base_url, "http://localhost:8080");
    assert_eq!(config.generative.model, "code");
    assert_eq!(config.generative.transport, ModelTransport::Chat);
}

#[test]
fn explicit_role_wins_over_legacy_map() {
    let dir = fluent_wvr_testutil::tempdir();
    std::fs::create_dir_all(dir.path().join(".guidance")).expect("dir");
    std::fs::write(
        dir.path().join(".guidance/guidance-config.json"),
        r#"{
            "models": {"decision": "llama:ignored"},
            "decision": {"model": "explicit", "transport": "chat"}
        }"#,
    )
    .expect("config");
    let config = load_config(dir.path()).expect("load");
    assert_eq!(config.decision.model, "explicit");
    assert_eq!(config.decision.transport, ModelTransport::Chat);
}

#[test]
fn calibration_path_is_workspace_relative() {
    let config = ProjectConfig::default();
    let path = calibration_path(&config, std::path::Path::new("/repo"));
    assert_eq!(
        path,
        std::path::PathBuf::from("/repo/.guidance/state/calibration.json")
    );
}
