//! Session multiplexing: real leases on session handles, one shared
//! bound across voice and text turns, per-session ordering, shutdown
//! drain. All against stub servers — never models, never wall-clock
//! audio. Timing assertions are generous bands, never exact.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::voice::{VoiceError, VoiceOwner};

fn leases(owner: &VoiceOwner) -> u32 {
    common_core::sync::lock(&owner.state).leases
}

fn voiced_audio_server() -> httpmock::MockServer {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).body(format!(
            "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n\
             data: [DONE]\n",
            one_f32()
        ));
    });
    server
}

fn slow_audio_server(delay_ms: u64) -> httpmock::MockServer {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200)
            .body(format!(
                "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n\
             data: [DONE]\n",
                one_f32()
            ))
            .delay(Duration::from_millis(delay_ms));
    });
    server
}

fn one_f32() -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(2.5f32.to_le_bytes())
}

#[test]
fn session_handles_carry_leases() {
    let owner = VoiceOwner::disabled();
    assert_eq!(leases(&owner), 0);
    let first = owner.start_session("assistant").expect("start");
    assert_eq!(leases(&owner), 1, "checkout on start");
    let second = owner.start_session("dictation").expect("start");
    assert_eq!(leases(&owner), 2, "one lease per live session");
    owner.stop_session(&first).expect("stop");
    assert_eq!(leases(&owner), 1, "entry drop releases its lease");
    owner.stop_session(&second).expect("stop");
    assert_eq!(leases(&owner), 0, "last release returns to zero");
    owner.shutdown();
}

#[test]
fn held_lease_holds_past_idle_released_reaps() {
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/llama-liquid-audio-server"),
        Duration::from_secs(1),
    );
    let id = owner.start_session("assistant").expect("start");
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.server = Some(orator_agent::llm::Supervisor::attach(
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
            state.server.is_some(),
            "a held lease holds the server past the idle bound"
        );
    }
    owner.stop_session(&id).expect("stop");
    {
        let mut state = common_core::sync::lock(&owner.state);
        VoiceOwner::reap_if_idle(
            &mut state,
            Duration::from_secs(1),
            std::time::Instant::now(),
        );
        assert!(state.server.is_none(), "released leases reap when idle");
    }
    owner.shutdown();
}

#[test]
fn contention_two_complete_third_refuses_fast() {
    let server = slow_audio_server(500);
    let url = server.base_url();
    let owner = Arc::new(VoiceOwner::disabled());
    let first = owner.start_session("assistant").expect("start");
    let second = owner.start_session("assistant").expect("start");
    let done = std::sync::atomic::AtomicU32::new(0);
    let refused = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for session in [&first, &second, &first] {
            let (owner, url) = (Arc::clone(&owner), url.clone());
            let done = &done;
            let refused = &refused;
            scope.spawn(move || match owner.say_via(&url, "hello", Some(session)) {
                Ok((samples, _)) => {
                    assert_eq!(samples.len(), 1);
                    done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                Err(VoiceError::Saturated(reason)) => {
                    common_core::sync::lock(refused).push(reason);
                }
                Err(other) => panic!("only saturation refuses here: {other:?}"),
            });
        }
    });
    assert_eq!(done.load(std::sync::atomic::Ordering::SeqCst), 2);
    let refused = common_core::sync::lock(&refused);
    assert_eq!(refused.len(), 1, "K+1 refuses fast");
    assert!(
        refused[0].contains("saturated"),
        "refusal names its cause: {}",
        refused[0]
    );
    owner.stop_session(&first).expect("stop");
    owner.stop_session(&second).expect("stop");
    assert_eq!(leases(&owner), 0);
    owner.shutdown();
}

#[test]
fn slow_session_never_blocks_fast_session() {
    let slow = slow_audio_server(800);
    let fast = voiced_audio_server();
    let owner = Arc::new(VoiceOwner::disabled());
    let (slow_url, fast_url) = (slow.base_url(), fast.base_url());
    let began = Instant::now();
    let (slow_done, fast_ms) = std::thread::scope(|scope| {
        let owner_slow = Arc::clone(&owner);
        let slow_turn = scope.spawn(move || {
            owner_slow
                .say_via(&slow_url, "hello", None)
                .expect("slow turn serves")
        });
        let owner_fast = Arc::clone(&owner);
        let fast_turn = scope.spawn(move || {
            let started = Instant::now();
            let out = owner_fast
                .say_via(&fast_url, "hello", None)
                .expect("fast turn serves");
            (out, started.elapsed())
        });
        let (_, (out, elapsed)) = (
            slow_turn.join().expect("slow done"),
            fast_turn.join().expect("fast done"),
        );
        let _ = out;
        (true, elapsed)
    });
    assert!(slow_done);
    assert!(
        fast_ms < Duration::from_millis(600),
        "fast session never waits behind the slow one: {fast_ms:?} (total {began:?})"
    );
    owner.shutdown();
}

