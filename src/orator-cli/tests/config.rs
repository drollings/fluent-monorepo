//! Configuration helper tests: backend resolution, dotted keys,
//! env overrides — all hermetic, no binary, no socket.
//!
//! Reader inventory (M1 characterization pin — current behavior, no
//! production diff): which code reads each root selection leaf, and
//! which side wins today on a disagreeing fixture (leaf value X vs
//! role head Y). Later milestones pin the change explicitly; this
//! table proves what moved.
//!
//! | Leaf | Readers | Winner on disagreement today |
//! |---|---|---|
//! | `stt.model` | `fallback::resolve_local_asr` (unified role walk: heads in order across whisper/moonshine backends, then `default`, then the leaf; `resolve_whisper_model`/`resolve_moonshine_model` are backend-filtered views); `chat::asr_serving` (unified resolution first, else role head, else the leaf); `main::stt_backend` (whisper-only, with the composed `language` for the serving tag) | Role-first in decode paths (a listed head serves at its position; the unlisted leaf closes); label paths prefer resolution, leaf last |
//! | `stt.language` | `main::stt_backend` (constructs `WhisperBackend` directly); `fallback::resolve_asr_language` (composed `language` param wins, leaf is fallback — used by chat/dictate paths) | Split: `stt_backend` uses the leaf unconditionally; `resolve_asr_language` callers use composed-first |
//! | `stt.backend` | `main::transcribe` + `dictate` (`resolve_backend(flag, &config.stt.backend)` picks the CLI flag or this leaf; `"audio-server"` routes to the endpoint, `"whisper"` to the local backend) | Flag wins over the leaf; the leaf wins over every role signal (no role input) |
//! | `llm.model` | `chat::llm_serving` (label only, leaf verbatim); `main::chat` pipeline label; `ask_cmd::ask` / `boot_supervised` (boot + request target); `fallback::local_ask` (direct endpoint dial); `resolve_llm_base` (`LLM_MODEL` env wins over the leaf) | Leaf everywhere (no `roles.chat` exists yet — the M4 gap); env wins where `resolve_llm_base` mediates |
//! | `tts.voice` | `say::say` → `say_kokoro` (leaf verbatim); `fallback::resolve_tts_voice` (composed `voice` param wins, leaf is fallback); `chat::tts_serving` (role head, else the leaf); `doctor::speech_lines` (leaf verbatim) | Split: `say` and doctor use the leaf unconditionally; `resolve_tts_voice`/`tts_serving` callers use composed/role-first |
//! | `s2s.model` | `say::run_audio_turn` (assurance target + session model); `main::transcribe_audio_server` / `dictate_audio_server` (via `run_audio_turn`); `main::chat` native branch (assurance + `ChatEvent` model labels); `orator-daemon::VoiceOwner` (assurance target) | Leaf everywhere (selection ignores the agreed role model — the M5 finding); `should_prefer_s2s` gates *whether* to assure, never *which* model |

use orator::{LlmEndpointConfig, OratorConfig};

use crate::config::{
    audio_base_url, config_get, config_keys, config_set, gpu_llm_args, resolve_backend,
    resolve_llm_base,
};

/// Process env is shared across test threads: the tests below mutate
/// it, so they serialize on one lock.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn flag_wins_over_the_file_value() {
    assert_eq!(resolve_backend("--flag", "file"), "--flag");
    assert_eq!(resolve_backend("", "file"), "file");
}

#[test]
fn llm_base_follows_config_then_env() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    std::env::remove_var("LLM_BASE_URL");
    std::env::remove_var("LLM_MODEL");
    let llm = LlmEndpointConfig {
        endpoint: "10.1.2.3:4321".to_string(),
        model: "config-model".to_string(),
        ..LlmEndpointConfig::default()
    };
    assert_eq!(
        resolve_llm_base(&llm),
        (
            "http://10.1.2.3:4321".to_string(),
            "config-model".to_string()
        )
    );
    assert_eq!(audio_base_url(&llm), "http://10.1.2.3:4321");
    std::env::set_var("LLM_BASE_URL", "http://env-host:9");
    std::env::set_var("LLM_MODEL", "env-model");
    assert_eq!(
        resolve_llm_base(&llm),
        ("http://env-host:9".to_string(), "env-model".to_string())
    );
    assert_eq!(audio_base_url(&llm), "http://env-host:9");
    std::env::remove_var("LLM_BASE_URL");
    std::env::remove_var("LLM_MODEL");
}

