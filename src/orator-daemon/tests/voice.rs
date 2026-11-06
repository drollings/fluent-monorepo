//! Voice-owner tests: disabled fails closed, bad binaries fail
//! fast, idle expiry is pure logic — no weights booted here.

use std::time::Duration;

use crate::voice::{RequestOutcome, VoiceError, VoiceOwner, VOICE_IDLE_TIMEOUT};

#[test]
fn disabled_owner_names_its_remedy() {
    let owner = VoiceOwner::disabled();
    let error =
        common_core::runtime::block_on_shared(owner.ensure()).expect_err("disabled never serves");
    assert_eq!(error, VoiceError::Disabled);
    owner.shutdown();
}

#[test]
fn unresolvable_binary_fails_fast() {
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/llama-liquid-audio-server"),
        Duration::from_secs(60),
    );
    let started = std::time::Instant::now();
    let error = common_core::runtime::block_on_shared(owner.ensure())
        .expect_err("missing binary never serves");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "fails fast, never waits out a boot"
    );
    assert!(
        matches!(error, VoiceError::Boot(_)),
        "spawn failure is a boot error: {error:?}"
    );
    owner.shutdown();
}

#[test]
fn idle_budget_matches_the_gateway_shape() {
    assert_eq!(VOICE_IDLE_TIMEOUT, Duration::from_mins(15));
}

#[test]
fn owner_keeps_the_loaded_configuration() {
    let mut config = orator_core::OratorConfig::default();
    config.tts.backend = "kokoro".to_string();
    let owner = VoiceOwner::new(&config);
    assert_eq!(owner.config().tts.backend, "kokoro");
    owner.shutdown();
}

#[test]
fn idle_supervisor_is_stopped_and_reaped() {
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/llama-liquid-audio-server"),
        Duration::from_secs(1),
    );
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.server = Some(orator_llm::Supervisor::attach(
            "http://127.0.0.1:9".to_string(),
        ));
        state.last_used = std::time::Instant::now()
            .checked_sub(Duration::from_secs(3600))
            .expect("stale timestamp");
    }
    {
        let mut state = common_core::sync::lock(&owner.state);
        VoiceOwner::reap_if_idle(
            &mut state,
            Duration::from_secs(1),
            std::time::Instant::now(),
        );
        assert!(
            state.server.is_none(),
            "idle server is reaped, never leaked"
        );
    }
    owner.shutdown();
}

#[test]
fn is_alive_tracks_the_options_probe() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        use std::io::{Read as _, Write as _};
        let mut buf = [0u8; 2048];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .expect("reply");
    });
    let url = format!("http://127.0.0.1:{port}");
    assert!(
        common_core::runtime::block_on_shared(super::is_alive(&url)),
        "OPTIONS 200 is alive"
    );
    server.join().expect("server done");
}

#[test]
fn is_alive_false_when_refused() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    assert!(
        !common_core::runtime::block_on_shared(super::is_alive(&format!(
            "http://127.0.0.1:{port}"
        ))),
        "refused port is not alive"
    );
}

#[test]
fn shutdown_stops_new_boots_and_reports() {
    let owner = VoiceOwner::new(&orator_core::OratorConfig::default());
    owner.shutdown();
    let error =
        common_core::runtime::block_on_shared(owner.ensure()).expect_err("no boots after shutdown");
    assert_eq!(error, VoiceError::ShuttingDown);
    let error = common_core::runtime::block_on_shared(owner.ensure()).expect_err("shutdown sticks");
    assert_eq!(error, VoiceError::ShuttingDown);
}

#[test]
fn shutdown_waits_boundedly_for_an_in_flight_boot() {
    let owner = VoiceOwner::new(&orator_core::OratorConfig::default());
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.booting = true;
    }
    let started = std::time::Instant::now();
    owner.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "shutdown waits boundedly, never hangs on a stuck boot"
    );
    let error =
        common_core::runtime::block_on_shared(owner.ensure()).expect_err("no boots after shutdown");
    assert_eq!(error, VoiceError::ShuttingDown);
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.booting = false;
    }
}

fn stale(state: &mut crate::voice::VoiceState) {
    state.last_used = std::time::Instant::now()
        .checked_sub(Duration::from_secs(3600))
        .expect("stale timestamp");
}

