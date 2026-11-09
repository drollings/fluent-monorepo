use super::super::config::{
    default_decision_role, default_generative_role, ModelRole, ProjectConfig,
};

#[test]
fn decision_role_defaults_match_contract() {
    let role = default_decision_role();
    assert_eq!(role.base_url, "http://localhost:8080");
    assert_eq!(role.model, "liquidai/d1-2b");
    assert!(role.enabled);
    assert!(role.timeout_ms > 0);
}

#[test]
fn generative_role_defaults_match_contract() {
    let role = default_generative_role();
    assert_eq!(role.base_url, "http://localhost:8080");
    assert_eq!(role.model, "code");
    assert!(role.enabled);
    assert!(role.timeout_ms > 0);
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
fn unknown_keys_are_tolerated() {
    let raw = r#"{"guidance_dir": ".guidance", "future_field": {"nested": true}}"#;
    let config: ProjectConfig = serde_json::from_str(raw).expect("unknown keys ignored");
    assert_eq!(config.guidance_dir, std::path::PathBuf::from(".guidance"));
}
