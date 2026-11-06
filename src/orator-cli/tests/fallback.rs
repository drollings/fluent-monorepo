//! Local-fallback tests: refusal classification, dial composition,
//! and role-driven selection — all hermetic (missing files, dead
//! ports, temp HOME; no models, no daemon, no network).

use fluent_concurrency::stream::StreamAbort;

use crate::fallback::{fallback_if_unavailable, voice_unavailable};

/// Process env is shared across test threads: the tests below move
/// HOME, so they serialize on this lock.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn unavailable_covers_nothing_to_serve_not_busy() {
    for down in [
        "orator: daemon refused Transcribe: {\"error\":\"voice auto-boot is off: no voice server answers\"}",
        "voice auto-boot is off: foo",
        "no voice server binary: /bin/nope",
        "voice server disabled here",
        "voice server shutting down",
        "voice endpoint ejected: streak",
        "orator: daemon unavailable at /run/user/1000/orator/orator.sock (run oratord)",
        "orator: no socket path: neither XDG_RUNTIME_DIR nor HOME is set",
        "orator: daemon read failed: connection refused (os error 111)",
    ] {
        assert!(voice_unavailable(down), "falls back on: {down}");
    }
    for terminal in [
        "admission saturated: bound 2",
        "voice server boot in progress — retry shortly (concurrent boot)",
        "voice turn aborted: cancelled",
        "server sent no audio: empty turn",
        "unknown method: DeleteEverything",
        "Approve takes an `id` param",
        "",
    ] {
        assert!(
            !voice_unavailable(terminal),
            "stays terminal on: {terminal}"
        );
    }
}

#[test]
fn composition_falls_back_only_on_unavailable() {
    // Success passes through untouched.
    assert_eq!(
        fallback_if_unavailable::<String>(Ok("hi".to_string()), || {
            panic!("success never falls back")
        })
        .expect("ok passes"),
        "hi"
    );
    // Terminal failures pass through with their text intact.
    let error =
        fallback_if_unavailable::<String>(Err("admission saturated: bound 2".to_string()), || {
            Ok("local".to_string())
        })
        .expect_err("saturated stays terminal");
    assert!(error.contains("saturated"), "{error}");
    // Unavailable failures try the fallback, good or bad.
    assert_eq!(
        fallback_if_unavailable::<String>(
            Err("voice auto-boot is off: nothing serving".to_string()),
            || Ok("local answer".to_string())
        )
        .expect("fallback serves"),
        "local answer"
    );
    let error = fallback_if_unavailable::<String>(
        Err("orator: daemon unavailable at /x (run oratord)".to_string()),
        || Err("orator: no local ASR serves the asr role".to_string()),
    )
    .expect_err("fallback failure reports");
    assert!(error.contains("no local ASR"), "{error}");
}

#[test]
fn whisper_path_needs_a_present_file() {
    let error = crate::fallback::whisper_model_path("definitely-not-a-model-xyz")
        .expect_err("missing file refuses");
    assert!(error.contains("model missing"), "{error}");
    assert!(error.contains("orator models pull"), "{error}");
}

/// Single-home authority: the role walk leads, the `default` tag
/// backstops it, and the `stt.model` leaf closes as the legacy
/// fallback. On a disagreeing file the heads decide; the leaf only
/// serves when the whole role side serves nothing.
#[test]
fn role_walk_leads_the_leaf_closes() {
    let mut config = orator_core::OratorConfig::default();
    config.models.models.insert(
        "m5-leaf-probe".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["whisper".to_string()],
            params: None,
            role_params: None,
        },
    );
    config.models.models.insert(
        "m5-head-probe".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["audio-server".to_string()],
            params: None,
            role_params: None,
        },
    );
    config.models.models.insert(
        "m5-default-probe".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["audio-server".to_string()],
            params: None,
            role_params: None,
        },
    );
    config.stt.model = "m5-leaf-probe".to_string();
    config.roles.asr.models = vec!["m5-head-probe".to_string()];
    config.default = "m5-default-probe".to_string();
    let error = crate::fallback::resolve_whisper_model(&config)
        .expect_err("nothing whisper-backed is present");
    let (head_at, default_at, leaf_at) = (
        error.find("m5-head-probe").expect("head tried: {error}"),
        error
            .find("m5-default-probe")
            .expect("default tried: {error}"),
        error.find("m5-leaf-probe").expect("leaf tried: {error}"),
    );
    assert!(
        head_at < default_at && default_at < leaf_at,
        "walk order is heads, default, leaf: {error}"
    );
}

