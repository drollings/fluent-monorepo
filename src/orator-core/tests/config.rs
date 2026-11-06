//! Unit tests for [`crate::config`]: documented defaults, field access.

use fluent_wvr::FieldAccess;

use crate::config::{
    AudioConfig, DaemonConfig, LlmEndpointConfig, OratorConfig, S2sConfig, SttConfig, TtsConfig,
    TurnConfig,
};

#[test]
fn canonical_paths_and_weights_resolution() {
    assert!(crate::config::config_path()
        .as_ref()
        .is_some_and(|path| path.ends_with("orator/orator.json")));
    assert!(crate::config::config_dir()
        .as_ref()
        .is_some_and(|dir| dir.ends_with("orator")));
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
    for dir in [
        crate::config::models_dir(),
        crate::config::meetings_dir(),
        crate::config::config_dir(),
    ] {
        let dir = dir.expect("home base resolves");
        assert!(dir.is_absolute(), "never relative: {}", dir.display());
    }
    let path = crate::config::config_path().expect("home base resolves");
    assert!(path.is_absolute(), "never relative: {}", path.display());
    assert!(path.ends_with("orator/orator.json"));
    std::env::remove_var("HOME");
    assert_eq!(crate::config::models_dir(), None);
    assert_eq!(crate::config::meetings_dir(), None);
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
        validate_server_binary(std::path::Path::new("relative/llama-server"), uid, false).is_err(),
        "relative binary paths refuse"
    );
    // /tmp-rooted paths refuse without an explicit override.
    assert!(
        validate_server_binary(std::path::Path::new("/tmp/llama-server"), uid, false).is_err(),
        "/tmp binaries refuse without explicit override"
    );
    assert!(
        validate_server_binary(std::path::Path::new("/tmp/llama-server"), uid, true).is_ok(),
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
    assert_eq!(stt.model, crate::config::DEFAULT_ASR_MODEL);
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
    assert_eq!(llm.gguf_dir, crate::config::DEFAULT_GGUF_DIR);

    let tts = TtsConfig::default();
    assert_eq!(tts.backend, "kokoro");
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
    tuned
        .set_field("ws_inactivity_secs", "10")
        .expect("valid set");
    tuned
        .set_field("ws_cap_secs", "0")
        .expect("zero disables the cap");
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
    assert_eq!(
        found,
        vec!["echo_cancel".to_string(), "system_source".to_string()]
    );
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
    let bare: OratorConfig =
        common_core::config::load_json(&bare_path).expect("empty section loads");
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
    use crate::config::ModelsConfig;
    let mut s2s = S2sConfig::default();
    let models = ModelsConfig::default();
    let bundle = crate::config::bundle_from_s2s(&s2s, &models, crate::config::DEFAULT_GGUF_DIR);
    assert_eq!(bundle.backbone, crate::config::default_bundle().backbone);
    assert!(bundle.mmproj.ends_with("mmproj.gguf"));
    // Relative declarations join under gguf_dir (never repeated);
    // absolute ones pass through verbatim.
    s2s.mmproj = "custom/custom-mmproj.gguf".to_string();
    let joined = crate::config::bundle_from_s2s(&s2s, &models, "/srv/weights");
    assert_eq!(
        joined.mmproj,
        std::path::PathBuf::from("/srv/weights/custom/custom-mmproj.gguf")
    );
    s2s.mmproj = "/voices/custom-mmproj.gguf".to_string();
    let custom = crate::config::bundle_from_s2s(&s2s, &models, crate::config::DEFAULT_GGUF_DIR);
    assert_eq!(
        custom.mmproj,
        std::path::PathBuf::from("/voices/custom-mmproj.gguf")
    );
    assert_eq!(custom.backbone, bundle.backbone, "backbone stays pinned");
    // The served model tag resolves its weights through the models
    // table instead of the pinned reference.
    let mut tabled = ModelsConfig::default();
    tabled
        .models
        .get_mut("liquidai/lfm3-audio-2b")
        .expect("LFM3 listed")
        .weights = Some("custom/audio.gguf".to_string());
    let routed = crate::config::bundle_from_s2s(&s2s, &tabled, crate::config::DEFAULT_GGUF_DIR);
    assert_eq!(
        routed.backbone,
        std::path::PathBuf::from(format!(
            "{}/custom/audio.gguf",
            crate::config::DEFAULT_GGUF_DIR
        ))
    );
}

#[test]
fn bundle_paths_resolve_router_style() {
    use crate::config::resolve_bundle_path;
    assert_eq!(
        resolve_bundle_path("a/b.gguf", "/srv/weights"),
        std::path::PathBuf::from("/srv/weights/a/b.gguf"),
        "relative joins under the root"
    );
    assert_eq!(
        resolve_bundle_path("/abs/b.gguf", "/srv/weights"),
        std::path::PathBuf::from("/abs/b.gguf"),
        "absolute passes through"
    );
    assert!(
        resolve_bundle_path("", "/srv/weights")
            .as_os_str()
            .is_empty(),
        "empty stays empty"
    );
}

