use super::*;
use crate::protocol::ChatMessage;

#[test]
fn roles_parse_roundtrip() {
    for (role, text) in [
        (ChatRole::System, "system"),
        (ChatRole::User, "user"),
        (ChatRole::Assistant, "assistant"),
        (ChatRole::Tool, "tool"),
    ] {
        assert_eq!(role.as_str(), text);
        assert_eq!(role.to_string(), text);
        let parsed: ChatRole = text.parse().expect("known role parses");
        assert_eq!(parsed, role);
        let json = serde_json::to_value(role).expect("serialize role");
        assert_eq!(json, serde_json::json!(text));
        let back: ChatRole = serde_json::from_value(json).expect("deserialize role");
        assert_eq!(back, role);
    }
}

#[test]
fn roles_reject_unknown() {
    for bad in ["admin", "", "USER", "User", " system", "system "] {
        let parsed: Result<ChatRole, RoleParseError> = bad.parse();
        assert!(parsed.is_err(), "must refuse {bad:?}");
        let de: Result<ChatRole, _> = serde_json::from_value(serde_json::json!(bad));
        assert!(de.is_err(), "serde must refuse {bad:?}");
    }
}

#[test]
fn protocol_role_wire() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "hi".into(),
    };
    let json = serde_json::to_value(&msg).expect("serialize message");
    assert_eq!(json, serde_json::json!({"role": "user", "content": "hi"}));
    let parsed: ChatMessage = serde_json::from_value(json).expect("deserialize message");
    assert_eq!(parsed.role, "user");
    assert_eq!(parsed.role_enum().expect("parse role"), ChatRole::User);

    let via_ctor = ChatMessage::with_role(ChatRole::Assistant, "hello");
    assert_eq!(via_ctor.role, "assistant");
    assert_eq!(
        via_ctor.role_enum().expect("parse role"),
        ChatRole::Assistant
    );
}
