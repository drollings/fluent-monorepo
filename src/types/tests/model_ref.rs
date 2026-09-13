//! `model_ref` unit pins: canonical `provider:model` parsing.

use crate::model_ref::{model_name, parse_model_ref, ModelRef};

#[test]
fn model_name_strips_first_segment_only() {
    assert_eq!(model_name("ollama:llama3"), "llama3");
    assert_eq!(model_name("openai:gpt-4"), "gpt-4");
    assert_eq!(model_name("plain_model"), "plain_model");
    assert_eq!(model_name("a:b:c"), "b:c");
    assert_eq!(model_name(""), "");
    assert_eq!(model_name(":"), "");
    assert_eq!(model_name(":x"), "x");
    assert_eq!(model_name("x:"), "");
}

#[test]
fn split_needs_a_colon() {
    assert_eq!(parse_model_ref("ollama:llama3"), Some(("ollama", "llama3")));
    assert_eq!(parse_model_ref("a:b:c"), Some(("a", "b:c")));
    assert_eq!(parse_model_ref(":"), Some(("", "")));
    assert_eq!(parse_model_ref(""), None);
    assert_eq!(parse_model_ref("plain"), None);
}

#[test]
fn model_ref_parse_defaults_provider() {
    assert_eq!(
        ModelRef::parse("ollama:llama3"),
        ModelRef {
            provider: "ollama".to_string(),
            model: "llama3".to_string(),
        }
    );
    assert_eq!(
        ModelRef::parse("a:b:c"),
        ModelRef {
            provider: "a".to_string(),
            model: "b:c".to_string(),
        }
    );
    assert_eq!(
        ModelRef::parse("plain"),
        ModelRef {
            provider: "default".to_string(),
            model: "plain".to_string(),
        }
    );
}
