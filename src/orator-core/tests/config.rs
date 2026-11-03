//! Unit tests for [`crate::config`]: documented defaults, field access.

use fluent_wvr::FieldAccess;

use crate::config::{AudioConfig, DaemonConfig, LlmEndpointConfig, OratorConfig, S2sConfig, SttConfig, TtsConfig, TurnConfig};

#[test]
fn canonical_paths_and_weights_resolution() {
    assert!(
        crate::config::config_path()
            .as_ref()
            .is_some_and(|path| path.ends_with("orator/orator.json"))
    );
    assert!(
        crate::config::config_dir()
            .as_ref()
            .is_some_and(|dir| dir.ends_with("orator"))
    );
    // Explicit declarations win without touching the filesystem.
    let absolute = LlmEndpointConfig {
        weights: "/weights/qwen.gguf".to_string(),
        ..LlmEndpointConfig::default()
    };
    assert_eq!(
        crate::config::resolve_llm_weights(&absolute),
        Some(std::path::PathBuf::from("/weights/qwen.gguf"))
    );
    let relative = LlmEndpointConfig {
        weights: "org/model.gguf".to_string(),
        gguf_dir: "/weights".to_string(),
        ..LlmEndpointConfig::default()
    };
    assert_eq!(
        crate::config::resolve_llm_weights(&relative),
        Some(std::path::PathBuf::from("/weights/org/model.gguf"))
    );
    // Nothing declares this tag and no such file exists: no location.
    let missing = LlmEndpointConfig {
        model: "orator-definitely-missing-model".to_string(),
        weights: String::new(),
        gguf_dir: String::new(),
        ..LlmEndpointConfig::default()
    };
    assert_eq!(crate::config::resolve_llm_weights(&missing), None);
}