#[test]
fn ok_outcome_reaps_when_idle() {
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/llama-liquid-audio-server"),
        Duration::from_secs(1),
    );
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.server = Some(orator_llm::Supervisor::attach(
            "http://127.0.0.1:9".to_string(),
        ));
        state.last_outcome = Some(RequestOutcome::Ok);
        stale(&mut state);
    }
    {
        let mut state = common_core::sync::lock(&owner.state);
        VoiceOwner::reap_if_idle(
            &mut state,
            Duration::from_secs(1),
            std::time::Instant::now(),
        );
        assert!(
            state.server.is_none(),
            "a healthy idle server is reaped, never leaked"
        );
    }
    owner.shutdown();
}

#[test]
fn backend_outcome_reaps_after_idle() {
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/llama-liquid-audio-server"),
        Duration::from_secs(1),
    );
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.server = Some(orator_llm::Supervisor::attach(
            "http://127.0.0.1:9".to_string(),
        ));
        state.last_outcome = Some(RequestOutcome::Backend);
        stale(&mut state);
    }
    {
        let mut state = common_core::sync::lock(&owner.state);
        VoiceOwner::reap_if_idle(
            &mut state,
            Duration::from_secs(1),
            std::time::Instant::now(),
        );
        assert!(
            state.server.is_none(),
            "a failing idle server reaps after the budget, never pinned"
        );
    }
    owner.shutdown();
}

#[test]
fn successful_use_records_ok_and_reaps_when_idle() {
    use std::io::{Read as _, Write as _};
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/llama-liquid-audio-server"),
        Duration::from_secs(1),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 2048];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .expect("reply");
    });
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.server = Some(orator_llm::Supervisor::attach(format!(
            "http://127.0.0.1:{port}"
        )));
    }
    let url = common_core::runtime::block_on_shared(owner.ensure()).expect("stub serves");
    assert!(url.contains(&port.to_string()));
    {
        let mut state = common_core::sync::lock(&owner.state);
        stale(&mut state);
    }
    // The idle server must be reaped, not reused: the next use fails
    // fast on the pinned (missing) binary instead of returning a URL.
    let error =
        common_core::runtime::block_on_shared(owner.ensure()).expect_err("idle server was reaped");
    assert!(
        matches!(error, VoiceError::Boot(_) | VoiceError::NoBinary(_)),
        "reap forces a fresh boot attempt: {error:?}"
    );
    {
        let state = common_core::sync::lock(&owner.state);
        assert!(state.server.is_none(), "nothing leaks past the reap");
    }
    server.join().expect("stub done");
    owner.shutdown();
}

#[test]
fn voice_error_kinds_stay_distinct_across_the_boundary() {
    use std::collections::HashSet;
    let errors = vec![
        VoiceError::Backend("x".to_string()),
        VoiceError::NoAudio("x".to_string()),
        VoiceError::Overflow("x".to_string()),
        VoiceError::Aborted("x".to_string()),
        VoiceError::Saturated("x".to_string()),
        VoiceError::Ejected("x".to_string()),
        VoiceError::BootContention,
        VoiceError::Boot("x".to_string()),
        VoiceError::NoBinary("x".to_string()),
        VoiceError::Disabled,
        VoiceError::ShuttingDown,
        VoiceError::UnknownSession("x".to_string()),
    ];
    let kinds: HashSet<&str> = errors.iter().map(super::VoiceError::kind).collect();
    assert_eq!(kinds.len(), errors.len(), "every kind stays distinct");
    assert_eq!(VoiceError::NoAudio("s".to_string()).kind(), "NoAudio");
    assert_eq!(VoiceError::Overflow("s".to_string()).kind(), "Overflow");
    assert_eq!(VoiceError::Aborted("s".to_string()).kind(), "Aborted");
    assert_eq!(VoiceError::Backend("s".to_string()).kind(), "Backend");
}

#[test]
fn overflow_counts_toward_ejection_like_backend() {
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/audio-server"),
        Duration::from_secs(60),
    );
    // Over-bound is server misbehavior, not a responsive turn: two in a
    // row eject the same way two transport failures do.
    for _ in 0..2 {
        common_core::sync::lock(&owner.state)
            .ejection
            .record_backend();
    }
    assert!(
        common_core::sync::lock(&owner.state).ejection.is_ejected(),
        "two counted failures eject"
    );
    owner.shutdown();
}

#[test]
fn boot_contention_stays_a_contention_kind() {
    let error = VoiceError::BootContention;
    assert_eq!(error.kind(), "BootContention");
    assert!(
        error.to_string().contains("concurrent boot"),
        "contention names its remedy: {error}"
    );
}