/// DRY characterization: the shipped `s2s` component leaves are
/// relative (the root appears once, in `gguf_dir`), so a relocated
/// store moves the whole bundle. Absolute values keep passing
/// through verbatim (user override, not the default shape).
#[test]
fn default_bundle_leaves_stay_relative_to_the_root() {
    use crate::config::{S2sConfig, DEFAULT_GGUF_DIR};
    let s2s = S2sConfig::default();
    for leaf in [&s2s.mmproj, &s2s.vocoder, &s2s.tokenizer] {
        assert!(
            std::path::Path::new(leaf).is_relative(),
            "default leaf stays relative: {leaf}"
        );
        assert!(
            !leaf.contains(DEFAULT_GGUF_DIR),
            "default leaf never repeats the root: {leaf}"
        );
    }
    let bundle = crate::config::bundle_from_s2s(
        &s2s,
        &crate::config::ModelsConfig::default(),
        DEFAULT_GGUF_DIR,
    );
    assert_eq!(
        bundle.mmproj,
        std::path::PathBuf::from(DEFAULT_GGUF_DIR).join(&s2s.mmproj),
        "relative leaves join under the configured root"
    );
}

/// One endpoint grammar: shipped defaults parse, garbage refuses with
/// the shape named. The runtime parsers and `config set` share this
/// exact function, so set-time and use-time never disagree.
#[test]
fn endpoint_grammar_accepts_shipped_shapes_and_refuses_garbage() {
    use crate::config::parse_endpoint;
    // Control: every endpoint in the checked-in default config parses.
    let defaults = OratorConfig::default();
    for endpoint in [&defaults.llm.endpoint, &defaults.s2s.endpoint] {
        parse_endpoint(endpoint).expect("shipped default parses");
    }
    let ok = parse_endpoint("127.0.0.1:8080").expect("bare host:port");
    assert_eq!(ok.port, 8080);
    assert_eq!(ok.base_url(), "http://127.0.0.1:8080");
    let ok = parse_endpoint("http://127.0.0.1:18081").expect("schemeful");
    assert_eq!(ok.port, 18081);
    assert_eq!(ok.base_url(), "http://127.0.0.1:18081");
    let ok = parse_endpoint("https://example.com:443").expect("https keeps its scheme");
    assert_eq!(ok.base_url(), "https://example.com:443");
    let ok = parse_endpoint("[::1]:8080").expect("bracketed IPv6");
    assert_eq!(ok.port, 8080);
    assert_eq!(ok.base_url(), "http://[::1]:8080");
    // Explicit zero stays legal (supervisor auto-reserve shape).
    assert_eq!(parse_endpoint("127.0.0.1:0").expect("zero port").port, 0);
    for garbage in [
        "not a host!!",
        "",
        ":99999",
        "host:abc",
        "garbage",
        "127.0.0.1:",
        "http://127.0.0.1",
        "ftp://127.0.0.1:21",
        "ho st:8080",
        "[::1]",
        "::1",
    ] {
        let error = parse_endpoint(garbage).expect_err("garbage refuses");
        assert!(
            error.contains("host:port"),
            "shape named for {garbage:?}: {error}"
        );
    }
}

/// The models table ships the integrated model plus the specialized
/// ASR/TTS models, each naming its serving backend. Membership lives
/// on the role side: models never name roles back.
#[test]
fn shipped_models_table_names_backends_not_roles() {
    use crate::config::{default_models, DEFAULT_ASR_MODEL, DEFAULT_TTS_MODEL};
    let models = default_models();
    let moonshine = models.get(DEFAULT_ASR_MODEL).expect("moonshine listed");
    assert_eq!(moonshine.primary_backend(), Some("moonshine"));
    assert!(
        moonshine.weights.is_some(),
        "moonshine declares GGUF weights"
    );
    assert!(moonshine.params.is_none(), "defaults carry no params");
    assert!(
        moonshine.role_params.is_none(),
        "defaults carry no overrides"
    );
    let kokoro = models.get(DEFAULT_TTS_MODEL).expect("kokoro listed");
    assert_eq!(kokoro.primary_backend(), Some("kokoro"));
    let integrated = models.get("liquidai/lfm3-audio-2b").expect("LFM3 listed");
    assert_eq!(integrated.primary_backend(), Some("audio-server"));
    // The serving sets live in the roles table, not on the entries.
    let roles = crate::config::RolesConfig::default();
    assert!(roles.asr.models.contains(&DEFAULT_ASR_MODEL.to_string()));
    assert!(roles
        .asr
        .models
        .contains(&"liquidai/lfm3-audio-2b".to_string()));
    assert!(roles.tts.models.contains(&DEFAULT_TTS_MODEL.to_string()));
    assert!(roles
        .tts
        .models
        .contains(&"liquidai/lfm3-audio-2b".to_string()));
}

/// A model-side `roles` list is rejected loudly: membership belongs
/// on the role side and would otherwise be silently ignored. (A bare
/// sequence refuses as a type error; a map with `models` inside
/// refuses by name — see the override test below.)
#[test]
fn model_side_roles_rejected_with_a_pointer() {
    let parsed: Result<crate::config::ModelSpec, _> = serde_json::from_value(serde_json::json!({
        "backend": "whisper",
        "roles": ["asr"],
    }));
    parsed.expect_err("model-side roles refuse");
}

