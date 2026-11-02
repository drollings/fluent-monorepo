//! Supervisor contract: resolution order, slot-save guard, argv shape,
//! attached lifecycle against a stub server, bounded-recovery policy.

use std::time::Duration;

use crate::supervisor::{
    boot_with_binary, build_args, find_on_path, free_port, recovery_action,
    resolve_binary_validated, resolve_with_override, slot_save_flag, HealthProbe, RecoveryAction,
    Supervisor, SupervisorConfig, GPU_OFFLOAD_LAYERS, LLAMA_SERVER_ENV,
};

/// Process env is shared across test threads: the tests below
/// mutate it, so they serialize on one lock.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Writable fixture base outside `/tmp`: the ownership gate rejects
/// anything under `/tmp`, so fixtures live under the test binary's
/// own directory instead.
fn fixture_dir() -> tempfile::TempDir {
    let base = std::env::current_exe()
        .expect("exe path")
        .parent()
        .expect("exe parent")
        .to_path_buf();
    tempfile::tempdir_in(base).expect("fixture dir")
}

#[test]
fn explicit_override_wins_over_path_search() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let explicit = std::path::PathBuf::from("/opt/llama/bin/llama-server");
    assert_eq!(resolve_with_override(Some(explicit.clone())), Some(explicit));
    // No override: whatever PATH says (possibly nothing on this machine).
    let _ = resolve_with_override(None);
    let _ = find_on_path("llama-server");
}

#[test]
fn validated_resolution_accepts_owner_only_hits() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior = std::env::var_os(LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    std::env::remove_var(LLAMA_SERVER_ENV);
    let dir = fixture_dir();
    let candidate = dir.path().join("llama-server");
    std::fs::write(&candidate, "#!/bin/sh\nexit 0\n").expect("stub writes");
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755))
        .expect("stub chmod");
    let mut paths = vec![dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&prior_path.clone().unwrap_or_default()));
    std::env::set_var("PATH", std::env::join_paths(paths).expect("join"));
    let resolved = resolve_binary_validated(orator_core::paths::current_uid())
        .expect("owner-only hit validates");
    assert_eq!(resolved, candidate);
    match prior {
        Some(value) => std::env::set_var(LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
}

#[test]
fn validated_resolution_rejects_untrusted_hits() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior = std::env::var_os(LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    std::env::remove_var(LLAMA_SERVER_ENV);
    let dir = fixture_dir();
    let candidate = dir.path().join("llama-server");
    std::fs::write(&candidate, "#!/bin/sh\nexit 0\n").expect("stub writes");
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o777))
        .expect("stub chmod");
    let mut paths = vec![dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&prior_path.clone().unwrap_or_default()));
    std::env::set_var("PATH", std::env::join_paths(paths).expect("join"));
    let error = resolve_binary_validated(orator_core::paths::current_uid())
        .expect_err("world-writable hit is refused");
    assert!(error.to_string().contains("writable"), "{error}");
    match prior {
        Some(value) => std::env::set_var(LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
}

#[test]
fn explicit_llama_override_bypasses_validation() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior = std::env::var_os(LLAMA_SERVER_ENV);
    std::env::set_var(LLAMA_SERVER_ENV, "/tmp/definitely-not-llama-server");
    let resolved = resolve_binary_validated(orator_core::paths::current_uid())
        .expect("explicit override is trusted");
    assert_eq!(
        resolved,
        std::path::PathBuf::from("/tmp/definitely-not-llama-server")
    );
    match prior {
        Some(value) => std::env::set_var(LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(LLAMA_SERVER_ENV),
    }
}

#[test]
fn symlinked_server_validates_through_its_target() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior = std::env::var_os(LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    std::env::remove_var(LLAMA_SERVER_ENV);
    // A link to a trusted target validates; a link into /tmp does not.
    let dir = fixture_dir();
    let target = dir.path().join("real-server");
    std::fs::write(&target, "#!/bin/sh\nexit 0\n").expect("stub writes");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
        .expect("stub chmod");
    let link = dir.path().join("llama-server");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let mut paths = vec![dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&prior_path.clone().unwrap_or_default()));
    std::env::set_var("PATH", std::env::join_paths(paths).expect("join"));
    let resolved = resolve_binary_validated(orator_core::paths::current_uid())
        .expect("linked trusted target validates");
    assert_eq!(resolved, link);
    std::fs::remove_file(&link).expect("unlink");
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(tmp.path().starts_with("/tmp"));
    let evil = tmp.path().join("evil-server");
    std::fs::write(&evil, "#!/bin/sh\nexit 0\n").expect("stub writes");
    std::os::unix::fs::symlink(&evil, &link).expect("symlink");
    resolve_binary_validated(orator_core::paths::current_uid())
        .expect_err("link into /tmp is refused");
    match prior {
        Some(value) => std::env::set_var(LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
}

#[test]
fn free_port_names_a_bindable_port() {
    // Port hints race by nature (a gateway autospawner may claim the
    // port between hint and bind), so retry a few fresh hints
    // before calling it a failure.
    for _ in 0..10 {
        let port = free_port().expect("loopback must yield a port");
        assert_ne!(port, 0);
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return;
        }
    }
    panic!("no hinted port stayed bindable across 10 tries");
}

#[test]
fn slot_save_guard_keeps_good_dirs_and_omits_bad_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let kept = slot_save_flag(Some(dir.path().to_path_buf()));
    assert_eq!(kept, Some(dir.path().to_string_lossy().into_owned()));
    let omitted = slot_save_flag(Some(std::path::PathBuf::from(
        "/proc/orator-definitely-missing/slots",
    )));
    assert_eq!(omitted, None);
    assert_eq!(slot_save_flag(None), None);
}

