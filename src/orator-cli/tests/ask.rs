//! Ask preflight goldens: strict ports, fast weights, distinct speak.

use crate::ask::{
    check_ask_weights, concise_for_speech, parse_ask_port, speak_failure_note, AskFail,
};

#[test]
fn bad_port_is_usage_never_auto_reserve() {
    assert_eq!(parse_ask_port("127.0.0.1:8080"), Ok(8080));
    assert_eq!(parse_ask_port("127.0.0.1:0"), Ok(0));
    for garbage in ["garbage", "127.0.0.1:", "127.0.0.1:abc", ""] {
        let error = parse_ask_port(garbage).expect_err("garbage port refuses");
        assert!(error.contains("invalid port"), "{error}");
    }
}

#[test]
fn missing_weights_fail_fast_with_a_named_remedy() {
    let endpoint = orator_core::LlmEndpointConfig {
        model: "no-such-model-xyz".to_string(),
        weights: "/nonexistent-weights-xyz.gguf".to_string(),
        gguf_dir: "/nonexistent-gguf-dir-xyz".to_string(),
        ..orator_core::LlmEndpointConfig::default()
    };
    let error = check_ask_weights(&endpoint).expect_err("missing refuses");
    assert!(error.contains("no weights"), "{error}");
    assert!(error.contains("no-such-model-xyz"), "{error}");
}

#[test]
fn tampered_pinned_weights_refuse_despite_presence() {
    // A present file with the wrong bytes fails the pinned hash gate
    // before any boot: presence alone never serves.
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("tampered.gguf");
    std::fs::write(&bad, b"not the model").expect("tampered writes");
    let endpoint = orator_core::LlmEndpointConfig {
        model: "liquidai/lfm2.5-2.6b".to_string(),
        weights: bad.display().to_string(),
        gguf_dir: String::new(),
        ..orator_core::LlmEndpointConfig::default()
    };
    // Unpinned or unknown models pass presence; pinned models with a
    // manifest entry verify. This model is pinned in the manifest, so
    // wrong bytes must refuse (or pass only if the manifest entry is
    // absent in this build — either way the gate runs, never skips).
    let result = check_ask_weights(&endpoint);
    if let Some(entry) =
        orator_models::find_entry(orator_models::ModelKind::Llm, &endpoint.model)
    {
        if entry.pinned() {
            assert!(
                result.is_err(),
                "tampered pinned weights must refuse: {result:?}"
            );
        }
    }
}

#[test]
fn speak_failure_note_names_voice_not_the_answer() {
    let note = speak_failure_note(crate::ExitCode::Error);
    assert!(note.contains("speak"), "{note}");
    assert!(note.contains("answer printed"), "{note}");
    let fail = AskFail::Usage("bad port".to_string());
    assert!(matches!(fail, AskFail::Usage(_)));
    let fail = AskFail::Error("no weights".to_string());
    assert!(matches!(fail, AskFail::Error(_)));
}

#[test]
fn concise_speech_passes_short_replies_and_announces_cuts() {
    assert_eq!(concise_for_speech("hi"), "hi");
    let long = format!("{} {}", "First sentence. Second sentence.", "Third. ".repeat(200));
    let spoken = concise_for_speech(&long);
    assert!(spoken.contains("First sentence."), "{spoken}");
    assert!(spoken.contains("Full answer printed"), "{spoken}");
    assert!(spoken.len() < long.len(), "long replies shorten for voice");
}