/// A model-side `models` list inside a role override is rejected
/// loudly: it belongs on the role side.
#[test]
fn override_side_models_rejected_with_a_pointer() {
    let parsed: Result<crate::config::ModelsConfig, _> =
        serde_json::from_value(serde_json::json!({
            "probe": {
                "backends": ["whisper"],
                "roles": { "asr": { "models": ["probe"] } },
            },
        }));
    let error = parsed.expect_err("override-side models refuse");
    assert!(error.to_string().contains("models"), "{error}");
}

/// Roles resolve to ordered model preference: moonshine leads ASR,
/// kokoro leads TTS, and the leaf defaults agree with the role
/// heads (one source of truth, pinned here).
#[test]
fn roles_resolve_specialized_models_first() {
    use crate::config::{resolve_role_model, RolesConfig, DEFAULT_ASR_MODEL, DEFAULT_TTS_MODEL};
    let roles = RolesConfig::default();
    assert_eq!(
        resolve_role_model(&roles, "asr").as_deref(),
        Some(DEFAULT_ASR_MODEL)
    );
    assert_eq!(
        resolve_role_model(&roles, "tts").as_deref(),
        Some(DEFAULT_TTS_MODEL)
    );
    assert_eq!(
        resolve_role_model(&roles, "ledger"),
        None,
        "unknown roles resolve to nothing"
    );
    assert_eq!(
        SttConfig::default().model,
        DEFAULT_ASR_MODEL,
        "STT leaf follows the ASR role"
    );
    let empty = RolesConfig {
        asr: crate::config::RoleSpec {
            models: Vec::new(),
            params: None,
        },
        tts: crate::config::RoleSpec {
            models: Vec::new(),
            params: None,
        },
        chat: crate::config::RoleSpec {
            models: Vec::new(),
            params: None,
        },
    };
    assert_eq!(
        resolve_role_model(&empty, "asr"),
        None,
        "empty roles resolve to nothing"
    );
}

/// Composed role params follow the router chain: model base, then the
/// role run block, then the model-side per-role override — each layer
/// winning wholesale per key, non-object sides contributing nothing.
#[test]
fn role_params_compose_base_runblock_override() {
    use crate::config::{compose_role_params, overlay_params, ModelsConfig, RolesConfig};
    let mut models = ModelsConfig::default();
    models
        .models
        .get_mut("large-v3-turbo-q5_0")
        .expect("whisper listed")
        .params = Some(serde_json::json!({"language": "en", "threads": 4}));
    let mut roles = RolesConfig::default();
    roles.asr.params = Some(serde_json::json!({"threads": 8, "vad": true}));
    models
        .models
        .get_mut("large-v3-turbo-q5_0")
        .expect("whisper listed")
        .role_params = Some(std::collections::HashMap::from([(
        "asr".to_string(),
        crate::config::ModelRoleOverride {
            params: Some(serde_json::json!({"vad": false})),
        },
    )]));
    let composed = compose_role_params(&models, &roles, "asr", "large-v3-turbo-q5_0");
    assert_eq!(
        composed["language"], "en",
        "base survives under later layers"
    );
    assert_eq!(composed["threads"], 8, "run block wins over the base");
    assert_eq!(composed["vad"], false, "override wins over the run block");
    // Unknown roles and tags compose to an empty object (callers fail
    // closed on the missing model/backend, never on params).
    assert_eq!(
        compose_role_params(&models, &roles, "ledger", "large-v3-turbo-q5_0"),
        serde_json::json!({})
    );
    assert_eq!(
        compose_role_params(&models, &roles, "asr", "no-such-model"),
        serde_json::json!({})
    );
    // Overlay semantics match the router: shallow wins, non-objects pass through.
    assert_eq!(
        overlay_params(
            Some(&serde_json::json!({"a": 1, "b": 1})),
            Some(&serde_json::json!({"b": 2}))
        ),
        serde_json::json!({"a": 1, "b": 2})
    );
    assert_eq!(
        overlay_params(
            Some(&serde_json::json!([1])),
            Some(&serde_json::json!({"b": 2}))
        ),
        serde_json::json!({"b": 2}),
        "non-object base contributes nothing"
    );
    assert_eq!(
        overlay_params(None, None),
        serde_json::json!({}),
        "empty chain is an empty object"
    );
}

/// Router-style weights resolution: absolute declarations win,
/// relative ones join under the top-level gguf dir, backend-owned
/// files resolve to nothing.
#[test]
fn model_weights_resolve_router_style() {
    use crate::config::{resolve_model_weights, ModelsConfig, DEFAULT_GGUF_DIR};
    let models = ModelsConfig::default();
    let moonshine =
        resolve_model_weights(&models, DEFAULT_GGUF_DIR, crate::config::DEFAULT_ASR_MODEL)
            .expect("moonshine weights resolve");
    assert_eq!(
        moonshine,
        std::path::PathBuf::from(format!(
            "{DEFAULT_GGUF_DIR}/handy-computer/moonshine-streaming-medium-gguf/latest.gguf"
        ))
    );
    // Backend-owned files (whisper ggml, kokoro onnx) resolve to nothing here.
    assert_eq!(
        resolve_model_weights(&models, DEFAULT_GGUF_DIR, "large-v3-turbo-q5_0"),
        None
    );
    assert_eq!(
        resolve_model_weights(&models, DEFAULT_GGUF_DIR, "no-such-model"),
        None,
        "unknown tags resolve to nothing"
    );
    // Temp-home check: relative declarations land under the given root.
    let dir = tempfile::tempdir().expect("tempdir");
    let landed = resolve_model_weights(
        &models,
        &dir.path().display().to_string(),
        crate::config::DEFAULT_ASR_MODEL,
    )
    .expect("resolves");
    assert!(landed.starts_with(dir.path()), "{landed:?}");
}