#[test]
fn home_dir_prefers_home_then_data_dir() {
    let _guard = common_core::sync::lock(&crate::ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = tempfile::tempdir().expect("tempdir");
    let data = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::set_var("XDG_DATA_HOME", data.path());
    assert_eq!(crate::paths::home_dir(), Some(home.path().to_path_buf()));
    std::env::remove_var("HOME");
    assert_eq!(crate::paths::home_dir(), Some(data.path().to_path_buf()));
    std::env::remove_var("XDG_DATA_HOME");
    assert_eq!(crate::paths::home_dir(), None);
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

#[test]
fn home_dir_skips_relative_values() {
    let _guard = common_core::sync::lock(&crate::ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    std::env::set_var("HOME", "relative/home");
    std::env::remove_var("XDG_DATA_HOME");
    assert_eq!(crate::paths::home_dir(), None);
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

#[test]
fn managed_dirs_are_absolute_or_absent() {
    let _guard = common_core::sync::lock(&crate::ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    for dir in [crate::config::models_dir(), crate::config::config_dir()] {
        let dir = dir.expect("home base resolves");
        assert!(dir.is_absolute(), "never relative: {}", dir.display());
    }
    let path = crate::config::config_path().expect("home base resolves");
    assert!(path.is_absolute(), "never relative: {}", path.display());
    assert!(path.ends_with("orator/orator.json"));
    std::env::remove_var("HOME");
    assert_eq!(crate::config::models_dir(), None);
    assert_eq!(crate::config::config_dir(), None);
    assert_eq!(crate::config::config_path(), None);
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

#[test]
fn within_root_rejects_symlink_escape() {
    let root = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("tempdir");
    let real = root.path().join("weights.gguf");
    std::fs::write(&real, b"stub").expect("stub writes");
    assert!(crate::paths::within_root(root.path(), &real));
    let escaped = outside.path().join("evil.gguf");
    std::fs::write(&escaped, b"stub").expect("stub writes");
    #[cfg(unix)]
    {
        let link = root.path().join("linked.gguf");
        std::os::unix::fs::symlink(&escaped, &link).expect("symlink");
        assert!(
            !crate::paths::within_root(root.path(), &link),
            "canonical escape is rejected"
        );
    }
    assert!(
        !crate::paths::within_root(root.path(), &root.path().join("missing.gguf")),
        "missing paths fail closed"
    );
}

#[test]
fn within_root_caps_symlink_hops_and_rejects_tmp() {
    let root = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside");
    let target = outside.path().join("evil.gguf");
    std::fs::write(&target, b"stub").expect("stub writes");
    #[cfg(unix)]
    {
        // Chain longer than the hop cap never resolves inside.
        let mut prev = target.clone();
        for depth in 0..50 {
            let link = root.path().join(format!("hop-{depth}"));
            std::os::unix::fs::symlink(&prev, &link).expect("chain link");
            prev = link;
        }
        assert!(
            !crate::paths::within_root(root.path(), &prev),
            "hop-capped chase refuses deep chains"
        );
        // Loop never resolves inside.
        let dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink("loop-b", dir.path().join("loop-a")).expect("loop a");
        std::os::unix::fs::symlink("loop-a", dir.path().join("loop-b")).expect("loop b");
        assert!(
            !crate::paths::within_root(dir.path(), &dir.path().join("loop-a")),
            "symlink loop fails closed"
        );
    }
    // Component-prefix /tmp check: /tmpfoo is fine, /tmp/evil is not
    // trusted even when it exists.
    let tmp_evil = std::path::PathBuf::from("/tmp/orator-redteam-guard-probe");
    assert!(
        !crate::paths::within_root(&tmp_evil, &tmp_evil.join("x")),
        "missing under /tmp still fails closed"
    );
}

#[test]
fn server_binary_gate_requires_absolute_trusted_paths() {
    use crate::paths::{current_uid, validate_server_binary};
    let uid = current_uid();
    // Relative paths refuse (explicit bypass is the only override).
    assert!(
        validate_server_binary(std::path::Path::new("relative/llama-server"), uid, false)
            .is_err(),
        "relative binary paths refuse"
    );
    // /tmp-rooted paths refuse without an explicit override.
    assert!(
        validate_server_binary(std::path::Path::new("/tmp/llama-server"), uid, false)
            .is_err(),
        "/tmp binaries refuse without explicit override"
    );
    assert!(
        validate_server_binary(
            std::path::Path::new("/tmp/llama-server"),
            uid,
            true
        )
        .is_ok(),
        "explicit override accepts responsibility"
    );
    // Missing files refuse loudly.
    assert!(
        validate_server_binary(
            std::path::Path::new("/nonexistent-orator/llama-server"),
            uid,
            false
        )
        .is_err(),
        "missing binaries refuse"
    );
}

#[test]
fn reference_bundle_names_all_components() {
    let bundle = crate::config::default_bundle();
    assert!(bundle.backbone.ends_with("latest.gguf"));
    assert!(bundle.mmproj.ends_with("mmproj.gguf"));
    assert!(bundle.vocoder.ends_with("vocoder.gguf"));
    assert!(bundle.tokenizer.ends_with("tokenizer.gguf"));
    // Presence reflects disk truth, never panics.
    let _ = bundle.present();
    let _ = bundle.missing().len();
}

#[test]
fn bundle_presence_tracks_the_filesystem() {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["b.gguf", "m.gguf", "v.gguf", "t.gguf"] {
        std::fs::write(dir.path().join(name), b"stub").expect("stub writes");
    }
    let full = crate::config::AudioBundle {
        backbone: dir.path().join("b.gguf"),
        mmproj: dir.path().join("m.gguf"),
        vocoder: dir.path().join("v.gguf"),
        tokenizer: dir.path().join("t.gguf"),
    };
    assert!(full.present());
    assert!(full.missing().is_empty());
    // Legacy companions are optional: LFM3 voices from backbone +
    // projector alone, so dropping them never un-presents the bundle.
    std::fs::remove_file(dir.path().join("v.gguf")).expect("remove");
    std::fs::remove_file(dir.path().join("t.gguf")).expect("remove");
    assert!(full.present());
    assert!(full.missing().is_empty());
    assert_eq!(full.missing_optional().len(), 2);
    // Required components still gate presence.
    std::fs::remove_file(dir.path().join("m.gguf")).expect("remove");
    assert!(!full.present());
    assert_eq!(full.missing(), vec![dir.path().join("m.gguf")]);
}

#[test]
fn defaults_match_the_documented_values() {
    let audio = AudioConfig::default();
    assert_eq!(audio.backend, "cpal");
    assert_eq!(audio.mic, "default");

    let stt = SttConfig::default();
    assert_eq!(stt.backend, "audio-server");
    assert_eq!(stt.language, "en");
    assert_eq!(stt.model, "large-v3-turbo-q5_0");
    assert_eq!(
        SttConfig::new()
            .backend("whisper".to_string())
            .model("large-v3-turbo-q5_0".to_string())
            .language("en".to_string())
            .build()
            .model,
        "large-v3-turbo-q5_0"
    );

    let llm = LlmEndpointConfig::default();
    assert_eq!(llm.endpoint, "127.0.0.1:8080");
    assert_eq!(llm.ctx, 8192);
    assert_eq!(llm.n_batch, 512);
    assert_eq!(llm.model, "bartowski/lfm3-1.7b");
    assert_eq!(llm.weights, "bartowski/lfm3-1.7b/latest.gguf");
    assert_eq!(llm.gguf_dir, "/ai/models/gguf");

    let tts = TtsConfig::default();
    assert_eq!(tts.backend, "audio-server");
    assert_eq!(tts.voice, "bf_emma");

    // The llama.cpp audio endpoint ships in the file (default port
    // 8080, same loopback default as the text endpoint).
    let s2s = S2sConfig::default();
    assert_eq!(s2s.backend, "audio-server");
    assert_eq!(s2s.endpoint, "http://127.0.0.1:8080");
    assert_eq!(s2s.model, "liquidai/lfm3-audio-2b");
    assert_eq!(s2s.turn_window_ms, 2000);

    // GPU offload is explicit opt-in everywhere: defaults stay CPU.
    assert!(!stt.gpu);
    assert!(!llm.gpu);
    assert!(!tts.gpu);

    let turn = TurnConfig::default();
    assert_eq!(turn.hold_min_ms, 300);
    assert_eq!(turn.hold_max_ms, 700);
    assert_eq!(turn.barge_in_ms, 150);

    // The socket is the only listener unless configured.
    let daemon = DaemonConfig::default();
    assert!(daemon.ws_listen.is_empty());
    assert!(daemon.ws_token.is_empty());
    // The admission bound ships calibrated, not open-ended.
    assert_eq!(daemon.admission_max, 2);
}

#[test]
fn root_default_composes_the_leaves() {
    let root = OratorConfig::default();
    assert_eq!(root.audio.backend, AudioConfig::default().backend);
    assert_eq!(root.turn.barge_in_ms, TurnConfig::default().barge_in_ms);
    assert!(root.daemon.ws_listen.is_empty());
}

#[test]
fn daemon_listen_round_trips_and_old_files_stay_socket_only() {
    let mut daemon = DaemonConfig::default();
    daemon
        .set_field("ws_listen", "127.0.0.1:18081")
        .expect("valid set");
    assert_eq!(
        daemon.get_field("ws_listen").expect("readable"),
        "127.0.0.1:18081"
    );
    daemon.set_field("ws_token", "secret").expect("valid set");
    assert!(daemon.field_names().contains(&"ws_listen"));
    // The bound round-trips and rejects out-of-band values.
    daemon.set_field("admission_max", "8").expect("valid set");
    assert_eq!(daemon.get_field("admission_max").expect("readable"), "8");
    daemon
        .set_field("admission_max", "0")
        .expect_err("below-minimum refuses");
    daemon
        .set_field("admission_max", "65")
        .expect_err("above-maximum refuses");
    daemon
        .set_field("admission_max", "many")
        .expect_err("non-numeric refuses");
    // A file written before the section existed loads socket-only.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"tts": {"backend": "kokoro", "voice": "brass"}}"#)
        .expect("write fixture");
    let loaded: OratorConfig = common_core::config::load_json(&path).expect("old schema loads");
    assert!(loaded.daemon.ws_listen.is_empty());
    assert!(loaded.daemon.ws_token.is_empty());
    // A file written before the bound existed loads calibrated, never zero.
    assert_eq!(loaded.daemon.admission_max, 2);
}

#[test]
fn ws_liveness_leaves_default_to_moshi_numbers_and_reject_garbage() {
    let daemon = DaemonConfig::default();
    assert_eq!(daemon.ws_ping_secs, 5);
    assert_eq!(daemon.ws_inactivity_secs, 10);
    assert_eq!(daemon.ws_cap_secs, 360);
    let mut tuned = DaemonConfig::default();
    tuned.set_field("ws_ping_secs", "5").expect("valid set");
    tuned.set_field("ws_inactivity_secs", "10").expect("valid set");
    tuned.set_field("ws_cap_secs", "0").expect("zero disables the cap");
    assert_eq!(tuned.get_field("ws_cap_secs").expect("readable"), "0");
    tuned
        .set_field("ws_ping_secs", "0")
        .expect_err("below-minimum refuses");
    tuned
        .set_field("ws_inactivity_secs", "many")
        .expect_err("non-numeric refuses");
    // Old files without the leaves load the calibrated defaults.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"daemon": {"admission_max": 2}}"#).expect("write fixture");
    let loaded: OratorConfig = common_core::config::load_json(&path).expect("old schema loads");
    assert_eq!(loaded.daemon.ws_ping_secs, 5);
    assert_eq!(loaded.daemon.ws_inactivity_secs, 10);
    assert_eq!(loaded.daemon.ws_cap_secs, 360);
}

#[test]
fn fields_round_trip_by_name() {
    let mut turn = TurnConfig::default();
    turn.set_field("barge_in_ms", "200").expect("valid set");
    assert_eq!(turn.get_field("barge_in_ms").expect("valid get"), "200");
    assert!(turn.field_names().contains(&"barge_in_ms"));
}

#[test]
fn constraints_reject_out_of_band_values() {
    let mut turn = TurnConfig::default();
    turn.set_field("hold_min_ms", "10")
        .expect_err("below-minimum hold must fail");
    turn.set_field("nope", "1")
        .expect_err("unknown field must fail");
}

#[test]
fn gpu_leaves_round_trip_and_reject_garbage() {
    let mut stt = SttConfig::default();
    stt.set_field("gpu", "true").expect("true parses");
    assert!(stt.gpu);
    assert_eq!(stt.get_field("gpu").expect("readable"), "true");
    stt.set_field("gpu", "false").expect("false parses");
    assert!(!stt.gpu);
    stt.set_field("gpu", "maybe")
        .expect_err("garbage must fail");
    assert!(stt.field_names().contains(&"gpu"));

    let mut llm = LlmEndpointConfig::default();
    llm.set_field("gpu", "true").expect("true parses");
    assert!(llm.gpu);

    let mut tts = TtsConfig::default();
    tts.set_field("gpu", "true").expect("true parses");
    assert!(tts.gpu);
    tts.set_field("gpu", "false").expect("false parses");
    assert!(!tts.gpu);
}

#[test]
fn config_survives_json_round_trip() {
    let root = OratorConfig::default();
    let json = serde_json::to_string(&root).expect("serializable");
    let back: OratorConfig = serde_json::from_str(&json).expect("deserializable");
    assert_eq!(back.audio.backend, root.audio.backend);
    assert_eq!(back.turn.hold_max_ms, root.turn.hold_max_ms);
    assert_eq!(back.stt.gpu, root.stt.gpu);
    assert_eq!(back.llm.gpu, root.llm.gpu);
    assert_eq!(back.tts.gpu, root.tts.gpu);
}

/// Files written before the GPU leaves existed load with their values
/// intact and the leaves off: schema growth must never reset an
/// operator's backend selection.
#[test]
fn pre_gpu_schema_files_load_with_gpu_off() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        r#"{
            "stt": {"backend": "whisper", "model": "m", "language": "en"},
            "llm": {"endpoint": "127.0.0.1:8080", "model": "m", "ctx": 8192, "n_batch": 512, "weights": "w", "gguf_dir": "g"},
            "tts": {"backend": "kokoro", "voice": "brass"}
        }"#,
    )
    .expect("write fixture");
    let loaded: OratorConfig = common_core::config::load_json(&path).expect("old schema loads");
    assert_eq!(loaded.stt.backend, "whisper");
    assert_eq!(loaded.tts.voice, "brass");
    assert!(!loaded.stt.gpu);
    assert!(!loaded.llm.gpu);
    assert!(!loaded.tts.gpu);
}

#[test]
fn removed_audio_keys_are_detected_by_name() {
    use crate::config::removed_audio_keys;
    let dir = tempfile::tempdir().expect("tempdir");
    // Clean configs pass silently.
    let clean = dir.path().join("clean.json");
    std::fs::write(&clean, r#"{"audio": {"backend": "cpal"}}"#).expect("writes");
    assert!(removed_audio_keys(&clean).is_empty());
    // Removed keys are named, never silently absorbed.
    let legacy = dir.path().join("legacy.json");
    std::fs::write(
        &legacy,
        r#"{"audio": {"system_source": "x", "echo_cancel": "auto"}}"#,
    )
    .expect("writes");
    let mut found = removed_audio_keys(&legacy);
    found.sort();
    assert_eq!(found, vec!["echo_cancel".to_string(), "system_source".to_string()]);
    // Missing files are not removals (defaults apply).
    assert!(removed_audio_keys(&dir.path().join("absent.json")).is_empty());
}

#[test]
fn s2s_section_loads_from_file_and_old_files_stay_port_8080() {
    // A file with an s2s section round-trips the llama.cpp endpoint.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        r#"{"s2s": {"endpoint": "http://127.0.0.1:18080", "model": "custom-voice"}}"#,
    )
    .expect("write fixture");
    let loaded: OratorConfig = common_core::config::load_json(&path).expect("s2s schema loads");
    assert_eq!(loaded.s2s.endpoint, "http://127.0.0.1:18080");
    assert_eq!(loaded.s2s.model, "custom-voice");
    // Unset leaves fall back to the builder values (never empty/zero).
    assert_eq!(loaded.s2s.backend, S2sConfig::default().backend);
    assert_eq!(loaded.s2s.turn_window_ms, 2000);
    let bare_path = dir.path().join("bare.json");
    std::fs::write(&bare_path, r#"{"s2s": {}}"#).expect("write fixture");
    let bare: OratorConfig = common_core::config::load_json(&bare_path).expect("empty section loads");
    assert_eq!(bare.s2s, S2sConfig::default());
    // Files written before the section existed load the port-8080 default.
    let legacy = dir.path().join("legacy.json");
    std::fs::write(&legacy, r#"{"daemon": {"admission_max": 2}}"#).expect("write fixture");
    let old: OratorConfig = common_core::config::load_json(&legacy).expect("old schema loads");
    assert_eq!(old.s2s, S2sConfig::default());
    assert_eq!(old.s2s.endpoint, "http://127.0.0.1:8080");
}

#[test]
fn bundle_from_s2s_pins_the_backbone_and_honors_component_paths() {
    let mut s2s = S2sConfig::default();
    let bundle = crate::config::bundle_from_s2s(&s2s);
    assert_eq!(bundle.backbone, crate::config::default_bundle().backbone);
    assert!(bundle.mmproj.ends_with("mmproj.gguf"));
    s2s.mmproj = "/voices/custom-mmproj.gguf".to_string();
    let custom = crate::config::bundle_from_s2s(&s2s);
    assert_eq!(
        custom.mmproj,
        std::path::PathBuf::from("/voices/custom-mmproj.gguf")
    );
    assert_eq!(custom.backbone, bundle.backbone, "backbone stays pinned");
}