#[test]
fn dotted_keys_round_trip_through_the_schema() {
    let mut config = OratorConfig::default();
    config_set(&mut config, "tts.backend", "kokoro").expect("valid set");
    assert_eq!(
        config_get(&config, "tts.backend").expect("valid get"),
        "kokoro"
    );
    config_set(&mut config, "turn.barge_in_ms", "200").expect("valid set");
    assert_eq!(
        config_get(&config, "turn.barge_in_ms").expect("valid get"),
        "200"
    );
    // The llama.cpp audio endpoint is file-settable (default port 8080).
    config_set(&mut config, "s2s.endpoint", "http://127.0.0.1:18080").expect("valid set");
    assert_eq!(
        config_get(&config, "s2s.endpoint").expect("valid get"),
        "http://127.0.0.1:18080"
    );
    assert!(config_keys().contains(&"tts.backend".to_string()));
    assert!(config_keys().contains(&"llm.endpoint".to_string()));
    assert!(config_keys().contains(&"s2s.endpoint".to_string()));
    assert!(config_keys().contains(&"s2s.model".to_string()));
}

/// Chat leaves round-trip through the schema: the session starts are
/// file-settable like every other leaf.
#[test]
fn chat_keys_round_trip_through_the_schema() {
    let mut config = OratorConfig::default();
    for (key, value) in [
        ("chat.voice", "full"),
        ("chat.text", "short"),
        ("chat.mic", "ptt"),
        ("chat.barge", "false"),
        ("chat.narration", "full"),
        ("chat.ptt", "on"),
    ] {
        config_set(&mut config, key, value).expect("valid set");
        assert_eq!(config_get(&config, key).expect("readable"), value);
        assert!(
            config_keys().contains(&key.to_string()),
            "{key} is advertised"
        );
    }
    config_set(&mut config, "chat.voice", "off").expect("off parses");
    assert_eq!(config_get(&config, "chat.voice").expect("readable"), "off");
    config_set(&mut config, "chat.narration", "none").expect("none parses");
    assert_eq!(
        config_get(&config, "chat.narration").expect("readable"),
        "none"
    );
    config_set(&mut config, "chat.ptt", "off").expect("off parses");
    assert_eq!(config_get(&config, "chat.ptt").expect("readable"), "off");
}

#[test]
fn unknown_keys_and_out_of_band_values_fail_actionably() {
    let mut config = OratorConfig::default();
    let error = config_set(&mut config, "nope", "1").expect_err("section.field required");
    assert!(error.contains("section.field"), "{error}");
    let error = config_set(&mut config, "bogus.backend", "x").expect_err("unknown section");
    assert!(error.contains("allowed keys"), "{error}");
    let error = config_set(&mut config, "tts.nope", "x").expect_err("unknown field");
    assert!(error.contains("allowed keys"), "{error}");
    let error = config_set(&mut config, "turn.hold_min_ms", "10").expect_err("below minimum");
    assert!(error.contains("hold_min_ms"), "{error}");
    let error = config_get(&config, "tts.nope").expect_err("unknown field reads fail");
    assert!(error.contains("allowed keys"), "{error}");
}

#[test]
fn gpu_leaves_round_trip_and_reject_garbage() {
    let mut config = OratorConfig::default();
    for key in ["stt.gpu", "llm.gpu", "tts.gpu"] {
        assert!(
            config_keys().contains(&key.to_string()),
            "{key} is settable"
        );
        config_set(&mut config, key, "true").expect("true parses");
        assert_eq!(config_get(&config, key).expect("readable"), "true");
        let error = config_set(&mut config, key, "maybe").expect_err("garbage must fail");
        assert!(error.contains(key), "{error}");
    }
    config_set(&mut config, "tts.gpu", "false").expect("false parses");
    assert_eq!(config_get(&config, "tts.gpu").expect("readable"), "false");
}