/// Old files (no gguf_dir/models/roles sections) load the new
/// defaults: /app layout, moonshine ASR tag, kokoro TTS.
#[test]
fn pre_roles_schema_files_load_new_defaults() {
    use crate::config::OratorConfig;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"daemon": {"admission_max": 2}}"#).expect("write fixture");
    let loaded: OratorConfig = common_core::config::load_json(&path).expect("old schema loads");
    assert_eq!(loaded.gguf_dir, crate::config::DEFAULT_GGUF_DIR);
    assert_eq!(loaded.stt.model, crate::config::DEFAULT_ASR_MODEL);
    assert_eq!(loaded.tts.backend, "kokoro");
    assert_eq!(
        crate::config::resolve_role_model(&loaded.roles, "asr").as_deref(),
        Some(crate::config::DEFAULT_ASR_MODEL)
    );
}

#[test]
fn backends_list_drives_dispatch_with_first_wins() {
    use crate::config::{ModelSpec, DEFAULT_MODEL};
    let entry = ModelSpec {
        weights: None,
        backends: vec!["audio-server".to_string(), "whisper".to_string()],
        params: None,
        role_params: None,
    };
    assert_eq!(entry.primary_backend(), Some("audio-server"));
    let empty = ModelSpec {
        weights: None,
        backends: Vec::new(),
        params: None,
        role_params: None,
    };
    assert_eq!(empty.primary_backend(), None, "no backends serves nothing");
    assert_eq!(DEFAULT_MODEL, "liquidai/lfm3-audio-2b");
    let config = crate::config::OratorConfig::default();
    assert_eq!(
        config.default, DEFAULT_MODEL,
        "LFM3-Audio-2B is the default fallback"
    );
}

#[test]
fn empty_role_lists_fall_back_to_the_default_model() {
    use crate::config::{resolve_role_model_or_default, RoleSpec, RolesConfig, DEFAULT_MODEL};
    let roles = RolesConfig {
        asr: RoleSpec {
            models: Vec::new(),
            params: None,
        },
        tts: RoleSpec {
            models: Vec::new(),
            params: None,
        },
        chat: RoleSpec {
            models: Vec::new(),
            params: None,
        },
    };
    assert_eq!(
        resolve_role_model_or_default(
            &roles,
            "asr",
            &crate::config::OratorConfig::default().default
        ),
        Some(DEFAULT_MODEL.to_string())
    );
    assert_eq!(
        resolve_role_model_or_default(&roles, "ledger", "anything"),
        None,
        "unknown roles never fall back"
    );
    assert_eq!(
        resolve_role_model_or_default(&roles, "asr", ""),
        None,
        "empty default resolves to nothing"
    );
}

#[test]
fn singular_backend_rejected_with_a_pointer() {
    let parsed: Result<crate::config::ModelSpec, _> =
        serde_json::from_value(serde_json::json!({"backend": "whisper"}));
    let error = parsed.expect_err("singular backend refuses");
    assert!(error.to_string().contains("backends"), "{error}");
}

#[test]
fn s2s_preferred_only_when_one_audio_server_model_heads_both_roles() {
    use crate::config::{should_prefer_s2s, OratorConfig};
    // Shipped defaults split ASR/TTS across backends: cascade.
    assert!(!should_prefer_s2s(&OratorConfig::default()));
    // Same tag, non-audio backend: still cascade.
    let mut config = OratorConfig::default();
    config.roles.asr.models = vec!["large-v3-turbo-q5_0".to_string()];
    config.roles.tts.models = vec!["large-v3-turbo-q5_0".to_string()];
    assert!(!should_prefer_s2s(&config), "whisper cannot voice natively");
    // Same tag through audio-server: the integrated path.
    config.roles.asr.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    config.roles.tts.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    assert!(
        should_prefer_s2s(&config),
        "one model voicing every role goes native"
    );
    // Empty roles fall back to nothing here (default joins later).
    config.roles.asr.models.clear();
    assert!(!should_prefer_s2s(&config));
}

/// The native preference is pure naming: it fires on tag equality
/// through the audio-server backend without consulting disk or
/// server. A both-heads-LFM3 file prefers native even with an empty
/// weights store — producer confidence (the names match), never a
/// measured turn (task-value). Assurance at use time is what fails
/// loudly; this predicate never gates on it.
#[test]
fn s2s_preference_fires_without_any_serving_evidence() {
    use crate::config::{should_prefer_s2s, OratorConfig, RoleSpec};
    let dir = tempfile::tempdir().expect("tempdir");
    let lfm3 = "liquidai/lfm3-audio-2b".to_string();
    let config = OratorConfig {
        gguf_dir: dir.path().to_string_lossy().into_owned(),
        roles: crate::config::RolesConfig {
            asr: RoleSpec {
                models: vec![lfm3.clone()],
                params: None,
            },
            tts: RoleSpec {
                models: vec![lfm3],
                params: None,
            },
            chat: RoleSpec {
                models: Vec::new(),
                params: None,
            },
        },
        ..OratorConfig::default()
    };
    assert!(
        should_prefer_s2s(&config),
        "fires with an empty store: names only, no evidence"
    );
}

