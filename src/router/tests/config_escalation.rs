use super::*;

#[test]
fn array_form_deserializes_and_lists_models() {
    let group: ModelGroup = serde_json::from_str(r#"["fast", "small"]"#).unwrap();
    assert_eq!(group.models(), &["fast".to_string(), "small".to_string()]);
    assert!(group.escalation().is_none());
}

#[test]
fn object_form_models_only() {
    let group: ModelGroup = serde_json::from_str(r#"{"models": ["code-model"]}"#).unwrap();
    assert_eq!(group.models(), &["code-model".to_string()]);
    assert!(group.escalation().is_none());
}

#[test]
fn object_form_with_escalation() {
    let group: ModelGroup = serde_json::from_str(
        r#"{
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
        }"#,
    )
    .unwrap();
    assert_eq!(group.models(), &["code-model".to_string()]);
    let ladder = group.escalation().expect("escalation present");
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
fn array_form_round_trips() {
    let group: ModelGroup = serde_json::from_str(r#"["fast", "small"]"#).unwrap();
    let serialized = serde_json::to_string(&group).unwrap();
    let back: ModelGroup = serde_json::from_str(&serialized).unwrap();
    assert_eq!(back.models(), group.models());
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
fn empty_models_object_deserializes() {
    let group: ModelGroup = serde_json::from_str("{}").unwrap();
    assert!(group.models().is_empty());
    assert!(group.escalation().is_none());
}

#[test]
fn group_member_parse_accepts_bare_sentinels_only() {
    assert_eq!(GroupMember::parse("last"), GroupMember::Last);
    assert_eq!(GroupMember::parse("any"), GroupMember::Any);
    // Case-sensitive: upper/mixed case stays a literal key.
    assert_eq!(GroupMember::parse("LAST"), GroupMember::Key("LAST".into()));
    assert_eq!(GroupMember::parse("Any"), GroupMember::Key("Any".into()));
    // Qualified forms stay literal qualifiers — never sentinels.
    assert_eq!(
        GroupMember::parse("code:last"),
        GroupMember::Key("code:last".into())
    );
    assert_eq!(
        GroupMember::parse("base:any"),
        GroupMember::Key("base:any".into())
    );
    assert_eq!(
        GroupMember::parse("code:default"),
        GroupMember::Key("code:default".into())
    );
}

#[test]
fn group_member_key_round_trips_through_raw() {
    for raw in ["last", "any", "LAST", "code:default", "code:last", "fast"] {
        let member = GroupMember::parse(raw);
        assert_eq!(member.raw(), raw, "parse→raw must be identity");
    }
}

#[test]
fn model_group_models_shape_unchanged_with_sentinels_present() {
    // `models()` keeps returning the raw member strings; sentinel expansion
    // is a dispatch-time concern, not a config-shape one.
    let group: ModelGroup =
        serde_json::from_str(r#"["code:default", "last", "any"]"#).unwrap();
    assert_eq!(
        group.models(),
        &[
            "code:default".to_string(),
            "last".to_string(),
            "any".to_string()
        ]
    );
    assert_eq!(
        group.members(),
        vec![
            GroupMember::Key("code:default".into()),
            GroupMember::Last,
            GroupMember::Any,
        ]
    );
}

#[test]
fn unknown_qualified_key_still_resolves_none() {
    // `base:last` is a literal qualifier: with no such base model the lookup
    // fails closed exactly as any unknown qualifier does today.
    let cfg: crate::config::RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"r": {"group": "g"}},
        "models": {},
        "model_groups": {"g": ["tiny"]},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "r",
    }))
    .expect("config");
    assert!(cfg.target_for_key("nosuch:last").is_none());
    assert!(cfg.target_for_key("nosuch:any").is_none());
}

#[test]
fn router_config_shipped_array_shape_still_parses() {
    // The shipped `env/coral-router.json` shape: array-form model_groups.
    let cfg: crate::config::RouterConfig = serde_json::from_str(
        r#"{
            "model_groups": {"fast": ["fast"], "code": ["code-model"]},
            "models": {}
        }"#,
    )
    .unwrap();
    assert_eq!(cfg.model_groups.len(), 2);
    assert_eq!(cfg.model_groups["fast"].models(), &["fast".to_string()]);
    assert!(cfg.model_groups["fast"].escalation().is_none());
}

#[test]
fn implicit_sentinel_suffix_appends_once() {
    // Dispatch lists gain `last`/`any` unless declared; declared sentinels
    // are never duplicated. Serialization keeps the declared form.
    let group: ModelGroup = serde_json::from_str(r#"["code"]"#).unwrap();
    assert_eq!(
        group.effective_models(),
        vec!["code".to_string(), "last".to_string(), "any".to_string()]
    );
    let declared: ModelGroup = serde_json::from_str(r#"["code", "last", "any"]"#).unwrap();
    assert_eq!(
        declared.effective_models(),
        vec!["code".to_string(), "last".to_string(), "any".to_string()],
        "declared sentinels are not duplicated"
    );
    let opted_out: ModelGroup =
        serde_json::from_value(serde_json::json!({"models": ["code"], "sentinels": false}))
            .unwrap();
    assert_eq!(opted_out.effective_models(), vec!["code".to_string()]);
    let back = serde_json::to_string(&group).unwrap();
    assert_eq!(back, r#"["code"]"#, "declared form round-trips");
}

#[test]
fn empty_group_rides_default_role() {
    // A group declaring no role/model members (empty, or sentinels only)
    // dispatches through the `default` role instead of resolving to nothing.
    let empty: ModelGroup = serde_json::from_str(r#"[]"#).unwrap();
    assert_eq!(empty.duty_members(), vec!["default".to_string()]);
    assert_eq!(
        empty.effective_models(),
        vec!["default".to_string(), "last".to_string(), "any".to_string()]
    );
    let sentinels_only: ModelGroup = serde_json::from_str(r#"["last", "any"]"#).unwrap();
    assert_eq!(
        sentinels_only.duty_members(),
        vec!["default".to_string()],
        "sentinels alone serve nothing"
    );
    let headed: ModelGroup = serde_json::from_str(r#"["code"]"#).unwrap();
    assert_eq!(headed.duty_members(), vec!["code".to_string()]);
}