/// A present head beats a present leaf: authority serves the role
/// side first, so the leaf is never even tried on a fully servable
/// role side.
#[test]
fn present_head_beats_a_present_leaf() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    let models_dir = home.path().join(".local/share/orator/models");
    std::fs::create_dir_all(&models_dir).expect("mkdir");
    for tag in ["m5-leaf-probe", "m5-head-probe"] {
        std::fs::write(models_dir.join(format!("ggml-{tag}.bin")), b"stub").expect("seed writes");
    }
    fn entry(backends: &[&str]) -> orator_core::ModelSpec {
        orator_core::ModelSpec {
            weights: None,
            backends: backends.iter().map(ToString::to_string).collect(),
            params: None,
            role_params: None,
        }
    }
    let mut config = orator_core::OratorConfig::default();
    config.stt.model = "m5-leaf-probe".to_string();
    config
        .models
        .models
        .insert("m5-leaf-probe".to_string(), entry(&["whisper"]));
    config
        .models
        .models
        .insert("m5-head-probe".to_string(), entry(&["whisper"]));
    config.roles.asr.models = vec!["m5-head-probe".to_string()];
    let (_, tag) = crate::fallback::resolve_whisper_model(&config).expect("head present");
    assert_eq!(tag, "m5-head-probe", "role side wins over a present leaf");
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// Precedence grid (deterministic — exhaustive, not scored):
/// role heads first, `default` backstop next, the `stt.model` leaf
/// last. Presence is controlled by seeding exactly two weights
/// files under a temp HOME; every other probe tag stays absent.
/// Row (a) proves a present head beats a present leaf; row (e)
/// proves `default` serves when the lists are empty and the tag is
/// present; row (d) proves the loud walk-order failure names heads,
/// default, leaf in authority order; row (f) proves
/// empty-everything fails loudly instead of resolving nothing
/// silently.
#[test]
fn precedence_grid_picks_exact_winners() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    let models_dir = home.path().join(".local/share/orator/models");
    std::fs::create_dir_all(&models_dir).expect("mkdir");
    for tag in ["m3-present-leaf", "m3-present-role"] {
        std::fs::write(models_dir.join(format!("ggml-{tag}.bin")), b"stub").expect("seed writes");
    }
    fn entry(backends: &[&str]) -> orator_core::ModelSpec {
        orator_core::ModelSpec {
            weights: None,
            backends: backends.iter().map(ToString::to_string).collect(),
            params: None,
            role_params: None,
        }
    }
    fn configured(
        leaf: &str,
        leaf_backends: &[&str],
        heads: &[&str],
        default: &str,
    ) -> orator_core::OratorConfig {
        let mut config = orator_core::OratorConfig::default();
        config.stt.model = leaf.to_string();
        config
            .models
            .models
            .insert(leaf.to_string(), entry(leaf_backends));
        for head in heads {
            config
                .models
                .models
                .insert(head.to_string(), entry(&["whisper"]));
        }
        config
            .models
            .models
            .insert("m3-absent-audio".to_string(), entry(&["audio-server"]));
        config
            .models
            .models
            .insert("m3-absent-default".to_string(), entry(&["audio-server"]));
        config.roles.asr.models = heads.iter().map(ToString::to_string).collect();
        config.default = default.to_string();
        config
    }
    // (a) Present head beats a present leaf.
    let config = configured(
        "m3-present-leaf",
        &["whisper"],
        &["m3-present-role"],
        "m3-absent-default",
    );
    assert_eq!(
        crate::fallback::resolve_whisper_model(&config)
            .expect("head present")
            .1,
        "m3-present-role"
    );
    // (b) Missing leaf falls through to a present head.
    let config = configured(
        "m3-absent-whisper",
        &["whisper"],
        &["m3-present-role"],
        "m3-absent-default",
    );
    assert_eq!(
        crate::fallback::resolve_whisper_model(&config)
            .expect("head present")
            .1,
        "m3-present-role"
    );
    // (c) Non-whisper leaf skips to a present head.
    let config = configured(
        "m3-absent-audio",
        &["audio-server"],
        &["m3-present-role"],
        "m3-absent-default",
    );
    assert_eq!(
        crate::fallback::resolve_whisper_model(&config)
            .expect("head present")
            .1,
        "m3-present-role"
    );
    // (d) Nothing whisper-servable: loud failure naming head,
    // default, leaf in authority order.
    let config = configured(
        "m3-absent-whisper",
        &["whisper"],
        &["m3-absent-audio"],
        "m3-absent-default",
    );
    let error = crate::fallback::resolve_whisper_model(&config).expect_err("nothing servable");
    let (head_at, default_at, leaf_at) = (
        error.find("m3-absent-audio").expect("head tried: {error}"),
        error
            .find("m3-absent-default")
            .expect("default tried: {error}"),
        error
            .find("m3-absent-whisper")
            .expect("leaf tried last: {error}"),
    );
    assert!(
        head_at < default_at && default_at < leaf_at,
        "walk order heads, default, leaf: {error}"
    );
    // (e) Empty lists + present default: the fallback serves.
    let mut config = configured("m3-absent-whisper", &["whisper"], &[], "m3-present-role");
    config
        .models
        .models
        .insert("m3-present-role".to_string(), entry(&["whisper"]));
    assert_eq!(
        crate::fallback::resolve_whisper_model(&config)
            .expect("default present")
            .1,
        "m3-present-role"
    );
    // (f) Empty lists + empty default: loud failure, never silent None.
    let config = configured("m3-absent-whisper", &["whisper"], &[], "");
    let error = crate::fallback::resolve_whisper_model(&config).expect_err("no candidates");
    assert!(
        error.contains("no local whisper weights serve ASR"),
        "{error}"
    );
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// Unified walk: role order decides across backends. A present
/// moonshine head beats a whisper head listed later; reversed lists
/// serve whisper. Probe tags with temp-HOME seeding stay hermetic
/// on machines with and without the real weights.
#[test]
fn unified_walk_follows_role_order_across_backends() {
    use crate::fallback::{resolve_local_asr, LocalBackend};
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    let models_dir = home.path().join(".local/share/orator/models");
    std::fs::create_dir_all(&models_dir).expect("mkdir");
    std::fs::write(models_dir.join("ggml-m5w.bin"), b"stub").expect("seed writes");
    let store = home.path().join("weights");
    std::fs::create_dir_all(&store).expect("mkdir");
    std::fs::write(store.join("m5moon.gguf"), b"stub").expect("seed writes");
    fn entry(backends: &[&str], weights: Option<&str>) -> orator_core::ModelSpec {
        orator_core::ModelSpec {
            weights: weights.map(str::to_string),
            backends: backends.iter().map(ToString::to_string).collect(),
            params: None,
            role_params: None,
        }
    }
    fn configured(store: &std::path::Path, heads: &[&str]) -> orator_core::OratorConfig {
        let mut models = orator_core::ModelsConfig::default();
        models
            .models
            .insert("m5w".to_string(), entry(&["whisper"], None));
        models.models.insert(
            "m5moon".to_string(),
            entry(&["moonshine"], Some("m5moon.gguf")),
        );
        models
            .models
            .insert("m5missing".to_string(), entry(&["whisper"], None));
        let base = orator_core::OratorConfig::default();
        orator_core::OratorConfig {
            gguf_dir: store.to_string_lossy().into_owned(),
            stt: orator_core::SttConfig {
                model: "m5missing".to_string(),
                ..base.stt
            },
            models,
            roles: orator_core::RolesConfig {
                asr: orator_core::RoleSpec {
                    models: heads.iter().map(ToString::to_string).collect(),
                    params: None,
                },
                ..base.roles
            },
            ..base
        }
    }
    // Moonshine listed first serves moonshine.
    let found =
        resolve_local_asr(&configured(&store, &["m5moon", "m5w"])).expect("moonshine serves");
    assert_eq!(found.tag, "m5moon");
    assert_eq!(found.backend, LocalBackend::Moonshine);
    // Reversed lists serve whisper.
    let found = resolve_local_asr(&configured(&store, &["m5w", "m5moon"])).expect("whisper serves");
    assert_eq!(found.tag, "m5w");
    assert_eq!(found.backend, LocalBackend::Whisper);
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// The whisper-only resolver keeps its shape: moonshine heads are
/// named skips, a present whisper head still wins.
#[test]
fn whisper_only_resolution_ignores_moonshine_heads() {
    use crate::fallback::resolve_whisper_model;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    let models_dir = home.path().join(".local/share/orator/models");
    std::fs::create_dir_all(&models_dir).expect("mkdir");
    std::fs::write(models_dir.join("ggml-m5w.bin"), b"stub").expect("seed writes");
    let mut config = orator_core::OratorConfig::default();
    config.models.models.insert(
        "m5w".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["whisper".to_string()],
            params: None,
            role_params: None,
        },
    );
    config.models.models.insert(
        "m5moon".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["moonshine".to_string()],
            params: None,
            role_params: None,
        },
    );
    config.stt.model = "m5missing".to_string();
    config.models.models.insert(
        "m5missing".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["whisper".to_string()],
            params: None,
            role_params: None,
        },
    );
    config.roles.asr.models = vec!["m5moon".to_string(), "m5w".to_string()];
    let (_, tag) = resolve_whisper_model(&config).expect("whisper serves");
    assert_eq!(tag, "m5w", "moonshine heads are skips on the whisper path");
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// Moonshine resolution names the missing file with the placement
/// remedy when the weights are absent.
#[test]
fn moonshine_missing_weights_name_the_file() {
    use crate::fallback::resolve_moonshine_model;
    let mut config = orator_core::OratorConfig::default();
    config.models.models.insert(
        "m5moon".to_string(),
        orator_core::ModelSpec {
            weights: Some("m5moon.gguf".to_string()),
            backends: vec!["moonshine".to_string()],
            params: None,
            role_params: None,
        },
    );
    config.stt.model = "m5other".to_string();
    config.roles.asr.models = vec!["m5moon".to_string()];
    let error = resolve_moonshine_model(&config).expect_err("weights absent");
    assert!(error.contains("m5moon.gguf"), "names the file: {error}");
    assert!(error.contains("no local moonshine weights"), "{error}");
}