/// The native session model derives from the agreed role model:
/// both heads listed and equal serve that tag; anything else (split
/// heads, empty lists, unknown roles) falls back to the `s2s.model`
/// leaf. Pure tag logic — servability is assured at use time, never
/// guessed here.
#[test]
fn agreed_s2s_model_derives_from_the_heads() {
    use crate::config::{agreed_s2s_model, OratorConfig, RoleSpec, RolesConfig};
    fn heads(asr: &[&str], tts: &[&str]) -> RolesConfig {
        RolesConfig {
            asr: RoleSpec {
                models: asr.iter().map(ToString::to_string).collect(),
                params: None,
            },
            tts: RoleSpec {
                models: tts.iter().map(ToString::to_string).collect(),
                params: None,
            },
            chat: RoleSpec {
                models: Vec::new(),
                params: None,
            },
        }
    }
    let lfm3 = "liquidai/lfm3-audio-2b";
    let leafed = |roles: RolesConfig| OratorConfig {
        roles,
        s2s: crate::config::S2sConfig {
            model: "some-other-leaf".to_string(),
            ..crate::config::S2sConfig::default()
        },
        ..OratorConfig::default()
    };
    assert_eq!(
        agreed_s2s_model(&leafed(heads(&[lfm3], &[lfm3]))),
        lfm3,
        "agreeing heads name the session model"
    );
    assert_eq!(
        agreed_s2s_model(&leafed(heads(&[lfm3], &["large-v3-turbo-q5_0"]))),
        "some-other-leaf",
        "split heads fall back to the leaf"
    );
    assert_eq!(
        agreed_s2s_model(&leafed(heads(&[], &[]))),
        "some-other-leaf",
        "empty lists fall back to the leaf"
    );
    let on_leaf = OratorConfig {
        roles: heads(&[lfm3], &[lfm3]),
        s2s: crate::config::S2sConfig {
            model: lfm3.to_string(),
            ..crate::config::S2sConfig::default()
        },
        ..OratorConfig::default()
    };
    assert_eq!(
        agreed_s2s_model(&on_leaf),
        lfm3,
        "agreement on the leaf is the leaf"
    );
}

/// The shipped chat default is one tag spelled once: the const,
/// the `llm.model` leaf default, and the `roles.chat` head default
/// all name it. A silent drift here would flip every default chat
/// resolution without failing the consistency-shaped tests.
#[test]
fn default_llm_tag_is_spelled_once() {
    use crate::config::{LlmEndpointConfig, RolesConfig, DEFAULT_LLM_MODEL};
    assert_eq!(DEFAULT_LLM_MODEL, "bartowski/lfm3-1.7b");
    assert_eq!(LlmEndpointConfig::default().model, DEFAULT_LLM_MODEL);
    assert_eq!(
        RolesConfig::default().chat.models,
        vec![DEFAULT_LLM_MODEL.to_string()]
    );
}

/// The `default` tag fills empty role lists only: a listed-but-
/// unresolvable head wins over `default` and fails loudly at use
/// (weights/backend lookup misses the tag). Silent substitution
/// would serve a model nobody listed; the contract is resolve the
/// head, fail loud, never swap.
#[test]
fn unresolvable_head_wins_over_the_default_tag() {
    use crate::config::{resolve_role_model_or_default, RoleSpec, RolesConfig, DEFAULT_MODEL};
    let roles = RolesConfig {
        asr: RoleSpec {
            models: vec!["no-such-tag".to_string()],
            params: None,
        },
        tts: RoleSpec {
            models: Vec::new(),
            params: None,
        },
        chat: RoleSpec {
            models: Vec::new(),
            params: None,
        },
    };
    assert_eq!(
        resolve_role_model_or_default(&roles, "asr", DEFAULT_MODEL),
        Some("no-such-tag".to_string()),
        "listed head beats default even when unresolvable"
    );
    assert_eq!(
        resolve_role_model_or_default(&roles, "tts", DEFAULT_MODEL),
        Some(DEFAULT_MODEL.to_string()),
        "empty list still falls back"
    );
}

/// Per-role overrides are params-only: a `backends` key at the role
/// side refuses with the shape named. One model tag cannot serve
/// roles through different runtimes today — recorded, not built.
#[test]
fn per_role_backend_override_refuses_loudly() {
    let parsed: Result<crate::config::ModelRoleOverride, _> =
        serde_json::from_value(serde_json::json!({"backends": ["llm"]}));
    let error = parsed.expect_err("backends at the role side refuses");
    assert!(error.to_string().contains("backends"), "{error}");
}