#[test]
fn argv_pins_loopback_and_model_shape() {
    let config = SupervisorConfig {
        model: "qwen3-test".to_string(),
        weights: Some(std::path::PathBuf::from("/models/qwen.gguf")),
        ctx: 8192,
        n_batch: 512,
        slot_save_dir: None,
        port: 18080,
        ..SupervisorConfig::default()
    };
    let argv = build_args(&config, None);
    assert_eq!(&argv[0..4], &["--host", "127.0.0.1", "--port", "18080"]);
    assert!(argv.contains(&"--alias".to_string()));
    assert!(argv.contains(&"qwen3-test".to_string()));
    let m = argv.iter().position(|arg| arg == "-m").expect("weights flag");
    assert_eq!(argv[m + 1], "/models/qwen.gguf");
    assert!(argv.contains(&"--ctx-size".to_string()));
    assert!(argv.contains(&"--batch-size".to_string()));
    // Unresolvable slot dir is omitted, never passed through to die on.
    assert!(!argv.iter().any(|arg| arg == "--slot-save-path"));
}

#[test]
fn argv_carries_audio_components_verbatim() {
    let config = SupervisorConfig {
        model: "lfm2.5-audio-1.5b".to_string(),
        weights: Some(std::path::PathBuf::from("/models/lfm.gguf")),
        mmproj: Some(std::path::PathBuf::from("/models/mmproj.gguf")),
        extra_args: vec!["--audio-flag".to_string(), "fast".to_string()],
        port: 18081,
        ..SupervisorConfig::default()
    };
    let argv = build_args(&config, None);
    let mm = argv.iter().position(|arg| arg == "--mmproj").expect("mmproj flag");
    assert_eq!(argv[mm + 1], "/models/mmproj.gguf");
    assert!(argv.contains(&"--audio-flag".to_string()));
    assert!(argv.contains(&"fast".to_string()));
    // Text-only default carries neither.
    let plain = build_args(&SupervisorConfig::default(), None);
    assert!(!plain.iter().any(|arg| arg == "--mmproj"));
}

#[test]
fn gpu_offload_pair_requests_full_layers_and_defaults_carry_none() {
    // The opt-in pair flows verbatim into argv, in order.
    let config = SupervisorConfig {
        extra_args: vec![
            "--n-gpu-layers".to_string(),
            GPU_OFFLOAD_LAYERS.to_string(),
        ],
        ..SupervisorConfig::default()
    };
    let argv = build_args(&config, None);
    let flag = argv
        .iter()
        .position(|arg| arg == "--n-gpu-layers")
        .expect("offload flag present");
    assert_eq!(argv[flag + 1], GPU_OFFLOAD_LAYERS);
    // CPU default carries no offload flag: today's argv exactly.
    let plain = build_args(&SupervisorConfig::default(), None);
    assert!(!plain.iter().any(|arg| arg == "--n-gpu-layers"));
}

#[test]
fn recovery_policy_restarts_then_contains() {
    assert_eq!(recovery_action(0, 3), RecoveryAction::Restart);
    assert_eq!(recovery_action(2, 3), RecoveryAction::Restart);
    assert_eq!(recovery_action(3, 3), RecoveryAction::Contain);
    assert_eq!(recovery_action(9, 3), RecoveryAction::Contain);
}