/// Unservable backends skip with reasons: an audio-server head and
/// an unknown tag are named in walk order, and the failure names
/// the fetch remedy. Probe tags keep this hermetic on machines with
/// and without the real weights.
#[test]
fn unservable_backends_skip_with_reasons() {
    let mut config = orator_core::OratorConfig::default();
    for (tag, backends) in [
        ("m5audio", vec!["audio-server"]),
        ("m5whisper", vec!["whisper"]),
    ] {
        config.models.models.insert(
            tag.to_string(),
            orator_core::ModelSpec {
                weights: None,
                backends: backends.iter().map(ToString::to_string).collect(),
                params: None,
                role_params: None,
            },
        );
    }
    config.stt.model = "m5whisper".to_string();
    config.roles.asr.models = vec!["m5audio".to_string(), "no-such-tag".to_string()];
    let error = crate::fallback::local_transcribe(&[0.0; 320], &config, &StreamAbort::new())
        .expect_err("nothing servable without weights");
    assert!(error.contains("no local ASR weights serve ASR"), "{error}");
    let audio_at = error
        .find("no local audio-server backend")
        .expect("audio skips: {error}");
    let unknown_at = error
        .find("no-such-tag (no spec)")
        .expect("unknown named: {error}");
    let leaf_at = error
        .find("m5whisper (weights missing)")
        .expect("leaf tried: {error}");
    assert!(
        audio_at < unknown_at && unknown_at < leaf_at,
        "heads, then leaf: {error}"
    );
    assert!(error.contains("orator models pull"), "{error}");
}