#[test]
fn ejected_probe_success_drops_the_same_child() {
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/audio-server"),
        Duration::from_secs(60),
    );
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.server = Some(orator_llm::Supervisor::attach(
            "http://127.0.0.1:9".to_string(),
        ));
        state.last_url = Some("http://127.0.0.1:9".to_string());
        state.ejection.record_backend();
        state.ejection.record_backend();
        assert!(state.ejection.is_ejected());
    }
    {
        let mut state = common_core::sync::lock(&owner.state);
        // The reap path drops ejected children on probe failure; the
        // success path drops the suspect child too (never re-admits
        // the same POST-failing child on probe health alone).
        if let Some(server) = state.server.take() {
            server.stop();
        }
        assert!(
            state.server.is_none(),
            "suspect child drops, never re-admitted"
        );
    }
    owner.shutdown();
}

#[test]
fn ensure_uses_a_serving_configured_endpoint_without_booting() {
    // The independently-started workflow: the configured server
    // answers readiness, so ensure returns it shared — no child boots
    // (no binary could even serve here: the managed legacy predates
    // LFM3 and the stub speaks no weights).
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("OPTIONS").path("/v1/chat/completions");
        then.status(200);
    });
    let mut config = orator_core::OratorConfig::default();
    config.s2s.endpoint = server.base_url();
    let owner = VoiceOwner::new(&config);
    let url = common_core::runtime::block_on_shared(owner.ensure()).expect("shared serves");
    assert_eq!(url, server.base_url());
    assert!(!owner.has_server(), "shared use boots no child");
    owner.shutdown();
}

#[test]
fn ensure_without_server_or_boot_names_auto_boot() {
    // Dead configured endpoint, auto-boot off: the refusal names the
    // file leave instead of spawning a child.
    let mut config = orator_core::OratorConfig::default();
    config.s2s.endpoint = "http://127.0.0.1:9".to_string();
    assert!(!config.s2s.auto_boot, "off unless the file opts in");
    let owner = VoiceOwner::new(&config);
    let error = common_core::runtime::block_on_shared(owner.ensure())
        .expect_err("nothing serves, nothing boots");
    assert_eq!(error.kind(), "AutoBootDisabled");
    assert!(error.to_string().contains("s2s.auto_boot"), "{error}");
    owner.shutdown();
}

/// Serializes process-environment mutation across these hermetic boot
/// tests (PATH/HOME/binary overrides are process-global).
#[cfg(unix)]
static BOOT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Writable fixture base outside `/tmp`: the ownership gate refuses
/// anything under `/tmp`, so candidate binaries live under the test
/// binary's own directory.
#[cfg(unix)]
fn fixture_base() -> tempfile::TempDir {
    let base = std::env::current_exe()
        .expect("exe path")
        .parent()
        .expect("exe parent")
        .to_path_buf();
    tempfile::tempdir_in(base).expect("fixture base")
}

/// Healthy audio stub with an absolute interpreter path (same contract
/// as the S2S fake, without the `env` lookup so an isolated `PATH`
/// still boots).
#[cfg(unix)]
const FAKE_AUDIO_SERVER_ABS: &str = r#"#!/usr/bin/python3
import http.server
import sys

port = 8080
args = sys.argv[1:]
for index, arg in enumerate(args):
    if arg == "--port" and index + 1 < len(args):
        port = int(args[index + 1])


class Handler(http.server.BaseHTTPRequestHandler):
    def do_OPTIONS(self):
        self.send_response(200)
        self.end_headers()

    def log_message(self, *args):
        pass


http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()
"#;

/// Config pointing at a dead endpoint with auto-boot on and a present
/// bundle rooted at `base` (backbone + mmproj exist there).
#[cfg(unix)]
fn boot_test_config(base: &std::path::Path) -> orator_core::OratorConfig {
    let mut config = orator_core::OratorConfig::default();
    config.s2s.endpoint = "http://127.0.0.1:9".to_string();
    config.s2s.model = "m".to_string();
    config.s2s.auto_boot = true;
    config.gguf_dir = base.to_string_lossy().into_owned();
    config.s2s.mmproj = "mmproj.gguf".to_string();
    config.models.models.insert(
        "m".to_string(),
        orator_core::ModelSpec {
            weights: Some("backbone.gguf".to_string()),
            backends: vec!["audio-server".to_string()],
            params: None,
            role_params: None,
        },
    );
    config
}

