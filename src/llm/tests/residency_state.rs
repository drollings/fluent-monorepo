use super::*;
use crate::backend::{ContextProfile, DEFAULT_CONTEXT_GROUP};

#[test]
fn residency_state_roundtrip() {
    for (state, text) in [
        (ResidencyState::Loaded, "loaded"),
        (ResidencyState::Sleeping, "sleeping"),
        (ResidencyState::Unloaded, "unloaded"),
    ] {
        assert_eq!(state.as_str(), text);
        assert_eq!(state.to_string(), text);
        let parsed: ResidencyState = text.parse().expect("known state parses");
        assert_eq!(parsed, state);
        let json = serde_json::to_value(state).expect("serialize state");
        assert_eq!(json, serde_json::json!(text));
        let back: ResidencyState = serde_json::from_value(json).expect("deserialize state");
        assert_eq!(back, state);
    }
}

#[test]
fn residency_state_reject_unknown() {
    for bad in ["", "LOADED", "ready", "loaded "] {
        assert!(bad.parse::<ResidencyState>().is_err());
    }
}

#[test]
fn default_group_const() {
    assert_eq!(DEFAULT_CONTEXT_GROUP, "default");
    assert_eq!(ContextProfile::default().group, "default");
}