#[test]
fn roles_walk_selects_present_whisper_weights() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    // Seed a (bogus) ggml file: selection reaches whisper decode,
    // which fails at load — proving the walk picked whisper, not
    // the skip path.
    let models_dir = home.path().join(".local/share/orator/models");
    std::fs::create_dir_all(&models_dir).expect("mkdir");
    std::fs::write(models_dir.join("ggml-probe-whisper.bin"), b"not a model").expect("seed writes");
    let base = orator_core::OratorConfig::default();
    let mut models = base.models.clone();
    models.models.insert(
        "probe-whisper".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["whisper".to_string()],
            params: None,
            role_params: None,
        },
    );
    let config = orator_core::OratorConfig {
        // Point the weights root at the temp HOME too: the default
        // moonshine head must stay missing here, or machines carrying
        // the real GGUF would serve it instead of reaching whisper.
        gguf_dir: home.path().to_string_lossy().into_owned(),
        models,
        roles: orator_core::RolesConfig {
            asr: orator_core::RoleSpec {
                models: vec![
                    orator_core::DEFAULT_ASR_MODEL.to_string(),
                    "probe-whisper".to_string(),
                ],
                params: None,
            },
            ..base.roles
        },
        ..base
    };
    let error = crate::fallback::local_transcribe(&[0.0; 3200], &config, &StreamAbort::new())
        .expect_err("bogus bytes fail at load");
    assert!(
        error.to_lowercase().contains("whisper"),
        "selection reached whisper decode: {error}"
    );
    assert!(
        !error.contains("no local ASR"),
        "present weights are served, not skipped: {error}"
    );
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
fn local_ask_names_a_dead_endpoint() {
    let config = orator_core::OratorConfig {
        llm: orator_core::LlmEndpointConfig {
            endpoint: "127.0.0.1:1".to_string(),
            ..orator_core::LlmEndpointConfig::default()
        },
        ..orator_core::OratorConfig::default()
    };
    let error = crate::fallback::local_ask("hi", &config).expect_err("refused port fails fast");
    assert!(error.contains("127.0.0.1:1"), "{error}");
}

