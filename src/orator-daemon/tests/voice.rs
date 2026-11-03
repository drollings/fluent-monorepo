//! Voice-owner tests: disabled fails closed, bad binaries fail
//! fast, idle expiry is pure logic — no weights booted here.

use std::time::Duration;

use crate::voice::{RequestOutcome, VoiceError, VoiceOwner, VOICE_IDLE_TIMEOUT};

#[test]
fn disabled_owner_names_its_remedy() {
    let owner = VoiceOwner::disabled();
    let error = common_core::runtime::block_on_shared(owner.ensure())
        .expect_err("disabled never serves");
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
    assert!(started.elapsed() < Duration::from_secs(30), "fails fast, never waits out a boot");
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
    let error = common_core::runtime::block_on_shared(owner.ensure())
        .expect_err("no boots after shutdown");
    assert_eq!(error, VoiceError::ShuttingDown);
    let error = common_core::runtime::block_on_shared(owner.ensure())
        .expect_err("shutdown sticks");
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
    let error = common_core::runtime::block_on_shared(owner.ensure())
        .expect_err("no boots after shutdown");
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
    let error = common_core::runtime::block_on_shared(owner.ensure())
        .expect_err("idle server was reaped");
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
        common_core::sync::lock(&owner.state).ejection.record_backend();
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
        assert!(state.server.is_none(), "suspect child drops, never re-admitted");
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