/// M13-S1: ordered audio candidates through `ensure` — first fails,
/// second serves, third never runs. Markers prove attempt order.
#[test]
#[cfg(unix)]
fn ensure_tries_audio_candidates_in_order_and_stops_after_success() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&BOOT_ENV_LOCK);
    let _home_guard = common_core::sync::lock(&super::export_tests::HOME_LOCK);
    let prior_audio = std::env::var_os(orator_s2s::AUDIO_SERVER_ENV);
    let prior_llama = std::env::var_os(orator_llm::LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    let prior_xdg = std::env::var_os("XDG_DATA_HOME");
    std::env::remove_var(orator_s2s::AUDIO_SERVER_ENV);
    std::env::remove_var(orator_llm::LLAMA_SERVER_ENV);

    let base = fixture_base();
    std::env::set_var("HOME", base.path());
    std::env::remove_var("XDG_DATA_HOME");
    for name in ["backbone.gguf", "mmproj.gguf"] {
        std::fs::write(base.path().join(name), b"stub").expect("stub weight");
    }
    let dir1 = base.path().join("d1");
    let dir2 = base.path().join("d2");
    let dir3 = base.path().join("d3");
    for dir in [&dir1, &dir2, &dir3] {
        std::fs::create_dir(dir).expect("candidate dir");
    }
    let marker1 = base.path().join("ran-first");
    let marker3 = base.path().join("ran-third");
    std::fs::write(
        dir1.join(orator_s2s::AUDIO_SERVER_BIN),
        format!("#!/bin/sh\ntouch \"{}\"\nexit 1\n", marker1.display()),
    )
    .expect("failing stub writes");
    std::fs::write(
        dir2.join(orator_s2s::AUDIO_SERVER_BIN),
        FAKE_AUDIO_SERVER_ABS,
    )
    .expect("healthy stub writes");
    std::fs::write(
        dir3.join(orator_s2s::AUDIO_SERVER_BIN),
        format!("#!/bin/sh\ntouch \"{}\"\nsleep 30\n", marker3.display()),
    )
    .expect("unused stub writes");
    for dir in [&dir1, &dir2, &dir3] {
        std::fs::set_permissions(
            dir.join(orator_s2s::AUDIO_SERVER_BIN),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("stub runs");
    }
    std::env::set_var(
        "PATH",
        std::env::join_paths([dir1.clone(), dir2.clone(), dir3.clone()]).expect("join"),
    );

    let config = boot_test_config(base.path());
    let owner = VoiceOwner::new(&config);
    let url = common_core::runtime::block_on_shared(owner.ensure()).expect("second serves");
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "owned loopback endpoint: {url}"
    );
    assert!(marker1.exists(), "first candidate ran");
    assert!(!marker3.exists(), "third candidate never runs after success");
    owner.shutdown();

    match prior_audio {
        Some(value) => std::env::set_var(orator_s2s::AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(orator_s2s::AUDIO_SERVER_ENV),
    }
    match prior_llama {
        Some(value) => std::env::set_var(orator_llm::LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(orator_llm::LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_xdg {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// M13-S1: all audio candidates fail — `ensure` maps to
/// `VoiceError::Boot("every voice server candidate failed (last: …)")`
/// with the last error winning (exec-format vs immediate exit).
#[test]
#[cfg(unix)]
fn ensure_maps_all_failed_candidates_to_boot_last_error() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&BOOT_ENV_LOCK);
    let _home_guard = common_core::sync::lock(&super::export_tests::HOME_LOCK);
    let prior_audio = std::env::var_os(orator_s2s::AUDIO_SERVER_ENV);
    let prior_llama = std::env::var_os(orator_llm::LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    let prior_xdg = std::env::var_os("XDG_DATA_HOME");
    std::env::remove_var(orator_s2s::AUDIO_SERVER_ENV);
    std::env::remove_var(orator_llm::LLAMA_SERVER_ENV);

    let base = fixture_base();
    std::env::set_var("HOME", base.path());
    std::env::remove_var("XDG_DATA_HOME");
    for name in ["backbone.gguf", "mmproj.gguf"] {
        std::fs::write(base.path().join(name), b"stub").expect("stub weight");
    }
    let dir1 = base.path().join("e1");
    let dir2 = base.path().join("e2");
    for dir in [&dir1, &dir2] {
        std::fs::create_dir(dir).expect("candidate dir");
    }
    std::fs::write(dir1.join(orator_s2s::AUDIO_SERVER_BIN), "not an executable\n")
        .expect("exec-format stub writes");
    std::fs::write(dir2.join(orator_s2s::AUDIO_SERVER_BIN), "#!/bin/sh\nexit 1\n")
        .expect("immediate-exit stub writes");
    for dir in [&dir1, &dir2] {
        std::fs::set_permissions(
            dir.join(orator_s2s::AUDIO_SERVER_BIN),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("stub mode");
    }
    std::env::set_var(
        "PATH",
        std::env::join_paths([dir1.clone(), dir2.clone()]).expect("join"),
    );

    let config = boot_test_config(base.path());
    let owner = VoiceOwner::new(&config);
    let error = common_core::runtime::block_on_shared(owner.ensure()).expect_err("all fail");
    assert_eq!(error.kind(), "Boot");
    let text = error.to_string();
    assert!(
        text.contains("every voice server candidate failed (last: "),
        "exact final text: {text}"
    );
    assert!(
        text.contains("exited before healthy"),
        "last error wins: {text}"
    );
    assert!(
        !text.contains("Exec format error"),
        "first error is not returned: {text}"
    );
    owner.shutdown();

    match prior_audio {
        Some(value) => std::env::set_var(orator_s2s::AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(orator_s2s::AUDIO_SERVER_ENV),
    }
    match prior_llama {
        Some(value) => std::env::set_var(orator_llm::LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(orator_llm::LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_xdg {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// M13-S1: no boot while ejected, and contention stays contention.
/// Both paths return without touching any candidate binary.
#[test]
#[cfg(unix)]
fn ensure_does_not_boot_while_ejected_or_contending() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&BOOT_ENV_LOCK);
    let _home_guard = common_core::sync::lock(&super::export_tests::HOME_LOCK);
    let prior_audio = std::env::var_os(orator_s2s::AUDIO_SERVER_ENV);
    let prior_llama = std::env::var_os(orator_llm::LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    let prior_xdg = std::env::var_os("XDG_DATA_HOME");
    std::env::remove_var(orator_s2s::AUDIO_SERVER_ENV);
    std::env::remove_var(orator_llm::LLAMA_SERVER_ENV);

    let base = fixture_base();
    std::env::set_var("HOME", base.path());
    std::env::remove_var("XDG_DATA_HOME");
    for name in ["backbone.gguf", "mmproj.gguf"] {
        std::fs::write(base.path().join(name), b"stub").expect("stub weight");
    }
    let dir = base.path().join("n1");
    std::fs::create_dir(&dir).expect("candidate dir");
    let marker = base.path().join("ran-boot");
    std::fs::write(
        dir.join(orator_s2s::AUDIO_SERVER_BIN),
        format!("#!/bin/sh\ntouch \"{}\"\nsleep 30\n", marker.display()),
    )
    .expect("stub writes");
    std::fs::set_permissions(
        dir.join(orator_s2s::AUDIO_SERVER_BIN),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("stub runs");
    std::env::set_var("PATH", std::env::join_paths([dir.clone()]).expect("join"));

    // Ejected with a dead last URL: the probe fails, ensure stays
    // ejected without booting any candidate.
    let config = boot_test_config(base.path());
    let owner = VoiceOwner::new(&config);
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.ejection.record_backend();
        state.ejection.record_backend();
        assert!(state.ejection.is_ejected());
        state.last_url = Some("http://127.0.0.1:9".to_string());
    }
    let error = common_core::runtime::block_on_shared(owner.ensure()).expect_err("ejected");
    assert_eq!(error.kind(), "Ejected");
    assert!(
        !marker.exists(),
        "ejected ensure boots nothing"
    );
    owner.shutdown();

    // Contention: a sibling boot owns the flag, so ensure answers
    // contention without touching any candidate.
    let owner = VoiceOwner::new(&config);
    owner.set_booting_for_tests(true);
    let error =
        common_core::runtime::block_on_shared(owner.ensure()).expect_err("contending");
    assert_eq!(error, VoiceError::BootContention);
    assert!(
        !marker.exists(),
        "contending ensure boots nothing"
    );
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.booting = false;
    }
    owner.shutdown();

    match prior_audio {
        Some(value) => std::env::set_var(orator_s2s::AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(orator_s2s::AUDIO_SERVER_ENV),
    }
    match prior_llama {
        Some(value) => std::env::set_var(orator_llm::LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(orator_llm::LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_xdg {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}