/// Scored harness: `should_prefer_s2s` as a predictor of "one
/// integrated model voices every role". Must-fire: both heads on the
/// audio-server model (params, store location, and a divergent
/// `s2s.model` leaf change nothing — preference keys off heads).
/// Must-not-fire: split roles, same non-audio tag, unknown tags,
/// any empty list. Scored through the shared gate; the exact counts
/// pin the corpus so a weakened rule fails here, not in review.
#[test]
fn s2s_preference_passes_the_scored_gate() {
    use crate::config::{should_prefer_s2s, OratorConfig};
    use common_core::calibration::calibrate_threshold;
    struct Case {
        name: &'static str,
        config: OratorConfig,
        must_fire: bool,
    }
    fn headed(asr: &[&str], tts: &[&str]) -> OratorConfig {
        let mut config = OratorConfig::default();
        config.roles.asr.models = asr.iter().map(ToString::to_string).collect();
        config.roles.tts.models = tts.iter().map(ToString::to_string).collect();
        config
    }
    let lfm3 = "liquidai/lfm3-audio-2b";
    let whisper = "large-v3-turbo-q5_0";
    let mut diverged = headed(&[lfm3], &[lfm3]);
    diverged.s2s.model = "some-other-leaf".to_string();
    diverged.gguf_dir = "/nowhere/store".to_string();
    diverged.roles.asr.params = Some(serde_json::json!({"language": "en"}));
    let cases = vec![
        Case {
            name: "both heads integrated",
            config: headed(&[lfm3], &[lfm3]),
            must_fire: true,
        },
        Case {
            name: "leaf/store/params diverge, heads agree",
            config: diverged,
            must_fire: true,
        },
        Case {
            name: "multi-head lists sharing the integrated head",
            config: headed(&[lfm3, whisper], &[lfm3]),
            must_fire: true,
        },
        Case {
            name: "shipped defaults split backends",
            config: OratorConfig::default(),
            must_fire: false,
        },
        Case {
            name: "same whisper tag both roles",
            config: headed(&[whisper], &[whisper]),
            must_fire: false,
        },
        Case {
            name: "same unknown tag both roles",
            config: headed(&["no-such-a", "x"], &["no-such-a"]),
            must_fire: false,
        },
        Case {
            name: "split roles",
            config: headed(&[whisper], &[lfm3]),
            must_fire: false,
        },
        Case {
            name: "empty asr",
            config: headed(&[], &[lfm3]),
            must_fire: false,
        },
        Case {
            name: "empty tts",
            config: headed(&[lfm3], &[]),
            must_fire: false,
        },
        Case {
            name: "both empty",
            config: headed(&[], &[]),
            must_fire: false,
        },
    ];
    for case in &cases {
        assert_eq!(
            should_prefer_s2s(&case.config),
            case.must_fire,
            "case `{}` decided wrong",
            case.name
        );
    }
    let report = calibrate_threshold(
        &cases,
        |case| f64::from(should_prefer_s2s(&case.config)),
        |case| case.must_fire,
        0.5,
    );
    assert_eq!((report.tp, report.fp, report.tn, report.r#fn), (3, 0, 7, 0));
    assert!(
        report.passes_gate(),
        "preference must pass the shared gate: {report:?}"
    );
    // Anti-tautology: inverted labels must NOT pass (the harness
    // reads the rule, not its own expectations).
    let inverted = calibrate_threshold(
        &cases,
        |case| f64::from(should_prefer_s2s(&case.config)),
        |case| !case.must_fire,
        0.5,
    );
    assert!(
        !inverted.passes_gate(),
        "inverted labels must fail the gate: {inverted:?}"
    );
}

/// Scored harness: `default`-fallback as a predictor of "resolution
/// yields a model". Must-fire: empty lists with `default` set (the
/// fallback serves), listed heads (resolvable or not — the
/// resolve-then-fail-loud contract). Must-not-fire, fail-closed:
/// empty `default`, unknown roles even with `default` set. Exact
/// winners pin alongside the gate.
#[test]
fn default_fallback_passes_the_scored_gate() {
    use crate::config::{resolve_role_model_or_default, RoleSpec, RolesConfig, DEFAULT_MODEL};
    use common_core::calibration::calibrate_threshold;
    struct Case {
        name: &'static str,
        roles: RolesConfig,
        role: &'static str,
        default: String,
        winner: Option<String>,
    }
    fn roles(asr: &[&str], tts: &[&str]) -> RolesConfig {
        RolesConfig {
            asr: RoleSpec {
                models: asr.iter().map(ToString::to_string).collect(),
                params: None,
            },
            tts: RoleSpec {
                models: tts.iter().map(ToString::to_string).collect(),
                params: None,
            },
            chat: RoleSpec {
                models: Vec::new(),
                params: None,
            },
        }
    }
    let cases = vec![
        Case {
            name: "empty asr falls back",
            roles: roles(&[], &[]),
            role: "asr",
            default: DEFAULT_MODEL.to_string(),
            winner: Some(DEFAULT_MODEL.to_string()),
        },
        Case {
            name: "empty tts falls back",
            roles: roles(&[], &[]),
            role: "tts",
            default: DEFAULT_MODEL.to_string(),
            winner: Some(DEFAULT_MODEL.to_string()),
        },
        Case {
            name: "listed head wins over default",
            roles: roles(&["large-v3-turbo-q5_0"], &[]),
            role: "asr",
            default: DEFAULT_MODEL.to_string(),
            winner: Some("large-v3-turbo-q5_0".to_string()),
        },
        Case {
            name: "unresolvable head still wins (fail loud later)",
            roles: roles(&["no-such-tag"], &[]),
            role: "asr",
            default: DEFAULT_MODEL.to_string(),
            winner: Some("no-such-tag".to_string()),
        },
        Case {
            name: "empty default resolves nothing",
            roles: roles(&[], &[]),
            role: "asr",
            default: String::new(),
            winner: None,
        },
        Case {
            name: "unknown role never falls back",
            roles: roles(&[], &[]),
            role: "ledger",
            default: DEFAULT_MODEL.to_string(),
            winner: None,
        },
        Case {
            name: "unknown role with empty default",
            roles: roles(&["large-v3-turbo-q5_0"], &[]),
            role: "ledger",
            default: String::new(),
            winner: None,
        },
    ];
    for case in &cases {
        assert_eq!(
            resolve_role_model_or_default(&case.roles, case.role, &case.default),
            case.winner,
            "case `{}` picked wrong",
            case.name
        );
    }
    let report = calibrate_threshold(
        &cases,
        |case| {
            f64::from(
                resolve_role_model_or_default(&case.roles, case.role, &case.default).is_some(),
            )
        },
        |case| case.winner.is_some(),
        0.5,
    );
    assert_eq!((report.tp, report.fp, report.tn, report.r#fn), (4, 0, 3, 0));
    assert!(
        report.passes_gate(),
        "fallback must pass the shared gate: {report:?}"
    );
    let inverted = calibrate_threshold(
        &cases,
        |case| {
            f64::from(
                resolve_role_model_or_default(&case.roles, case.role, &case.default).is_some(),
            )
        },
        |case| case.winner.is_none(),
        0.5,
    );
    assert!(
        !inverted.passes_gate(),
        "inverted labels must fail the gate: {inverted:?}"
    );
}

/// The chat role resolves through the shared chain: head first,
/// top-level `default` filling empty lists, composed params over
/// base ← run block ← model-side override. Unknown roles still
/// resolve to nothing.
#[test]
fn chat_role_resolves_through_the_shared_chain() {
    use crate::config::{
        compose_role_params, resolve_role_model, resolve_role_model_or_default, RoleSpec,
        RolesConfig, DEFAULT_MODEL,
    };
    let roles = RolesConfig {
        asr: RoleSpec {
            models: Vec::new(),
            params: None,
        },
        tts: RoleSpec {
            models: Vec::new(),
            params: None,
        },
        chat: RoleSpec {
            models: vec!["chat-probe".to_string()],
            params: None,
        },
    };
    assert_eq!(
        resolve_role_model(&roles, "chat").as_deref(),
        Some("chat-probe")
    );
    assert_eq!(resolve_role_model(&roles, "ledger"), None);
    let empty = RolesConfig {
        asr: RoleSpec {
            models: Vec::new(),
            params: None,
        },
        tts: RoleSpec {
            models: Vec::new(),
            params: None,
        },
        chat: RoleSpec {
            models: Vec::new(),
            params: None,
        },
    };
    assert_eq!(
        resolve_role_model_or_default(&empty, "chat", DEFAULT_MODEL).as_deref(),
        Some(DEFAULT_MODEL)
    );
    let mut models = crate::config::ModelsConfig::default();
    models.models.insert(
        "chat-probe".to_string(),
        crate::config::ModelSpec {
            weights: None,
            backends: vec!["llm".to_string()],
            params: Some(serde_json::json!({"ctx": 2048})),
            role_params: Some(std::collections::HashMap::from([(
                "chat".to_string(),
                crate::config::ModelRoleOverride {
                    params: Some(serde_json::json!({"ctx": 4096})),
                },
            )])),
        },
    );
    let chat_roles = RolesConfig {
        asr: RoleSpec {
            models: Vec::new(),
            params: None,
        },
        tts: RoleSpec {
            models: Vec::new(),
            params: None,
        },
        chat: RoleSpec {
            models: vec!["chat-probe".to_string()],
            params: Some(serde_json::json!({"n_batch": 1024})),
        },
    };
    let composed = compose_role_params(&models, &chat_roles, "chat", "chat-probe");
    assert_eq!(
        composed.get("ctx"),
        Some(&serde_json::json!(4096)),
        "override wins"
    );
    assert_eq!(
        composed.get("n_batch"),
        Some(&serde_json::json!(1024)),
        "run block composes"
    );
}

/// The shipped table serves the chat role: an `llm`-backend entry
/// for the leaf default, so every default role head resolves.
#[test]
fn shipped_table_serves_the_chat_role() {
    let models = crate::config::ModelsConfig::default();
    let entry = models
        .models
        .get("bartowski/lfm3-1.7b")
        .expect("chat default listed");
    assert_eq!(entry.backends, vec!["llm".to_string()]);
    let weights = entry
        .weights
        .as_deref()
        .expect("chat default declares weights");
    assert!(
        !weights.is_empty(),
        "chat weights resolve, never backend-owned"
    );
}

/// Three-level weights layering: `llm.weights` resolves against the
/// `llm.gguf_dir` leaf only; the top-level `gguf_dir` never enters
/// `resolve_llm_weights` (it is not even a parameter). The models
/// table resolves the same-shaped relative declaration against the
/// top level instead — two roots, no shared composition.
#[test]
fn llm_weights_ignore_the_top_level_gguf_dir() {
    use crate::config::{resolve_llm_weights, resolve_model_weights, LlmEndpointConfig};
    let llm = LlmEndpointConfig {
        weights: "rel/w.gguf".to_string(),
        gguf_dir: "/srv/llm".to_string(),
        ..LlmEndpointConfig::default()
    };
    assert_eq!(
        resolve_llm_weights(&llm),
        Some(std::path::PathBuf::from("/srv/llm/rel/w.gguf")),
        "llm leaf root joins"
    );
    let bare = LlmEndpointConfig {
        weights: "rel/w.gguf".to_string(),
        gguf_dir: String::new(),
        ..LlmEndpointConfig::default()
    };
    assert_eq!(
        resolve_llm_weights(&bare),
        Some(std::path::PathBuf::from("rel/w.gguf")),
        "empty llm leaf passes through"
    );
    // Same relative shape, models-table side: the top level joins.
    let mut models = crate::config::ModelsConfig::default();
    models
        .models
        .get_mut("liquidai/lfm3-audio-2b")
        .expect("LFM3 listed")
        .weights = Some("rel/w.gguf".to_string());
    assert_eq!(
        resolve_model_weights(&models, "/srv/top", "liquidai/lfm3-audio-2b"),
        Some(std::path::PathBuf::from("/srv/top/rel/w.gguf")),
        "table side joins under the top root"
    );
}

/// M7-S2: the root control-plane handle. Nested delegation reads
/// and writes through the leaf schemas; skipped tables refuse by
/// name; unknown sections refuse as not-found.
#[test]
fn root_field_access_delegates_into_sections() {
    use fluent_wvr::FieldAccess;
    let mut config = OratorConfig::default();
    assert_eq!(
        config.get_field("llm.model").expect("nested get"),
        config.llm.model
    );
    config
        .set_field("tts.backend", "kokoro")
        .expect("nested set");
    assert_eq!(config.tts.backend, "kokoro");
    // Whole-table JSON stays out of field access (config_get/set own it).
    assert!(config.get_field("models").is_err(), "tables are skipped");
    assert!(config.get_field("roles").is_err(), "roles are skipped");
    assert!(
        config.get_field("bogus.backend").is_err(),
        "unknown sections refuse"
    );
    assert!(
        config.set_field("llm.bogus", "x").is_err(),
        "unknown leaves refuse"
    );
    // Top-level leaves ride the root handle too.
    config
        .set_field("gguf_dir", "/srv/gguf")
        .expect("top-level set");
    assert_eq!(
        config.get_field("gguf_dir").expect("top-level get"),
        "/srv/gguf"
    );
}

/// M7-S2 describe() golden: the whole-tree schema — every advertised
/// dotted key present, tables absent, sections flattened, leaf
/// constraints pinned. Schema edits are behavior changes.
#[test]
fn describe_pins_the_whole_tree_schema() {
    use fluent_wvr::{Describable, FieldAccess};
    let config = OratorConfig::default();
    let schema = config.describe();
    // Exact flattened surface (static leaves first, then sections).
    let mut expected = vec!["gguf_dir".to_string(), "default".to_string()];
    for (section, leaves) in [
        ("audio", ["backend", "mic", "aec"].as_slice()),
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
            ]
            .as_slice(),
        ),
        (
            "turn",
            ["hold_min_ms", "hold_max_ms", "barge_in_ms"].as_slice(),
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
    assert_eq!(config.field_names(), expected.as_slice());
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .expect("object schema");
    // Every dotted leaf plus one object per section, nothing else.
    let mut keys: Vec<&str> = properties.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut want: Vec<&str> = expected.iter().map(String::as_str).collect();
    want.extend(["audio", "stt", "llm", "tts", "s2s", "turn", "daemon"]);
    want.sort_unstable();
    assert_eq!(keys, want);
    assert!(
        !properties.contains_key("models"),
        "tables stay out of the schema"
    );
    assert!(
        !properties.contains_key("roles"),
        "roles stay out of the schema"
    );
    // Sections nest with the flatten marker; constraints pin exactly.
    for section in ["audio", "stt", "llm", "tts", "s2s", "turn", "daemon"] {
        assert_eq!(
            properties[section].get("x-flatten"),
            Some(&serde_json::Value::Bool(true)),
            "{section} flattens"
        );
    }
    assert_eq!(
        properties["llm.ctx"],
        serde_json::json!({
            "description": "Context window",
            "maximum": 65535.0,
            "minimum": 1024.0,
            "type": "integer",
        })
    );
    assert_eq!(
        properties["gguf_dir"],
        serde_json::json!({
            "description": "GGUF weights home",
            "maxLength": 256,
            "type": "string",
        })
    );
    // Deterministic: the schema is a pure function of the struct.
    assert_eq!(schema, config.describe());
}