#[test]
fn local_say_names_missing_weights() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    let config = orator_core::OratorConfig::default();
    let error = crate::fallback::local_say("hi", &config).expect_err("no weights fails");
    assert!(error.contains("kokoro"), "{error}");
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// Dead daemon + dead LLM endpoint: the ask dial falls back locally
/// and reports the endpoint, proving the local leg ran (not the
/// daemon refusal).
#[test]
fn ask_dial_falls_back_to_direct_endpoint() {
    use crate::fallback::ask_dial;
    use crate::socket::Transport;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let runtime = tempfile::tempdir().expect("tempdir");
    std::env::set_var("XDG_RUNTIME_DIR", runtime.path());
    let config = orator_core::OratorConfig {
        llm: orator_core::LlmEndpointConfig {
            endpoint: "127.0.0.1:1".to_string(),
            ..orator_core::LlmEndpointConfig::default()
        },
        ..orator_core::OratorConfig::default()
    };
    let error = ask_dial("hi", None, Transport::Socket, &StreamAbort::new(), &config)
        .expect_err("both legs fail loudly");
    assert!(
        error.contains("no LLM server answers at http://127.0.0.1:1"),
        "local leg answered: {error}"
    );
    match prior_runtime {
        Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
        None => std::env::remove_var("XDG_RUNTIME_DIR"),
    }
}