#[test]
fn boot_with_missing_binary_fails_actionably() {
    let config = SupervisorConfig {
        model: "qwen3-test".to_string(),
        weights: None,
        ctx: 1024,
        n_batch: 64,
        slot_save_dir: None,
        port: free_port().expect("port"),
        ..SupervisorConfig::default()
    };
    let Err(error) = common_core::runtime::block_on_shared(boot_with_binary(
        config,
        std::path::Path::new("/nonexistent-orator/llama-server"),
    )) else {
        panic!("missing binary must fail");
    };
    assert!(
        error.to_string().contains("llama-server"),
        "unexpected error: {error}"
    );
}

#[test]
fn attached_lifecycle_against_stub_server() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("GET").path("/health");
        then.status(200).json_body(serde_json::json!({"status": "ok"}));
    });
    common_core::runtime::block_on_shared(async {
        let supervisor = Supervisor::attach(server.base_url());
        assert!(!supervisor.is_live());
        supervisor
            .wait_healthy_timeout(Duration::from_secs(10))
            .await
            .expect("stub is healthy");
        assert!(supervisor.is_live());
        supervisor.stop();
        assert!(!supervisor.is_live());
    });
}

#[test]
fn unhealthy_attach_fails_fast() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("GET").path("/health");
        then.status(500).body("sick");
    });
    common_core::runtime::block_on_shared(async {
        let supervisor = Supervisor::attach(server.base_url());
        supervisor
            .wait_healthy_timeout(Duration::from_secs(2))
            .await
            .expect_err("sick stub must fail health");
        assert!(!supervisor.is_live());
    });
}

#[test]
fn options_completions_probe_accepts_2xx() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("OPTIONS").path("/v1/chat/completions");
        then.status(200);
    });
    common_core::runtime::block_on_shared(async {
        let supervisor =
            Supervisor::attach_with_probe(server.base_url(), HealthProbe::OptionsCompletions);
        supervisor
            .wait_healthy_timeout(Duration::from_secs(10))
            .await
            .expect("OPTIONS 200 is healthy");
        assert!(supervisor.is_live());
    });
}

#[test]
fn options_completions_probe_rejects_405_strictly() {
    // No calibration yet: 405 is not success, so the audio probe
    // fails closed on it until the calibration decision lands.
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("OPTIONS").path("/v1/chat/completions");
        then.status(405).body("method not allowed");
    });
    common_core::runtime::block_on_shared(async {
        let supervisor =
            Supervisor::attach_with_probe(server.base_url(), HealthProbe::OptionsCompletions);
        supervisor
            .wait_healthy_timeout(Duration::from_secs(2))
            .await
            .expect_err("405 is not 2xx, so not healthy");
        assert!(!supervisor.is_live());
    });
}

#[test]
fn get_health_probe_rejects_non_2xx() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("GET").path("/health");
        then.status(405).body("method not allowed");
    });
    common_core::runtime::block_on_shared(async {
        let supervisor =
            Supervisor::attach_with_probe(server.base_url(), HealthProbe::GetHealth);
        supervisor
            .wait_healthy_timeout(Duration::from_secs(2))
            .await
            .expect_err("GET /health 405 is not healthy");
        assert!(!supervisor.is_live());
    });
}

#[test]
fn default_probe_is_get_health() {
    assert_eq!(
        SupervisorConfig::default().health_probe,
        HealthProbe::GetHealth
    );
}

#[test]
fn audio_argv_carries_vocoder_and_speaker_without_alias() {
    let config = SupervisorConfig {
        model: "lfm2.5-audio-1.5b".to_string(),
        weights: Some(std::path::PathBuf::from("/models/backbone.gguf")),
        mmproj: Some(std::path::PathBuf::from("/models/mmproj.gguf")),
        vocoder: Some(std::path::PathBuf::from("/models/vocoder.gguf")),
        tts_speaker_file: Some(std::path::PathBuf::from("/models/tokenizer.gguf")),
        port: 18082,
        ..SupervisorConfig::default()
    };
    let argv = build_args(&config, None);
    let flag = |name: &str| {
        argv.iter()
            .position(|arg| arg == name)
            .map(|pos| argv[pos + 1].clone())
    };
    assert_eq!(flag("-m").as_deref(), Some("/models/backbone.gguf"));
    assert_eq!(flag("-mm").as_deref(), Some("/models/mmproj.gguf"));
    assert_eq!(flag("-mv").as_deref(), Some("/models/vocoder.gguf"));
    assert_eq!(
        flag("--tts-speaker-file").as_deref(),
        Some("/models/tokenizer.gguf")
    );
    assert_eq!(flag("--host").as_deref(), Some("127.0.0.1"));
    assert_eq!(flag("--port").as_deref(), Some("18082"));
    assert!(
        !argv.iter().any(|arg| arg == "--alias"),
        "audio servers take no alias: {argv:?}"
    );
}