#[test]
fn gpu_llm_args_stay_empty_on_cpu_and_pair_on_opt_in() {
    assert!(gpu_llm_args(false).is_empty());
    assert_eq!(
        gpu_llm_args(true),
        vec![
            "--n-gpu-layers".to_string(),
            orator_supervise::GPU_OFFLOAD_LAYERS.to_string()
        ]
    );
}

#[test]
fn gpu_flag_reaches_the_whisper_params() {
    // Spot-check: the `stt.gpu` leaf threads into the backend params.
    // Without a device the GPU opt-in refuses actionably naming the
    // remedy; the default path names the missing model instead —
    // the flag is read, never silently dropped.
    use fluent_concurrency::stream::StreamAbort;
    if orator::gpu::device_present() {
        eprintln!("SKIP gpu-flag-reaches-params: GPU device present, nothing to refuse");
        return;
    }
    let backend =
        orator_voice::stt::WhisperBackend::new(std::path::PathBuf::from("/nonexistent/ggml.bin"), "en")
            .with_gpu();
    let error = backend
        .decode_blocking(&[0.0; 320], &StreamAbort::new())
        .expect_err("GPU without a device must fail");
    assert!(
        error.to_string().contains("GPU"),
        "the flag reached the params: {error:?}"
    );
    let plain =
        orator_voice::stt::WhisperBackend::new(std::path::PathBuf::from("/nonexistent/ggml.bin"), "en");
    let error = plain
        .decode_blocking(&[0.0; 320], &StreamAbort::new())
        .expect_err("missing model must fail");
    assert!(
        error.to_string().contains("missing"),
        "the default path stays CPU: {error:?}"
    );
}

#[test]
fn daemon_listen_keys_round_trip() {
    let mut config = OratorConfig::default();
    for key in ["daemon.ws_listen", "daemon.ws_token"] {
        assert!(
            config_keys().contains(&key.to_string()),
            "{key} is settable"
        );
    }
    config_set(&mut config, "daemon.ws_listen", "127.0.0.1:18081").expect("valid set");
    assert_eq!(
        config_get(&config, "daemon.ws_listen").expect("readable"),
        "127.0.0.1:18081"
    );
    config_set(&mut config, "daemon.ws_token", "secret").expect("valid set");
    assert_eq!(
        config_get(&config, "daemon.ws_token").expect("readable"),
        "secret"
    );
    config_set(&mut config, "daemon.admission_max", "4").expect("valid set");
    assert_eq!(
        config_get(&config, "daemon.admission_max").expect("readable"),
        "4"
    );
    let error =
        config_set(&mut config, "daemon.admission_max", "0").expect_err("below-minimum refuses");
    assert!(error.contains("admission_max"), "{error}");
}

/// Endpoint leaves refuse garbage at set-time with the shape named;
/// valid shapes still round-trip. The dialers parse through the same
/// grammar, so set-time and use-time cannot disagree.
#[test]
fn endpoint_leaves_validate_at_set_time() {
    use crate::config::s2s_base_url;
    let mut config = OratorConfig::default();
    for key in ["llm.endpoint", "s2s.endpoint"] {
        for garbage in ["not a host!!", "", "host:abc", "127.0.0.1:"] {
            let error = config_set(&mut config, key, garbage).expect_err("garbage refuses");
            assert!(error.contains("host:port"), "{key} {garbage:?}: {error}");
        }
    }
    // Untouched by refusal: shipped values still hold.
    assert_eq!(config.llm.endpoint, OratorConfig::default().llm.endpoint);
    config_set(&mut config, "llm.endpoint", "10.9.9.9:8080").expect("bare host:port");
    assert_eq!(
        config_get(&config, "llm.endpoint").expect("readable"),
        "10.9.9.9:8080"
    );
    config_set(&mut config, "s2s.endpoint", "http://127.0.0.1:18081").expect("schemeful");
    assert_eq!(
        s2s_base_url(&config.s2s),
        "http://127.0.0.1:18081",
        "valid file values render one base URL"
    );
}

