//! Configuration helper tests: backend resolution, dotted keys,
//! env overrides — all hermetic, no binary, no socket.

use orator_core::{LlmEndpointConfig, OratorConfig};

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
    assert_eq!(config_get(&config, "tts.backend").expect("valid get"), "kokoro");
    config_set(&mut config, "turn.barge_in_ms", "200").expect("valid set");
    assert_eq!(config_get(&config, "turn.barge_in_ms").expect("valid get"), "200");
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
        assert!(config_keys().contains(&key.to_string()), "{key} is settable");
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
            orator_llm::GPU_OFFLOAD_LAYERS.to_string()
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
    if orator_core::gpu::device_present() {
        eprintln!("SKIP gpu-flag-reaches-params: GPU device present, nothing to refuse");
        return;
    }
    let backend =
        orator_stt::WhisperBackend::new(std::path::PathBuf::from("/nonexistent/ggml.bin"), "en")
            .with_gpu();
    let error = backend
        .decode_blocking(&[0.0; 320], &StreamAbort::new())
        .expect_err("GPU without a device must fail");
    assert!(
        error.to_string().contains("GPU"),
        "the flag reached the params: {error:?}"
    );
    let plain =
        orator_stt::WhisperBackend::new(std::path::PathBuf::from("/nonexistent/ggml.bin"), "en");
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
        assert!(config_keys().contains(&key.to_string()), "{key} is settable");
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
    let error = config_set(&mut config, "daemon.admission_max", "0")
        .expect_err("below-minimum refuses");
    assert!(error.contains("admission_max"), "{error}");
}