#[test]
fn ask_counts_toward_the_same_bound() {
    let server = slow_audio_server(500);
    let url = server.base_url();
    let owner = Arc::new(VoiceOwner::disabled());
    let barrier = Arc::new(std::sync::Barrier::new(3));
    std::thread::scope(|scope| {
        for _ in 0..2 {
            let (owner, url, barrier) = (Arc::clone(&owner), url.clone(), Arc::clone(&barrier));
            scope.spawn(move || {
                barrier.wait();
                let _ = owner.say_via(&url, "hello", None);
            });
        }
        barrier.wait();
        // Both slow turns admitted: spin until the bound is held.
        let deadline = Instant::now() + Duration::from_secs(5);
        while owner.gate.inflight() < 2 {
            assert!(Instant::now() < deadline, "slow turns must admit");
            std::thread::sleep(Duration::from_millis(5));
        }
        let error = owner
            .ask_via("http://127.0.0.1:9", "model", 1024, 64, "hi", None)
            .expect_err("text shares the bound");
        assert!(
            matches!(error, VoiceError::Saturated(_)),
            "ask refuses past the bound, never dials: {error:?}"
        );
    });
    owner.shutdown();
}

#[test]
fn shutdown_drains_sessions_leases_and_server() {
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/llama-liquid-audio-server"),
        Duration::from_secs(60),
    );
    let first = owner.start_session("assistant").expect("start");
    let _ = owner.start_session("meeting");
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.server = Some(orator_agent::llm::Supervisor::attach(
            "http://127.0.0.1:9".to_string(),
        ));
    }
    assert_eq!(leases(&owner), 2);
    owner.shutdown();
    assert!(
        owner.list_sessions().is_empty(),
        "shutdown stops every session"
    );
    assert_eq!(leases(&owner), 0, "drained entries release");
    assert!(
        !owner.has_server(),
        "shutdown reaps the child, never leaks it"
    );
    // Shutdown is sticky and idempotent.
    owner.shutdown();
    let _ = first;
}

#[test]
fn mid_turn_shutdown_reaps_everything_owned() {
    // No-leak golden: shutdown mid-turn with sessions active reaps the
    // fake child, drains the sessions and leases, and the in-flight
    // turn still completes boundedly. Fake binary, never real models.
    let server = slow_audio_server(500);
    let url = server.base_url();
    let owner = Arc::new(VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/audio-server"),
        Duration::from_secs(60),
    ));
    let first = owner.start_session("assistant").expect("start");
    let _ = owner.start_session("dictation").expect("start");
    {
        let mut state = common_core::sync::lock(&owner.state);
        state.server = Some(orator_agent::llm::Supervisor::attach(
            "http://127.0.0.1:9".to_string(),
        ));
    }
    let worker = {
        let (owner, url) = (Arc::clone(&owner), url.clone());
        std::thread::spawn(move || owner.say_via(&url, "hello", Some(&first)))
    };
    // Wait until the turn is admitted, then shut down mid-turn.
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.gate.inflight() == 0 {
        assert!(Instant::now() < deadline, "slow turn must admit");
        std::thread::sleep(Duration::from_millis(5));
    }
    owner.shutdown();
    let outcome = worker.join().expect("in-flight turn completes boundedly");
    assert!(
        outcome.is_ok(),
        "shutdown never strands a turn: {outcome:?}"
    );
    assert!(
        owner.list_sessions().is_empty(),
        "shutdown stops every session"
    );
    assert_eq!(
        common_core::sync::lock(&owner.state).leases,
        0,
        "drained entries release"
    );
    assert!(!owner.has_server(), "the child is reaped, never leaked");
    let error =
        common_core::runtime::block_on(owner.ensure()).expect_err("post-shutdown ensures fail");
    assert!(
        matches!(error, VoiceError::ShuttingDown),
        "shutdown latches: {error:?}"
    );
}

#[test]
fn concurrent_sessions_multiplex_over_one_shared_endpoint() {
    // No per-session servers: two live sessions drive turns against
    // the same stub endpoint concurrently and both are served — the
    // admission bound and ejection policy compose underneath.
    let server = voiced_audio_server();
    let url = server.base_url();
    let owner = Arc::new(VoiceOwner::disabled());
    let first = owner.start_session("assistant").expect("start");
    let second = owner.start_session("dictation").expect("start");
    let done = std::sync::atomic::AtomicU32::new(0);
    std::thread::scope(|scope| {
        for session in [&first, &second] {
            let (owner, url) = (Arc::clone(&owner), url.clone());
            let done = &done;
            scope.spawn(move || {
                let (samples, rate) = owner
                    .say_via(&url, "hello", Some(session))
                    .expect("shared endpoint serves every session");
                assert_eq!(samples.len(), 1);
                assert_eq!(rate, orator_voice::s2s::AUDIO_OUT_RATE);
                done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
    });
    assert_eq!(done.load(std::sync::atomic::Ordering::SeqCst), 2);
    owner.stop_session(&first).expect("stop");
    owner.stop_session(&second).expect("stop");
    owner.shutdown();
}
