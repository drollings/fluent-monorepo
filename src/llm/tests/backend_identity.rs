use super::*;
use crate::testutil::{CountingStubBackend, StubBackend};
use std::sync::Arc;

#[test]
fn well_known_backend_strings() {
    assert_eq!(WellKnownBackend::Llama.as_str(), "llama");
    assert_eq!(WellKnownBackend::Onnx.as_str(), "onnx");
    assert_eq!(WellKnownBackend::Llama.to_string(), "llama");
    let llama: WellKnownBackend = "llama".parse().expect("parse llama");
    assert_eq!(llama, WellKnownBackend::Llama);
    let onnx: WellKnownBackend = "onnx".parse().expect("parse onnx");
    assert_eq!(onnx, WellKnownBackend::Onnx);
    assert!("multimodal".parse::<WellKnownBackend>().is_err());
    assert!("".parse::<WellKnownBackend>().is_err());
}

#[test]
fn base_key_parity_corpus() {
    for (input, expected) in [
        ("model-a", "model-a"),
        ("base:qualifier", "base"),
        ("base:", "base:"),
        (":qualifier", ":qualifier"),
        ("", ""),
        ("a:b:c", "a"),
    ] {
        assert_eq!(super::base_key(input), expected, "input {input:?}");
    }
}

#[test]
fn route_chat_skips_failed() {
    let mut registry = InferenceRegistry::new();
    registry.register(Arc::new(CountingStubBackend::failed(
        "failed-stub",
        vec!["model-a"],
    )));
    registry.register(Arc::new(StubBackend::new("good", vec!["model-a"], "hello")));
    let backend = registry
        .route_chat("model-a", None)
        .expect("good backend serves after failed is skipped");
    let out = backend
        .chat_complete(&[crate::protocol::ChatMessage {
            role: "user".into(),
            content: "hi".into(),
        }])
        .unwrap();
    assert_eq!(out, "hello");
}