/// Every key `config_keys()` advertises round-trips its own value:
/// get, set the same rendering back, get again byte-identical. This
/// pins the whole surface (including future keys — the loop follows
/// `config_keys()`, so a new leaf without get/set support fails
/// here, not in a user's file).
#[test]
fn every_advertised_key_round_trips_its_own_value() {
    let mut config = OratorConfig::default();
    let keys = config_keys();
    assert!(keys.len() > 20, "surface covers every section: {keys:?}");
    for section in [
        "audio.", "stt.", "llm.", "tts.", "s2s.", "turn.", "vad.", "chat.", "daemon.",
    ] {
        assert!(
            keys.iter().any(|key| key.starts_with(section)),
            "section {section} has keys: {keys:?}"
        );
    }
    for key in &keys {
        let before = config_get(&config, key).unwrap_or_else(|_| panic!("{key} reads"));
        config_set(&mut config, key, &before)
            .unwrap_or_else(|_| panic!("{key} writes its own rendering {before:?}"));
        let after = config_get(&config, key).unwrap_or_else(|_| panic!("{key} re-reads"));
        if matches!(key.as_str(), "models" | "roles") {
            // Tables serialize from a map: key order is not stable
            // across a parse round-trip, so compare semantically.
            let before_json: serde_json::Value =
                serde_json::from_str(&before).expect("tables render JSON");
            let after_json: serde_json::Value =
                serde_json::from_str(&after).expect("tables re-render JSON");
            assert_eq!(before_json, after_json, "{key} round-trips");
        } else {
            assert_eq!(before, after, "{key} round-trips");
        }
    }
    assert_eq!(
        config,
        OratorConfig::default(),
        "self round-trip changes nothing"
    );
}

