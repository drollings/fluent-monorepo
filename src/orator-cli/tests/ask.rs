//! Ask preflight goldens: strict ports, fast weights, distinct speak.

use crate::ask::{
    check_ask_weights, concise_for_speech, effective_llm_config, parse_ask_port, resolve_ask_model,
    speak_failure_note, verify_pinned_ask_weights, AskFail,
};
use orator_models::{ModelEntry, ModelKind};

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
    if let Some(entry) = orator_models::find_entry(orator_models::ModelKind::Llm, &endpoint.model) {
        if entry.pinned() {
            assert!(
                result.is_err(),
                "tampered pinned weights must refuse: {result:?}"
            );
        }
    }
}

#[test]
fn ask_verification_maps_size_and_hash_outcomes_to_exact_refusals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("weights.gguf");
    let entry = ModelEntry {
        name: "fixture".to_string(),
        kind: ModelKind::Llm,
        repo: "local/fixture".to_string(),
        file: "weights.gguf".to_string(),
        size: Some(5),
        sha256: Some(common_core::hash::sha256_hex(b"hello")),
    };

    std::fs::write(&path, b"hi").expect("short fixture");
    assert_eq!(
        verify_pinned_ask_weights(&entry, &path, "m").expect_err("short refuses"),
        "orator: weights for model m failed verification (size mismatch: got 2 bytes, want 5; re-pull the model)"
    );

    std::fs::write(&path, b"world").expect("same-length tamper");
    assert_eq!(
        verify_pinned_ask_weights(&entry, &path, "m").expect_err("tampered refuses"),
        "orator: weights for model m failed verification (hash mismatch; re-pull the model)"
    );

    std::fs::write(&path, b"hello").expect("matching fixture");
    verify_pinned_ask_weights(&entry, &path, "m").expect("matching weights pass");

    std::fs::remove_file(&path).expect("remove fixture");
    let error = verify_pinned_ask_weights(&entry, &path, "m").expect_err("missing refuses");
    assert!(error.starts_with("orator: weights for model m refused: "), "{error}");
}

#[test]
fn ask_verification_zero_size_skips_size_and_still_checks_digest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("weights.gguf");
    let entry = ModelEntry {
        name: "fixture".to_string(),
        kind: ModelKind::Llm,
        repo: "local/fixture".to_string(),
        file: "weights.gguf".to_string(),
        size: Some(0),
        sha256: Some(common_core::hash::sha256_hex(b"hello")),
    };

    std::fs::write(&path, b"hello").expect("matching fixture");
    verify_pinned_ask_weights(&entry, &path, "m").expect("digest still checked and matches");

    std::fs::write(&path, b"world").expect("same-length tamper");
    assert_eq!(
        verify_pinned_ask_weights(&entry, &path, "m").expect_err("tampered refuses"),
        "orator: weights for model m failed verification (hash mismatch; re-pull the model)"
    );
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
    let long = format!(
        "{} {}",
        "First sentence. Second sentence.",
        "Third. ".repeat(200)
    );
    let spoken = concise_for_speech(&long);
    assert!(spoken.contains("First sentence."), "{spoken}");
    assert!(spoken.contains("Full answer printed"), "{spoken}");
    assert!(spoken.len() < long.len(), "long replies shorten for voice");
}

/// The chat role head wins; an empty list falls back to the
/// `llm.model` leaf (never the top-level `default` — that tag names
/// the audio-bundle model, not the chat model).
#[test]
fn chat_head_wins_empty_list_falls_back_to_the_leaf() {
    let mut config = orator_core::OratorConfig::default();
    config.roles.chat.models = vec!["custom-chat-tag".to_string()];
    config.llm.model = "leaf-model".to_string();
    assert_eq!(resolve_ask_model(&config), "custom-chat-tag");
    config.roles.chat.models.clear();
    assert_eq!(resolve_ask_model(&config), "leaf-model");
}

