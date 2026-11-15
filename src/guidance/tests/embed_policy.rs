//! Embed policy pins: off by default, explicit opt-in.
use crate::config::EmbedPolicy;

#[test]
fn embed_policy_defaults_off() {
    let cfg = crate::config::ProjectConfig::default();
    assert_eq!(cfg.embed_policy, EmbedPolicy::Off);
}

#[test]
fn embed_policy_parses_all_forms() {
    let cfg: crate::config::ProjectConfig =
        serde_json::from_str(r#"{"embed_policy":"all"}"#).expect("parse");
    assert_eq!(cfg.embed_policy, EmbedPolicy::All);
    let cfg: crate::config::ProjectConfig =
        serde_json::from_str(r#"{"embed_policy":"changed"}"#).expect("parse");
    assert_eq!(cfg.embed_policy, EmbedPolicy::Changed);
}
