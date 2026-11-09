//! Supervisor contract: resolution order, slot-save guard, argv shape,
//! attached lifecycle against a stub server, bounded-recovery policy.

use std::time::Duration;

use crate::supervisor::{
    boot_with_binary, build_args, find_on_path, free_port, recovery_action, resolve_binary,
    resolve_binary_candidates, resolve_binary_validated, resolve_with_override, slot_save_flag,
    HealthProbe, RecoveryAction, Supervisor, SupervisorConfig, GPU_OFFLOAD_LAYERS,
    LLAMA_SERVER_ENV,
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
    assert_eq!(
        resolve_with_override(Some(explicit.clone())),
        Some(explicit)
    );
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
    paths.extend(std::env::split_paths(
        &prior_path.clone().unwrap_or_default(),
    ));
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
    paths.extend(std::env::split_paths(
        &prior_path.clone().unwrap_or_default(),
    ));
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
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).expect("stub chmod");
    let link = dir.path().join("llama-server");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let mut paths = vec![dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &prior_path.clone().unwrap_or_default(),
    ));
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
    let m = argv
        .iter()
        .position(|arg| arg == "-m")
        .expect("weights flag");
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
    let mm = argv
        .iter()
        .position(|arg| arg == "--mmproj")
        .expect("mmproj flag");
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
        extra_args: vec!["--n-gpu-layers".to_string(), GPU_OFFLOAD_LAYERS.to_string()],
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
        then.status(200)
            .json_body(serde_json::json!({"status": "ok"}));
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
        let supervisor = Supervisor::attach_with_probe(server.base_url(), HealthProbe::GetHealth);
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
    assert!(
        error.to_string().contains("--exec"),
        "names the flag: {error}"
    );
}

/// M11-S1 warning capture: records `binary` from each
/// `orator.llm` WARN so candidate order is observable without a
/// global subscriber. Used with `block_on` (current-thread) so the
/// scoped dispatcher stays visible across the boot future.
#[cfg(unix)]
mod warn_capture {
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    #[derive(Clone)]
    pub struct Capture {
        pub warnings: Arc<Mutex<Vec<String>>>,
    }

    pub struct AllVisitor {
        pub text: String,
    }

    impl Visit for AllVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write as _;
            let _ = write!(self.text, "{}={value:?};", field.name());
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            use std::fmt::Write as _;
            let _ = write!(self.text, "{}={value};", field.name());
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() == tracing::Level::WARN && metadata.target() == "orator.llm"
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            if *event.metadata().level() != tracing::Level::WARN {
                return;
            }
            let mut visitor = AllVisitor {
                text: String::new(),
            };
            event.record(&mut visitor);
            self.warnings
                .lock()
                .expect("warnings")
                .push(visitor.text);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }
}

/// Healthy `llama-server` stand-in: parses `--port` and answers
/// `GET /health` 200 on loopback, ignoring every other argv token.
/// Same shape as the S2S fake audio server, but for the text probe.
#[cfg(unix)]
const FAKE_LLAMA_SERVER: &str = r#"#!/usr/bin/env python3
import http.server
import sys

port = 8080
args = sys.argv[1:]
for index, arg in enumerate(args):
    if arg == "--port" and index + 1 < len(args):
        port = int(args[index + 1])


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            body = b'{"status":"ok"}'
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, *args):
        pass


http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()
"#;