/// Dead daemon: the transcribe dial falls back to the roles walk,
/// which reports the unservable entries instead of the daemon error.
#[test]
fn transcribe_dial_falls_back_to_roles_walk() {
    use crate::fallback::transcribe_dial;
    use crate::socket::Transport;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let runtime = tempfile::tempdir().expect("tempdir");
    std::env::set_var("XDG_RUNTIME_DIR", runtime.path());
    // Probe config with nothing servable: the daemon refuses (no
    // socket), then the unified walk names every tried candidate in
    // role order. Hermetic on machines with and without the real
    // weights (gguf_dir points at the temp HOME).
    let base = orator_core::OratorConfig::default();
    let mut models = base.models.clone();
    models.models.insert(
        "m5w".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["whisper".to_string()],
            params: None,
            role_params: None,
        },
    );
    let config = orator_core::OratorConfig {
        gguf_dir: runtime.path().to_string_lossy().into_owned(),
        stt: orator_core::SttConfig {
            model: "m5w".to_string(),
            ..base.stt
        },
        models,
        roles: orator_core::RolesConfig {
            asr: orator_core::RoleSpec {
                models: vec!["m5w".to_string()],
                params: None,
            },
            ..base.roles
        },
        ..base
    };
    let error = transcribe_dial(
        &[0.0; 3200],
        Transport::Socket,
        &StreamAbort::new(),
        &config,
    )
    .expect_err("nothing servable");
    assert!(
        error.contains("no local ASR weights serve ASR"),
        "roles walk answered, not the daemon: {error}"
    );
    assert!(error.contains("m5w (weights missing)"), "{error}");
    match prior_runtime {
        Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
        None => std::env::remove_var("XDG_RUNTIME_DIR"),
    }
}

/// Dead daemon + empty model home: the say dial falls back to local
/// kokoro and reports the missing weights.
#[test]
fn say_dial_falls_back_to_local_kokoro() {
    use crate::fallback::say_dial;
    use crate::socket::Transport;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let prior_runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    std::env::set_var("XDG_RUNTIME_DIR", runtime.path());
    let config = orator_core::OratorConfig::default();
    let error = say_dial("hi", Transport::Socket, &StreamAbort::new(), &config)
        .expect_err("no weights, no voice");
    assert!(error.contains("kokoro"), "kokoro leg answered: {error}");
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
    match prior_runtime {
        Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
        None => std::env::remove_var("XDG_RUNTIME_DIR"),
    }
}

#[test]
fn local_ops_share_one_ten_second_budget() {
    use crate::fallback::{budgeted, LOCAL_OP_BUDGET};
    assert_eq!(LOCAL_OP_BUDGET, std::time::Duration::from_secs(10));
    assert_eq!(
        budgeted("probe", std::time::Duration::from_secs(10), || Ok::<
            _,
            String,
        >(
            "fast".to_string()
        ))
        .expect("fast op passes"),
        "fast"
    );
    let error = budgeted("probe", std::time::Duration::from_millis(50), || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        Ok::<_, String>("slow".to_string())
    })
    .expect_err("slow op fails instead of stalling");
    assert!(error.contains("50") || error.contains("probe"), "{error}");
    assert!(error.contains("instead of stalling"), "{error}");
}

#[test]
fn resolve_whisper_model_prefers_explicit_then_roles() {
    use crate::fallback::resolve_whisper_model;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    let models = home.path().join(".local/share/orator/models");
    std::fs::create_dir_all(&models).expect("mkdir");
    // Explicit whisper tag with present weights wins outright.
    std::fs::write(models.join("ggml-explicit.bin"), b"x").expect("seed writes");
    let mut config = orator_core::OratorConfig::default();
    config.stt.model = "explicit".to_string();
    config.models.models.insert(
        "explicit".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["whisper".to_string()],
            params: None,
            role_params: None,
        },
    );
    let (path, tag) = resolve_whisper_model(&config).expect("explicit serves");
    assert_eq!(tag, "explicit");
    assert!(path.ends_with("ggml-explicit.bin"), "{}", path.display());
    // A moonshine default falls through to present whisper weights.
    std::fs::write(models.join("ggml-fallback.bin"), b"x").expect("seed writes");
    config.stt.model = orator_core::DEFAULT_ASR_MODEL.to_string();
    config.models.models.insert(
        "fallback".to_string(),
        orator_core::ModelSpec {
            weights: None,
            backends: vec!["whisper".to_string()],
            params: None,
            role_params: None,
        },
    );
    config.roles.asr.models = vec![
        orator_core::DEFAULT_ASR_MODEL.to_string(),
        "fallback".to_string(),
    ];
    let (path, tag) = resolve_whisper_model(&config).expect("roles fall back");
    assert_eq!(
        tag, "fallback",
        "moonshine default yields to present whisper"
    );
    assert!(path.ends_with("ggml-fallback.bin"), "{}", path.display());
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
fn slow_synth_reports_its_tts_budget_instead_of_stalling() {
    use crate::fallback::{budgeted, LOCAL_OP_BUDGET};
    // The chat voice path wraps every local synth in
    // `budgeted("TTS", LOCAL_OP_BUDGET, ...)`; a synth that cannot
    // finish in ten seconds reports the harness-visible line below
    // (surfaced as a chat ERROR event) instead of hanging the turn.
    let started = std::time::Instant::now();
    let error = budgeted("TTS", LOCAL_OP_BUDGET, || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        Ok::<_, String>((vec![0.0; 8], 24_000))
    })
    .expect_err("slow synth reports");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the caller never waits out the synth"
    );
    assert!(
        error.contains("local TTS exceeded its 10 s budget"),
        "{error}"
    );
    assert!(error.contains("instead of stalling"), "{error}");
}