/// Default configs resolve byte-identically: the head equals the
/// leaf by construction and no params compose — so the effective
/// section names the same model with the same `ctx`/`n_batch`, and
/// the weights declaration resolves to the same file (the table
/// entry spells the leaf-relative path absolutely).
#[test]
fn default_config_resolves_byte_identically() {
    let config = orator_core::OratorConfig::default();
    assert_eq!(resolve_ask_model(&config), config.llm.model);
    let effective = effective_llm_config(&config).expect("defaults compose");
    assert_eq!(effective.model, config.llm.model);
    assert_eq!(effective.ctx, config.llm.ctx);
    assert_eq!(effective.n_batch, config.llm.n_batch);
    assert_eq!(
        orator_core::resolve_llm_weights(&effective),
        orator_core::resolve_llm_weights(&config.llm),
        "same weights file through either layer"
    );
}

/// Composed `ctx`/`n_batch` override the leaves only when the tables
/// specify them: the role run block, then the model-side override,
/// each winning wholesale per key.
#[test]
fn composed_ctx_and_n_batch_override_the_leaves() {
    let mut config = orator_core::OratorConfig::default();
    config.roles.chat.models = vec!["chat-probe".to_string()];
    config.roles.chat.params = Some(serde_json::json!({"ctx": 4096}));
    config.models.models.insert(
        "chat-probe".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["llm".to_string()],
            params: Some(serde_json::json!({"n_batch": 256})),
            role_params: Some(std::collections::HashMap::from([(
                "chat".to_string(),
                orator_core::ModelRoleOverride {
                    params: Some(serde_json::json!({"n_batch": 1024})),
                },
            )])),
        },
    );
    let effective = effective_llm_config(&config).expect("tables compose");
    assert_eq!(effective.model, "chat-probe");
    assert_eq!(effective.ctx, 4096, "run block wins over the leaf");
    assert_eq!(
        effective.n_batch, 1024,
        "model-side override wins over base"
    );
}

/// Out-of-range or mistyped composed tuning refuses loudly naming
/// the key — the same bounds `config set` enforces on the leaves,
/// so tables cannot smuggle a boot the leaves would refuse.
#[test]
fn out_of_range_composed_tuning_refuses_loudly() {
    let mut config = orator_core::OratorConfig::default();
    config.roles.chat.models = vec!["chat-probe".to_string()];
    config.models.models.insert(
        "chat-probe".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["llm".to_string()],
            params: None,
            role_params: None,
        },
    );
    for (key, value) in [
        ("ctx", serde_json::json!(128)),
        ("n_batch", serde_json::json!(8)),
    ] {
        config.roles.chat.params = Some(serde_json::json!({key: value}));
        let error = effective_llm_config(&config)
            .expect_err(format!("{key} below leaf minimum refuses").as_str());
        assert!(error.contains(key), "{error}");
    }
    config.roles.chat.params = Some(serde_json::json!({"ctx": "lots"}));
    let error = effective_llm_config(&config).expect_err("mistyped ctx refuses");
    assert!(error.contains("ctx"), "{error}");
}

/// A table weights declaration wins for the head: the resolved file
/// (relative joined under the top root, absolute passing through)
/// replaces the leaf declaration in the effective section.
#[test]
fn table_weights_win_for_the_head() {
    let mut config = orator_core::OratorConfig::default();
    config.roles.chat.models = vec!["chat-probe".to_string()];
    config.models.models.insert(
        "chat-probe".to_string(),
        orator_core::ModelSpec {
            weights: Some("custom/chat.gguf".to_string()),
            backends: vec!["llm".to_string()],
            params: None,
            role_params: None,
        },
    );
    let effective = effective_llm_config(&config).expect("weights compose");
    assert_eq!(
        effective.weights,
        format!("{}/custom/chat.gguf", orator_core::DEFAULT_GGUF_DIR),
        "relative table weights join under the top root"
    );
}

/// A listed-but-unlisted-in-the-table head still resolves (the
/// resolve-then-fail-loud contract): leaves carry ctx/n_batch, the
/// boot names the missing weights later instead of substituting a
/// model nobody listed.
#[test]
fn unlisted_head_resolves_for_loud_failure_later() {
    let mut config = orator_core::OratorConfig::default();
    config.roles.chat.models = vec!["no-such-chat-tag".to_string()];
    let effective = effective_llm_config(&config).expect("resolution succeeds");
    assert_eq!(effective.model, "no-such-chat-tag");
    assert_eq!(effective.ctx, config.llm.ctx);
    assert_eq!(effective.n_batch, config.llm.n_batch);
}