/// M11-S1: ordered walk — first fails, second serves, third never runs.
/// First touches its marker then exits; second is the healthy stub;
/// third would touch its marker if attempted. Warnings are captured
/// in order; only the first candidate warns.
#[test]
#[cfg(unix)]
fn boot_tries_candidates_in_order_and_stops_after_success() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_server = std::env::var_os(LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    std::env::remove_var(LLAMA_SERVER_ENV);

    let base = fixture_dir();
    let dir1 = base.path().join("c1");
    let dir2 = base.path().join("c2");
    let dir3 = base.path().join("c3");
    for dir in [&dir1, &dir2, &dir3] {
        std::fs::create_dir(dir).expect("candidate dir");
    }
    let marker1 = base.path().join("ran-first");
    let marker3 = base.path().join("ran-third");
    std::fs::write(
        dir1.join("llama-server"),
        format!("#!/bin/sh\ntouch \"{}\"\nexit 1\n", marker1.display()),
    )
    .expect("failing stub writes");
    std::fs::write(dir2.join("llama-server"), FAKE_LLAMA_SERVER).expect("healthy stub writes");
    std::fs::write(
        dir3.join("llama-server"),
        format!("#!/bin/sh\ntouch \"{}\"\nsleep 30\n", marker3.display()),
    )
    .expect("unused stub writes");
    for dir in [&dir1, &dir2, &dir3] {
        std::fs::set_permissions(dir.join("llama-server"), std::fs::Permissions::from_mode(0o755))
            .expect("stub runs");
    }
    let mut paths = vec![dir1.clone(), dir2.clone(), dir3.clone()];
    paths.extend(std::env::split_paths(&prior_path.clone().unwrap_or_default()));
    std::env::set_var("PATH", std::env::join_paths(paths).expect("join"));

    let port = free_port().expect("fixed free port");
    let config = SupervisorConfig {
        port,
        health_timeout: Duration::from_secs(10),
        ..SupervisorConfig::default()
    };

    let warnings = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = warn_capture::Capture {
        warnings: std::sync::Arc::clone(&warnings),
    };
    let server = tracing::dispatcher::with_default(
        &tracing::Dispatch::new(capture),
        || common_core::runtime::block_on(Supervisor::boot(config)),
    );
    let Ok(server) = server else {
        panic!("second candidate serves");
    };
    assert!(server.is_live(), "boot waits for health");
    assert!(marker1.exists(), "first candidate ran");
    assert!(!marker3.exists(), "third candidate never runs after success");
    let warns = warnings.lock().expect("warnings").clone();
    assert_eq!(warns.len(), 1, "exactly one failure warns: {warns:?}");
    assert!(
        warns[0].contains(&dir1.display().to_string()),
        "warning names the failed first candidate: {:?}",
        warns[0]
    );
    assert!(
        !warns[0].contains(&dir2.display().to_string()),
        "warning never names the winner: {:?}",
        warns[0]
    );
    server.stop();

    match prior_server {
        Some(value) => std::env::set_var(LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
}

/// M11-S1: all-failing walk returns the last candidate's error.
/// Exec-format failure (spawn) vs immediate exit (exited before
/// healthy) keeps last-error identity observable; warnings arrive in
/// candidate order.
#[test]
#[cfg(unix)]
fn boot_returns_last_error_when_all_candidates_fail() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_server = std::env::var_os(LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    std::env::remove_var(LLAMA_SERVER_ENV);

    let base = fixture_dir();
    let dir1 = base.path().join("f1");
    let dir2 = base.path().join("f2");
    for dir in [&dir1, &dir2] {
        std::fs::create_dir(dir).expect("candidate dir");
    }
    // No shebang, no ELF: exec fails with "Exec format error".
    std::fs::write(dir1.join("llama-server"), "not an executable\n")
        .expect("exec-format stub writes");
    std::fs::write(dir2.join("llama-server"), "#!/bin/sh\nexit 1\n")
        .expect("immediate-exit stub writes");
    for dir in [&dir1, &dir2] {
        std::fs::set_permissions(dir.join("llama-server"), std::fs::Permissions::from_mode(0o755))
            .expect("stub mode");
    }
    let paths = vec![dir1.clone(), dir2.clone()];
    // Isolated PATH: the real `llama-server` on this machine must not
    // become a third candidate after both fixtures fail. Both stubs
    // use absolute `#!/bin/sh`, so no PATH lookup is needed.
    std::env::set_var("PATH", std::env::join_paths(paths).expect("join"));

    let port = free_port().expect("fixed free port");
    let config = SupervisorConfig {
        port,
        health_timeout: Duration::from_secs(2),
        ..SupervisorConfig::default()
    };
    let warnings = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = warn_capture::Capture {
        warnings: std::sync::Arc::clone(&warnings),
    };
    let error = tracing::dispatcher::with_default(
        &tracing::Dispatch::new(capture),
        || common_core::runtime::block_on(Supervisor::boot(config)),
    );
    let Err(error) = error else {
        panic!("all candidates fail");
    };
    let text = error.to_string();
    assert!(
        text.contains("exited before healthy"),
        "last error is the immediate-exit shape: {text}"
    );
    assert!(
        !text.contains("Exec format error"),
        "first error is not returned: {text}"
    );
    let warns = warnings.lock().expect("warnings").clone();
    assert_eq!(warns.len(), 2, "both failures warn: {warns:?}");
    assert!(
        warns[0].contains(&dir1.display().to_string()),
        "first warning names the first candidate: {:?}",
        warns[0]
    );
    assert!(
        warns[1].contains(&dir2.display().to_string()),
        "second warning names the second candidate: {:?}",
        warns[1]
    );

    match prior_server {
        Some(value) => std::env::set_var(LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
}

/// M11-S1: no candidates — empty search reports `NoBinary` naming
/// `$PATH`, without spawning anything.
#[test]
fn boot_with_no_candidates_reports_no_binary() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_server = std::env::var_os(LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    std::env::remove_var(LLAMA_SERVER_ENV);
    let empty = fixture_dir();
    std::env::set_var("PATH", empty.path());
    let config = SupervisorConfig {
        port: free_port().expect("port"),
        health_timeout: Duration::from_secs(1),
        ..SupervisorConfig::default()
    };
    let error = common_core::runtime::block_on_shared(Supervisor::boot(config));
    let Err(error) = error else {
        panic!("no candidates must fail");
    };
    assert!(
        error.to_string().contains("not found in $PATH"),
        "names the remedy: {error}"
    );
    match prior_server {
        Some(value) => std::env::set_var(LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
}
/// to nothing (no search, no serve) and reports `NoBinary` naming
/// `$PATH` — pin before the shared-helper migration.
#[test]
fn relative_llama_override_resolves_to_nothing() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior = std::env::var_os(LLAMA_SERVER_ENV);
    std::env::set_var(LLAMA_SERVER_ENV, "relative/llama-server");
    assert_eq!(resolve_binary(), None, "relative explicit never serves");
    assert!(
        resolve_binary_candidates().is_empty(),
        "relative explicit skips the search"
    );
    let error = resolve_binary_validated(orator_core::paths::current_uid())
        .expect_err("relative explicit is not found");
    assert!(error.to_string().contains("not found in $PATH"), "{error}");
    match prior {
        Some(value) => std::env::set_var(LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(LLAMA_SERVER_ENV),
    }
}

/// M6-S1 characterization: a searched hit owned by another uid is
/// refused with the `owned by uid` string — pin before the
/// shared-helper migration.
#[test]
#[cfg(unix)]
fn uid_mismatch_refuses_with_owner_string() {
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
    paths.extend(std::env::split_paths(
        &prior_path.clone().unwrap_or_default(),
    ));
    std::env::set_var("PATH", std::env::join_paths(paths).expect("join"));
    let foreign_uid = orator_core::paths::current_uid().wrapping_add(1);
    let error = resolve_binary_validated(foreign_uid).expect_err("foreign uid is refused");
    assert!(error.to_string().contains("owned by uid"), "{error}");
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