/// A `roles` table carrying `chat` round-trips through get/set and
/// resolves the chat head (old tables without `chat` keep loading —
/// the field defaults to the llm-default head).
#[test]
fn roles_chat_round_trips_and_resolves() {
    use crate::config::{config_get, config_set};
    let mut config = OratorConfig::default();
    config_set(
        &mut config,
        "roles",
        r#"{"asr":{"models":["liquidai/lfm3-audio-2b"]},"tts":{"models":["liquidai/lfm3-audio-2b"]},"chat":{"models":["chat-probe"],"params":{"ctx":4096}}}"#,
    )
    .expect("roles with chat settable");
    assert_eq!(
        orator::resolve_role_model(&config.roles, "chat").as_deref(),
        Some("chat-probe")
    );
    let back = config_get(&config, "roles").expect("roles readable");
    let roles: orator::RolesConfig = serde_json::from_str(&back).expect("roles are JSON");
    assert_eq!(
        orator::resolve_role_model(&roles, "chat").as_deref(),
        Some("chat-probe"),
        "chat survives the round-trip"
    );
    // Old tables without chat load today's chat behavior.
    let legacy: orator::RolesConfig =
        serde_json::from_str(r#"{"asr":{"models":["x"]},"tts":{"models":["y"]}}"#)
            .expect("legacy roles parse");
    assert_eq!(
        orator::resolve_role_model(&legacy, "chat").as_deref(),
        Some(orator::DEFAULT_LLM_MODEL),
        "missing chat defaults to the llm-default head"
    );
}

/// Top-level gguf dir and whole-table models/roles round-trip through
/// get/set; garbage tables refuse without touching the file state.
#[test]
fn gguf_models_roles_round_trip_as_json() {
    use crate::config::{config_get, config_keys, config_set};
    let mut config = OratorConfig::default();
    assert_eq!(
        config_get(&config, "gguf_dir").expect("readable"),
        orator::DEFAULT_GGUF_DIR
    );
    config_set(&mut config, "gguf_dir", "/app/ai/models/gguf").expect("settable");
    assert_eq!(
        config_get(&config, "gguf_dir").expect("readable"),
        "/app/ai/models/gguf"
    );
    assert!(config_keys().contains(&"gguf_dir".to_string()));
    assert!(config_keys().contains(&"default".to_string()));
    assert!(config_keys().contains(&"models".to_string()));
    assert!(config_keys().contains(&"roles".to_string()));
    assert_eq!(
        config_get(&config, "default").expect("readable"),
        orator::DEFAULT_MODEL
    );
    config_set(&mut config, "default", "liquidai/lfm3-audio-2b").expect("settable");
    assert_eq!(
        config_get(&config, "default").expect("readable"),
        "liquidai/lfm3-audio-2b"
    );
    // Whole-table get parses back to the same tables.
    let models_json = config_get(&config, "models").expect("models readable");
    let models: orator::ModelsConfig =
        serde_json::from_str(&models_json).expect("models are JSON");
    assert!(models.models.contains_key(orator::DEFAULT_TTS_MODEL));
    let roles_json = config_get(&config, "roles").expect("roles readable");
    let roles: orator::RolesConfig =
        serde_json::from_str(&roles_json).expect("roles are JSON");
    assert_eq!(
        orator::resolve_role_model(&roles, "asr").as_deref(),
        Some(orator::DEFAULT_ASR_MODEL)
    );
    // A custom roles table round-trips and resolves.
    config_set(
        &mut config,
        "roles",
        r#"{"asr":{"models":["liquidai/lfm3-audio-2b"]},"tts":{"models":["liquidai/lfm3-audio-2b"]}}"#,
    )
    .expect("roles settable");
    assert_eq!(
        orator::resolve_role_model(&config.roles, "asr").as_deref(),
        Some("liquidai/lfm3-audio-2b")
    );
    // Garbage refuses with the shape named; state untouched.
    let before = config.clone();
    for (key, value) in [("models", "{oops"), ("roles", "[1,2]")] {
        let error = config_set(&mut config, key, value).expect_err("garbage refuses");
        assert!(error.contains("JSON"), "{error}");
    }
    assert_eq!(config, before, "failed sets change nothing");
    // Unknown top-level key still refuses.
    config_set(&mut config, "gguf", "/x").expect_err("section.field required");
}

/// M7-S1 characterization: the advertised key surface, pinned exactly.
/// `config_keys()` order feeds the `allowed keys` refusal strings, so
/// additions, removals, and reorderings are all behavior changes.
#[test]
fn config_keys_pins_the_advertised_surface() {
    use crate::config::config_keys;
    let keys = config_keys();
    let mut expected = vec![
        "gguf_dir".to_string(),
        "default".to_string(),
        "models".to_string(),
        "roles".to_string(),
    ];
    for (section, leaves) in [
        ("audio", ["backend", "mic"].as_slice()),
        ("stt", ["backend", "model", "language", "gpu"].as_slice()),
        (
            "llm",
            [
                "endpoint",
                "model",
                "ctx",
                "n_batch",
                "weights",
                "gguf_dir",
                "gpu",
                "auto_boot",
            ]
            .as_slice(),
        ),
        ("tts", ["backend", "voice", "gpu"].as_slice()),
        (
            "s2s",
            [
                "backend",
                "mode",
                "endpoint",
                "model",
                "mmproj",
                "vocoder",
                "tokenizer",
                "voice_prompt",
                "turn_window_ms",
                "max_tokens",
                "max_audio_samples",
                "auto_boot",
                "gpu",
            ]
            .as_slice(),
        ),
        (
            "turn",
            [
                "hold_min_ms",
                "hold_max_ms",
                "barge_in_ms",
                "backchannel_ms",
                "tail_hold_ms",
                "poll_ms",
                "saturated_cooldown_secs",
                "shed_notice_secs",
                "duck_ramp_ms",
                "cut_fade_ms",
                "duck_depth_db",
            ]
            .as_slice(),
        ),
        (
            "vad",
            [
                "floor",
                "hysteresis",
                "attack_frames",
                "hangover_frames",
                "max_zero_crossing",
                "silence_rms",
                "gate_floor",
                "min_speech_fraction",
                "rearm_frames",
                "min_tone_intervals",
                "tone_max_cv",
            ]
            .as_slice(),
        ),
        (
            "chat",
            ["voice", "text", "mic", "barge", "ptt", "narration"].as_slice(),
        ),
        (
            "daemon",
            [
                "ws_listen",
                "ws_token",
                "admission_max",
                "ws_ping_secs",
                "ws_inactivity_secs",
                "ws_cap_secs",
            ]
            .as_slice(),
        ),
    ] {
        for leaf in leaves {
            expected.push(format!("{section}.{leaf}"));
        }
    }
    assert_eq!(keys, expected);
}
