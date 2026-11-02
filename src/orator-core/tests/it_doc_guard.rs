//! Documentation guard: every name below must resolve in code (the imports)
//! and be mentioned in the normative spec (the assertions). Renaming a type
//! without updating the spec fails here, not in review.

#[allow(unused_imports)]
use orator_core::{
    AudioConfig, DuplexEngine, Epoch, Event, Frame, LlmEndpointConfig, LlmSession, MediaTime,
    OratorConfig, SttConfig, SttStream, TtsConfig, TtsStream, TurnConfig,
};

const SPEC: &str = include_str!("../../../ORATOR.md");

#[test]
fn spec_names_only_real_public_items() {
    for name in [
        "Frame",
        "MediaTime",
        "Event",
        "SttStream",
        "LlmSession",
        "TtsStream",
        "DuplexEngine",
    ] {
        assert!(
            SPEC.contains(name),
            "spec must keep documenting `{name}` while the type exists"
        );
    }
}