#[test]
fn slow_asr_reports_its_budget_instead_of_stalling() {
    use crate::fallback::{budgeted, LOCAL_OP_BUDGET};
    let started = std::time::Instant::now();
    let error = budgeted("ASR", LOCAL_OP_BUDGET, || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        Ok::<_, String>("late transcript".to_string())
    })
    .expect_err("slow decode reports");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the caller never waits out the decode"
    );
    assert!(
        error.contains("local ASR exceeded its 10 s budget"),
        "{error}"
    );
}

#[test]
fn role_overrides_tune_language_and_voice() {
    use crate::fallback::{resolve_asr_language, resolve_tts_voice};
    let mut config = orator_core::OratorConfig::default();
    // Globals hold without overrides.
    assert_eq!(resolve_asr_language(&config, "large-v3-turbo-q5_0"), "en");
    assert_eq!(resolve_tts_voice(&config, "agentory/kokoro-82m"), "bf_emma");
    // A model base param tunes its roles.
    config
        .models
        .models
        .get_mut("large-v3-turbo-q5_0")
        .expect("whisper listed")
        .params = Some(serde_json::json!({"language": "de"}));
    assert_eq!(resolve_asr_language(&config, "large-v3-turbo-q5_0"), "de");
    // A per-role override wins over the base (chain, not replacement).
    config
        .models
        .models
        .get_mut("large-v3-turbo-q5_0")
        .expect("whisper listed")
        .role_params = Some(std::collections::HashMap::from([(
        "asr".to_string(),
        orator_core::ModelRoleOverride {
            params: Some(serde_json::json!({"language": "fr"})),
        },
    )]));
    assert_eq!(resolve_asr_language(&config, "large-v3-turbo-q5_0"), "fr");
    // The TTS side composes independently (voice override on kokoro).
    config
        .models
        .models
        .get_mut("agentory/kokoro-82m")
        .expect("kokoro listed")
        .role_params = Some(std::collections::HashMap::from([(
        "tts".to_string(),
        orator_core::ModelRoleOverride {
            params: Some(serde_json::json!({"voice": "af_sky"})),
        },
    )]));
    assert_eq!(resolve_tts_voice(&config, "agentory/kokoro-82m"), "af_sky");
    assert_eq!(
        resolve_tts_voice(&config, "no-such-model"),
        "bf_emma",
        "unknown tags fall back to the leaf, never to nothing"
    );
}

