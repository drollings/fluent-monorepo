//! Characterization table for the three colon-grammars: the routing
//! `base:qualifier` split, the provider `provider:model` split, and the
//! strict instance-id validation. The routing split is total and lenient
//! (qualified iff both halves are non-empty); the provider split differs on
//! empty halves (documented below — provider-defaulting is its own
//! contract); validation is a separate fallible layer (split ≠ validate).
//!
//! Any refactor must keep every vector byte-identical: silent changes here
//! reroute models.

use crate::instance_id::{is_valid_instance_name, InstanceId, InstanceIdError};
use crate::model_key::split_model_key;
use crate::model_ref::parse_model_ref;

#[test]
fn routing_split_boundaries() {
    let cases: &[(&str, (&str, Option<&str>))] = &[
        ("llama", ("llama", None)),
        ("llama:latest", ("llama", Some("latest"))),
        ("llama:inst-1", ("llama", Some("inst-1"))),
        ("llama:", ("llama:", None)),
        (":latest", (":latest", None)),
        ("", ("", None)),
        ("a:b:c", ("a", Some("b:c"))),
        ("mödell:instänz-1", ("mödell", Some("instänz-1"))),
        ("m:bad name!", ("m", Some("bad name!"))),
    ];
    for (input, want) in cases {
        assert_eq!(split_model_key(input), *want, "input: {input:?}");
    }
}

#[test]
fn provider_split_agrees_except_on_empty_halves() {
    // Agreement: bare is bare, first colon splits, multi-colon keeps the tail.
    assert_eq!(parse_model_ref("plain"), None);
    assert_eq!(parse_model_ref(""), None);
    assert_eq!(parse_model_ref("ollama:llama3"), Some(("ollama", "llama3")));
    assert_eq!(parse_model_ref("a:b:c"), Some(("a", "b:c")));
    // PINNED DIVERGENCE (provider-defaulting contract — not a bug): empty
    // halves still split here, while the routing split treats them as bare.
    assert_eq!(parse_model_ref("llama:"), Some(("llama", "")));
    assert_eq!(parse_model_ref(":latest"), Some(("", "latest")));
    assert_eq!(parse_model_ref(":"), Some(("", "")));
}

#[test]
fn validation_is_a_separate_layer() {
    // Split accepts; validation rejects (never conflate the two).
    assert!(is_valid_instance_name("latest"));
    assert!(is_valid_instance_name("inst_1.ok-2"));
    assert!(!is_valid_instance_name("b:c"));
    assert!(!is_valid_instance_name("instänz-1"));
    assert!(!is_valid_instance_name("bad name!"));
    assert!(!is_valid_instance_name(""));
    // Strict parse rejects what the lenient splits accept.
    assert!(InstanceId::parse("llama:latest").is_ok());
    assert!(InstanceId::parse("model:inst_1.ok-2").is_ok());
    assert_eq!(
        InstanceId::parse("llama").unwrap_err(),
        InstanceIdError::MissingSeparator
    );
    assert_eq!(
        InstanceId::parse("a:b:c").unwrap_err(),
        InstanceIdError::ExtraSeparator
    );
    assert_eq!(
        InstanceId::parse("llama:").unwrap_err(),
        InstanceIdError::EmptyName
    );
    assert_eq!(
        InstanceId::parse(":latest").unwrap_err(),
        InstanceIdError::EmptyModel
    );
}
