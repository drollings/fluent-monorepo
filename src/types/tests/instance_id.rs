use super::*;

#[test]
fn valid_parse_and_display() {
    let id = InstanceId::parse("model:instance").unwrap();
    assert_eq!(id.model(), &ArcIntern::from("model"));
    assert_eq!(id.name(), &ArcIntern::from("instance"));
    assert_eq!(id.to_string(), "model:instance");
    assert_eq!(id.as_pair(), ("model", "instance"));
}

#[test]
fn parse_rejects_missing_separator() {
    assert_eq!(
        InstanceId::parse("baremodel").unwrap_err(),
        InstanceIdError::MissingSeparator
    );
}

#[test]
fn parse_rejects_extra_separator() {
    assert_eq!(
        InstanceId::parse("m:a:b").unwrap_err(),
        InstanceIdError::ExtraSeparator
    );
}

#[test]
fn parse_rejects_empty_name() {
    assert_eq!(
        InstanceId::parse("model:").unwrap_err(),
        InstanceIdError::EmptyName
    );
}

#[test]
fn parse_rejects_empty_model() {
    assert_eq!(
        InstanceId::parse(":name").unwrap_err(),
        InstanceIdError::EmptyModel
    );
}

#[test]
fn parse_rejects_invalid_chars() {
    for bad in ["m:bad/name", "m:bad name", "m:bad:"] {
        assert!(
            matches!(
                InstanceId::parse(bad).unwrap_err(),
                InstanceIdError::ExtraSeparator | InstanceIdError::InvalidName(_)
            ),
            "bad input {bad}"
        );
    }
    assert_eq!(
        InstanceId::parse("m:bad/name").unwrap_err(),
        InstanceIdError::InvalidName("bad/name".into())
    );
    assert_eq!(
        InstanceId::parse("m:bad name").unwrap_err(),
        InstanceIdError::InvalidName("bad name".into())
    );
}

#[test]
fn accepts_valid_chars() {
    for name in [
        "m:n",
        "model:my.instance_name-1",
        "swarm:ledger",
        "my-model:my_instance.name-1",
    ] {
        assert!(InstanceId::parse(name).is_ok(), "{name}");
    }
}

#[test]
fn new_validates() {
    assert!(InstanceId::new("model", "valid_name").is_ok());
    assert_eq!(
        InstanceId::new("", "name").unwrap_err(),
        InstanceIdError::EmptyModel
    );
    assert_eq!(
        InstanceId::new("model", "").unwrap_err(),
        InstanceIdError::EmptyName
    );
    assert!(matches!(
        InstanceId::new("model", "bad/name").unwrap_err(),
        InstanceIdError::InvalidName(_)
    ));
}

#[test]
fn display_round_trip() {
    let original = "my-model:my_instance.name-1";
    let id = InstanceId::parse(original).unwrap();
    assert_eq!(id.to_string(), original);
    assert_eq!(InstanceId::parse(&id.to_string()).unwrap(), id);
}

#[test]
fn from_str_round_trip() {
    let id: InstanceId = "model:instance".parse().unwrap();
    assert_eq!(id.to_string(), "model:instance");
}

#[test]
fn serde_round_trip() {
    let id = InstanceId::parse("model:instance").unwrap();
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "\"model:instance\"");
    let back: InstanceId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}

#[test]
fn aliases_default_includes_bare_and_latest() {
    let id = InstanceId::parse("model:inst").unwrap();
    let aliases = id.aliases("group", true);
    assert!(aliases.contains(&"model".to_string()));
    assert!(aliases.contains(&"model:latest".to_string()));
    assert!(aliases.contains(&"model:group".to_string()));
    assert!(aliases.contains(&"model:inst".to_string()));
}

#[test]
fn aliases_non_default_excludes_bare() {
    let id = InstanceId::parse("model:inst").unwrap();
    let aliases = id.aliases("group", false);
    assert!(!aliases.contains(&"model".to_string()));
    assert!(!aliases.contains(&"model:latest".to_string()));
    assert!(aliases.contains(&"model:group".to_string()));
    assert!(aliases.contains(&"model:inst".to_string()));
}

#[test]
fn is_valid_instance_name_cases() {
    assert!(is_valid_instance_name("abc-123._"));
    assert!(!is_valid_instance_name(""));
    assert!(!is_valid_instance_name("bad/name"));
    assert!(!is_valid_instance_name("bad name"));
    assert!(!is_valid_instance_name("bad:name"));
}

#[test]
fn new_and_parse_agree_on_separator_vs_name_errors() {
    // `a:b` names report `ExtraSeparator` from both constructors, never
    // `InvalidName` from one and `ExtraSeparator` from the other.
    assert_eq!(
        InstanceId::new("m", "a:b").unwrap_err(),
        InstanceIdError::ExtraSeparator
    );
    assert_eq!(
        InstanceId::parse("m:a:b").unwrap_err(),
        InstanceIdError::ExtraSeparator
    );
}