/// M4-S1 characterization: the `budgeted` contract edges. A failing op
/// passes its error through unchanged; a panicking op surfaces the
/// same timeout line (fast — the dead worker disconnects the channel,
/// it never hangs to the budget); a timed-out op still runs to
/// completion detached (at most one stray, reaped at process exit).
#[test]
fn orator_budgeted_contract() {
    use crate::fallback::budgeted;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    // Errors pass through verbatim, never wrapped in a timeout.
    assert_eq!(
        budgeted("probe", std::time::Duration::from_secs(10), || Err::<
            String,
            String,
        >(
            "decode blew up".to_string()
        ))
        .expect_err("errors pass through"),
        "decode blew up"
    );
    // Panics become the timeout line (documented, not fixed: no
    // panic propagation across the worker boundary by design).
    let error = budgeted(
        "probe",
        std::time::Duration::from_secs(10),
        || -> Result<String, String> {
            panic!("worker blew up");
        },
    )
    .expect_err("panic reports");
    assert!(
        error.contains("local probe exceeded its 10 s budget"),
        "{error}"
    );
    // The timed-out worker runs detached to completion.
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let error = budgeted("probe", std::time::Duration::from_millis(50), move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        flag.store(true, Ordering::SeqCst);
        Ok::<_, String>("late".to_string())
    })
    .expect_err("slow op times out");
    assert!(error.contains("instead of stalling"), "{error}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "stray worker finishes detached"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// M11-S1 characterization: both fallback predicates over one shared
/// corpus — structured kinds, old-daemon wordings, transport-down,
/// and busy/transient lines. The tables MUST disagree in places (they
/// serve different call sites: local-fallback vs chain-advance), so
/// disagreements are pinned, never silently merged.
#[test]
fn shared_corpus_pins_classifier_disagreements() {
    use crate::fallback::voice_unavailable;
    use crate::say_chain::{classify_daemon_voice_error, DaemonVoiceHint};
    fn hint_name(hint: &DaemonVoiceHint) -> &'static str {
        match hint {
            DaemonVoiceHint::Available(_) => "Available",
            DaemonVoiceHint::Unavailable => "Unavailable",
            DaemonVoiceHint::Ejected(_) => "Ejected",
            DaemonVoiceHint::Saturated(_) => "Saturated",
            DaemonVoiceHint::BootContention(_) => "BootContention",
            DaemonVoiceHint::SlowPeer(_) => "SlowPeer",
        }
    }
    // (line, voice_unavailable, classify discriminant)
    let corpus = [
        // Structured ejection: the chain fires on the kind, but the
        // local fallback does NOT (its marker is the full wording
        // "voice endpoint ejected", not the bare kind).
        ("{\"error\":\"x\",\"kind\":\"Ejected\"}", false, "Ejected"),
        // Old-daemon ejection wording: both fire.
        ("voice endpoint ejected: streak 2", true, "Ejected"),
        // Saturation: the chain skips, the local fallback stays
        // terminal (admission pressure is not nothing-serving).
        ("admission saturated: bound 2", false, "Saturated"),
        (
            "{\"error\":\"x\",\"kind\":\"Saturated\"}",
            false,
            "Saturated",
        ),
        // Boot contention: the chain waits, the fallback stays
        // terminal (a sibling boot means serving exists).
        (
            "voice server boot in progress — retry shortly (concurrent boot)",
            false,
            "BootContention",
        ),
        // Slow peer: chain-retryable, never a local-fallback trigger.
        (
            "{\"error\":\"slow peer: handler budget elapsed\",\"kind\":\"SlowPeer\"}",
            false,
            "SlowPeer",
        ),
        // Transport down: local fallback fires, chain sees plain
        // Unavailable (no kind, no wording markers).
        (
            "orator: daemon unavailable at /run/user/1000/orator.sock (run oratord)",
            true,
            "Unavailable",
        ),
        (
            "orator: daemon read failed: connection refused (os error 111)",
            true,
            "Unavailable",
        ),
        // Voiceless: neither fires (the server answered).
        ("server sent no audio: empty turn", false, "Unavailable"),
        // Shutting down: local fallback fires on the marker; the
        // chain sees Unavailable (no kind/wording match).
        ("voice server shutting down", true, "Unavailable"),
        // Empty: neither fires.
        ("", false, "Unavailable"),
    ];
    for (line, unavailable, hint) in corpus {
        assert_eq!(voice_unavailable(line), unavailable, "fallback on: {line}");
        assert_eq!(
            hint_name(&classify_daemon_voice_error(line)),
            hint,
            "chain hint on: {line}"
        );
    }
}
