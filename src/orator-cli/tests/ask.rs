//! Ask preflight goldens: strict ports, fast weights, distinct speak.

use crate::ask::{check_ask_weights, parse_ask_port, speak_failure_note, AskFail};

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
fn speak_failure_note_names_voice_not_the_answer() {
    let note = speak_failure_note(crate::ExitCode::Error);
    assert!(note.contains("speak"), "{note}");
    assert!(note.contains("answer printed"), "{note}");
    let fail = AskFail::Usage("bad port".to_string());
    assert!(matches!(fail, AskFail::Usage(_)));
    let fail = AskFail::Error("no weights".to_string());
    assert!(matches!(fail, AskFail::Error(_)));
}
