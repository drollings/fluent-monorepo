use super::*;
use crate::config::RoleEntry;

fn role_from(json: serde_json::Value) -> RoleEntry {
    serde_json::from_value(json).expect("role parses")
}

#[test]
fn role_models_list_deserializes() {
    let role = role_from(serde_json::json!({"models": ["fast", "small"]}));
    assert_eq!(role.effective_models(), vec!["fast".to_string(), "small".to_string()]);
    assert!(role.escalation.is_none());
}

#[test]
fn role_empty_models_rides_last_sentinel() {
    // A role declaring no `models` list dispatches through the `last`
    // availability sentinel instead of resolving to nothing.
    let empty = role_from(serde_json::json!({}));
    assert!(empty.models.is_empty());
    assert_eq!(empty.effective_models(), vec!["last".to_string()]);
    assert!(empty.escalation.is_none());
}

#[test]
fn role_with_escalation() {
    let role = role_from(serde_json::json!(
        {
            "models": ["code-model"],
            "escalation": {
                "modes": ["filter", "question", "team", "turnover"],
                "frontier": {"endpoint": "https://frontier.example/v1/chat/completions", "model": "claude-sonnet", "api_key_env": "ANTHROPIC_KEY"},
                "decomposer_model": "fast",
                "assembler_model": "fast",
                "classifier_model": "small",
                "classifier_parallel": 5,
                "draft_model": "small",
                "judge_model": "fast"
            }
        }
    ));
    assert_eq!(role.effective_models(), vec!["code-model".to_string()]);
    let ladder = role.escalation.expect("escalation present");
    assert_eq!(
        ladder.modes,
        vec![
            EscalationMode::Filter,
            EscalationMode::Question,
            EscalationMode::Team,
            EscalationMode::Turnover
        ]
    );
    let front = ladder.frontier.as_ref().unwrap();
    assert_eq!(
        front.endpoint,
        "https://frontier.example/v1/chat/completions"
    );
    assert_eq!(front.model, "claude-sonnet");
    assert_eq!(front.api_key_env.as_deref(), Some("ANTHROPIC_KEY"));
    assert_eq!(ladder.decomposer_model.as_deref(), Some("fast"));
    assert_eq!(ladder.classifier_parallel, 5);
    assert_eq!(ladder.draft_model.as_deref(), Some("small"));
    assert_eq!(ladder.judge_model.as_deref(), Some("fast"));
}

#[test]
fn role_models_round_trip() {
    let role = role_from(serde_json::json!({"models": ["fast", "small"]}));
    let serialized = serde_json::to_string(&role).unwrap();
    let back: RoleEntry = serde_json::from_str(&serialized).unwrap();
    assert_eq!(back.effective_models(), role.effective_models());
}

#[test]
fn ladder_defaults() {
    let ladder = EscalationLadderConfig::default();
    assert!(ladder.modes.is_empty());
    assert!(ladder.frontier.is_none());
    assert_eq!(ladder.classifier_parallel, 3);
}

#[test]
fn ladder_missing_fields_default() {
    let ladder: EscalationLadderConfig =
        serde_json::from_str(r#"{"frontier": {"endpoint": "u", "model": "m"}}"#).unwrap();
    assert!(ladder.modes.is_empty());
    assert_eq!(ladder.classifier_parallel, 3, "unset parallel defaults");
    assert!(ladder.decomposer_model.is_none());
}

#[test]
fn escalation_mode_list_deserializes() {
    let modes: Vec<EscalationMode> = serde_json::from_str(r#"["filter","team"]"#).unwrap();
    assert_eq!(modes, vec![EscalationMode::Filter, EscalationMode::Team]);
}

#[test]
fn role_member_parse_accepts_bare_sentinels_only() {
    assert_eq!(RoleMember::parse("last"), RoleMember::Last);
    assert_eq!(RoleMember::parse("any"), RoleMember::Any);
    // Case-sensitive: upper/mixed case stays a literal key.
    assert_eq!(RoleMember::parse("LAST"), RoleMember::Key("LAST".into()));
    assert_eq!(RoleMember::parse("Any"), RoleMember::Key("Any".into()));
    // Qualified forms stay literal qualifiers — never sentinels.
    assert_eq!(
        RoleMember::parse("quick:last"),
        RoleMember::Key("quick:last".into())
    );
    assert_eq!(
        RoleMember::parse("base:any"),
        RoleMember::Key("base:any".into())
    );
    assert_eq!(
        RoleMember::parse("code:default"),
        RoleMember::Key("code:default".into())
    );
}

#[test]
fn role_member_key_round_trips_through_raw() {
    for raw in ["last", "any", "LAST", "code:default", "code:last", "fast"] {
        let member = RoleMember::parse(raw);
        assert_eq!(member.raw(), raw, "parse→raw must be identity");
    }
}

#[test]
fn unknown_qualified_key_still_resolves_none() {
    // `base:last` is a literal qualifier: with no such base model the lookup
    // fails closed exactly as any unknown qualifier does today.
    let cfg: crate::config::RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"r": {"role": "s"}},
        "models": {},
        "roles": {"s": {"models": ["tiny"]}},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "r",
    }))
    .expect("config");
    assert!(cfg.target_for_key("nosuch:last").is_none());
    assert!(cfg.target_for_key("nosuch:any").is_none());
}

#[test]
fn router_config_roles_shape_parses() {
    // The role-owned member shape: routes name roles, roles name models.
    let cfg: crate::config::RouterConfig = serde_json::from_str(
        r#"{
            "roles": {"fast": {"models": ["fast"]}, "code": {"models": ["code-model"]}},
            "models": {}
        }"#,
    )
    .unwrap();
    assert_eq!(cfg.roles.len(), 2);
    assert_eq!(
        cfg.roles["fast"].effective_models(),
        vec!["fast".to_string()]
    );
    assert!(cfg.roles["fast"].escalation.is_none());
}