#[test]
fn disallowed_extra_arg_refuses_as_usage() {
    use crate::supervisor::validate_extra_args;
    validate_extra_args(&["--n-gpu-layers".to_string(), "999".to_string()])
        .expect("allowlisted flags pass");
    let error = validate_extra_args(&["--exec".to_string(), "rm -rf /".to_string()])
        .expect_err("unknown flags refuse");
    assert!(error.to_string().contains("--exec"), "names the flag: {error}");
}

#[test]
fn free_port_hint_is_bindable_and_retries() {
    let port = free_port().expect("hint");
    let listener =
        std::net::TcpListener::bind(format!("127.0.0.1:{port}")).expect("hint binds");
    let _ = listener;
    let _ = free_port().expect("second hint");
}

#[test]
fn spawned_child_has_clean_env_and_its_own_process_group() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = fixture_dir();
    let out = dir.path().join("child-env.txt");
    let stub = dir.path().join("env-stub.sh");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\necho \"$PATH\" > \"{}\"\necho \"$LANG\" >> \"{}\"\ncat /proc/$$/stat >> \"{}\"\nsleep 5\n",
            out.display(),
            out.display(),
            out.display()
        ),
    )
    .expect("stub writes");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let config = SupervisorConfig {
        port: 0,
        health_timeout: Duration::from_secs(1),
        ..SupervisorConfig::default()
    };
    let Err(error) = common_core::runtime::block_on_shared(boot_with_binary(config, &stub)) else {
        panic!("stub never serves health")
    };
    assert!(
        error.to_string().contains("unhealthy") || error.to_string().contains("exited"),
        "stub fails health, never hangs: {error}"
    );
    let text = std::fs::read_to_string(&out).expect("stub recorded env");
    let mut lines = text.lines();
    assert_eq!(lines.next().unwrap_or_default(), "/usr/bin:/bin");
    assert_eq!(lines.next().unwrap_or_default(), "C.UTF-8");
    let stat = lines.next().unwrap_or_default().to_string();
    let after_paren = stat.rsplit(')').next().unwrap_or_default().trim();
    let fields: Vec<&str> = after_paren.split_whitespace().collect();
    assert!(fields.len() >= 4, "stat parses: {stat}");
    let pid: i32 = stat
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let pgrp: i32 = fields[2].parse().unwrap_or(-1);
    assert_ne!(pid, 0, "pid parses: {stat}");
    assert_eq!(pgrp, pid, "child leads its own process group: {stat}");
}

#[test]
fn failed_boot_reaps_the_spawned_child() {
    use std::os::unix::fs::PermissionsExt as _;
    // No-leak golden: a real child is spawned, the boot fails health,
    // and the failure path must reap it — no zombie survives the call.
    // The stub records its own pid; after `boot_with_binary` returns
    // `Err` (whose path calls `stop()` synchronously), `/proc/<pid>`
    // must be gone.
    let dir = fixture_dir();
    let pidfile = dir.path().join("child.pid");
    let stub = dir.path().join("sleep-stub.sh");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\necho $$ > \"{}\"\nsleep 30\n", pidfile.display()),
    )
    .expect("stub writes");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let config = SupervisorConfig {
        port: 0,
        health_timeout: Duration::from_secs(1),
        ..SupervisorConfig::default()
    };
    let Err(error) = common_core::runtime::block_on_shared(boot_with_binary(config, &stub)) else {
        panic!("stub never serves health")
    };
    assert!(
        error.to_string().contains("unhealthy") || error.to_string().contains("exited"),
        "stub fails health, never hangs: {error}"
    );
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .expect("stub recorded its pid")
        .trim()
        .parse()
        .expect("pid parses");
    assert_ne!(pid, 0, "stub pid recorded");
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "failed boot reaps the child: /proc/{pid} must be gone"
    );
}
