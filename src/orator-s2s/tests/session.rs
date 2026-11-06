//! Session tests: scripted SSE turns (text + audio), abort, duplex
//! windowing — no server, no model.

use std::num::NonZeroU32;

use fluent_concurrency::stream::StreamAbort;
use orator_core::{DuplexEngine, Epoch, Frame, MediaTime};

use crate::config::S2sConfig;
use crate::session::{AudioSession, S2sError};

/// Deterministic voiced synth: harmonic sines with speech-like energy
/// and low zero-crossing rate, so the speech-presence gate passes and
/// scripted turns run without a microphone.
fn voiced(samples: usize) -> Vec<f32> {
    (0..samples)
        .map(|index| {
            let time = index as f32 / 16_000.0;
            0.35 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
                + 0.35 * (2.0 * std::f32::consts::PI * 300.0 * time).sin()
        })
        .collect()
}

/// Deterministic white-ish noise (xorshift): broadband energy the
/// detector's zero-crossing ceiling rejects as non-speech.
fn hiss(samples: usize, amplitude: f32) -> Vec<f32> {
    let mut state = 0x1234_5679u32;
    (0..samples)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state as f32 / u32::MAX as f32 - 0.5) * 2.0 * amplitude
        })
        .collect()
}

fn sse_turn(text: &str, audio_b64: Option<&str>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(encoded) = audio_b64 {
        let _ = write!(
            out,
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\",\"audio_chunk\":{{\"data\":\"{encoded}\"}}}}}}]}}"
        );
        out.push('\n');
    } else {
        let _ = write!(
            out,
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}"
        );
        out.push('\n');
    }
    out.push_str("data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n");
    out
}

fn one_f32() -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(2.5f32.to_le_bytes())
}

fn session_with(stub: &httpmock::MockServer) -> AudioSession {
    AudioSession::new(S2sConfig {
        endpoint: stub.base_url(),
        ..S2sConfig::default()
    })
}

#[test]
fn transcribe_collects_text_deltas() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("hello world", None));
        });
        let mut session = session_with(&server);
        let text = session
            .transcribe_blocking(&voiced(1600))
            .expect("asr runs");
        assert_eq!(text, "hello world");
        assert!(!session.heard_audio());
    });
}

#[test]
fn speak_collects_audio_and_reports_text_only() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("hi", Some(&one_f32())));
        });
        let mut session = session_with(&server);
        let samples = session.speak_blocking("hi").expect("tts runs");
        assert_eq!(samples, vec![2.5]);
        assert!(session.heard_audio());

        let quiet = httpmock::MockServer::start();
        quiet.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("text only", None));
        });
        let mut session = session_with(&quiet);
        assert_eq!(
            session.speak_blocking("hi").expect_err("no audio reports"),
            S2sError::NoAudio
        );
    });
}

#[test]
fn interleaved_chat_resets_once_then_holds_history() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("ok", Some(&one_f32())));
        });
        let mut session = session_with(&server);
        let abort = StreamAbort::new();
        let first = session
            .chat_turn(Some("hello"), None, &abort)
            .expect("turn one");
        assert!(first.voiced);
        session
            .chat_turn(Some("again"), None, &abort)
            .expect("turn two");
        assert_eq!(mock.hits(), 2);
        session.reset();
        session
            .chat_turn(Some("fresh"), None, &abort)
            .expect("post-reset");
        assert_eq!(mock.hits(), 3);
    });
}

#[test]
fn history_rolls_under_a_cap_keeping_newest() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("ok", Some(&one_f32())));
        });
        let mut session = session_with(&server);
        let abort = StreamAbort::new();
        for index in 0..70 {
            session
                .chat_turn(Some(&format!("turn {index}")), None, &abort)
                .expect("turn runs");
        }
        assert!(
            session.history.len() <= crate::session::MAX_HISTORY_ENTRIES,
            "history bounded: {}",
            session.history.len()
        );
        let tail: Vec<String> = session
            .history
            .iter()
            .rev()
            .take(2)
            .map(ToString::to_string)
            .collect();
        assert!(
            tail.iter().any(|entry| entry.contains("turn 69")),
            "newest turn protected: {tail:?}"
        );
    });
}

#[test]
fn endpoint_errors_name_the_model_body_and_served_set() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(400).json_body(serde_json::json!({
            "error": {"message": "model 'lfm2.5-audio-1.5b' not found", "type": "invalid_request_error"},
        }));
    });
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "liquidai/lfm3-audio-2b"}],
            }));
        });
        let mut session = session_with(&server);
        let error = session
            .speak_blocking("hi")
            .expect_err("unknown model fails");
        let message = error.to_string();
        assert!(message.contains("400"), "{message}");
        assert!(message.contains("lfm2.5-audio-1.5b"), "{message}");
        assert!(message.contains("not found"), "{message}");
        assert!(message.contains("liquidai/lfm3-audio-2b"), "{message}");
    });
}

#[test]
fn cancelled_turn_never_dials() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("x", None));
        });
        let mut session = session_with(&server);
        let abort = StreamAbort::new();
        abort.cancel();
        assert_eq!(
            session
                .chat_turn(Some("hi"), None, &abort)
                .expect_err("cancelled"),
            S2sError::Aborted
        );
        mock.assert_hits(0);
    });
}

#[test]
fn duplex_buffers_until_the_window_then_turns() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("reply", Some(&one_f32())));
        });
        let mut session = AudioSession::new(S2sConfig {
            endpoint: server.base_url(),
            turn_window_ms: 100,
            ..S2sConfig::default()
        });
        let rate = NonZeroU32::new(16_000).expect("rate");
        let frame = Frame {
            samples: std::sync::Arc::new(vec![0.1; 320]),
            channel: orator_core::Channel::Mic,
            sample_rate: 16_000,
            stamp: MediaTime::zero(rate),
        };
        // Six 20 ms frames stay under the 100 ms cap once the VAD attack
        // latches (the first two hot frames open the detector, the next
        // four buffer): silence, no dial.
        for _ in 0..6 {
            let out = session.step(&frame).expect("buffers");
            assert!(out.audio.samples.is_empty());
            assert!(!out.end_of_turn);
        }
        mock.assert_hits(0);
        let out = session.step(&frame).expect("window turns");
        assert_eq!(*out.audio.samples, vec![2.5]);
        assert_eq!(out.audio.sample_rate, 24_000);
        mock.assert_hits(1);
        let _ = Epoch(0);
    });
}

/// A stub that writes each SSE line as its own packet with a beat
/// between them, proving the session reassembles turns across
/// chunks instead of buffering the whole body.
fn chunked_stub(lines: Vec<String>, hold_open_secs: u64) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .expect("timeout");
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).is_err() || request.len() > 1_000_000 {
                break;
            }
            request.extend_from_slice(&byte);
        }
        let body: String = lines.concat();
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        // One packet per line: the session must parse incrementally.
        stream.write_all(head.as_bytes()).expect("head");
        for line in &lines {
            stream.write_all(line.as_bytes()).expect("line");
            stream.flush().expect("flush");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Hold the connection open past DONE: a buffering reader would
        // hang here; the streaming reader returns at DONE.
        std::thread::sleep(std::time::Duration::from_secs(hold_open_secs));
    });
    (format!("http://127.0.0.1:{port}"), server)
}

#[test]
fn turn_reassembles_across_separate_chunks() {
    let text_line = "data: {\"choices\":[{\"delta\":{\"content\":\"hello world\"}}]}\n".to_string();
    let audio_line = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n",
        one_f32()
    );
    let done_line = "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n".to_string();
    let (url, server) = chunked_stub(vec![text_line, audio_line, done_line], 0);
    let mut session = AudioSession::new(S2sConfig {
        endpoint: url,
        ..S2sConfig::default()
    });
    let text = session
        .transcribe_blocking(&voiced(1600))
        .expect("asr runs");
    assert_eq!(text, "hello world");
    server.join().expect("stub done");
}

#[test]
fn done_returns_without_waiting_for_close() {
    // The stub holds the connection open 30 s past DONE; the turn
    // must return promptly anyway.
    let (url, server) = chunked_stub(
        vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\",\"audio_chunk\":{\"data\":\"\"}}]}}\n"
                .to_string(),
            format!(
                "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n",
                one_f32()
            ),
            "data: [DONE]\n".to_string(),
        ],
        30,
    );
    let mut session = AudioSession::new(S2sConfig {
        endpoint: url,
        ..S2sConfig::default()
    });
    let started = std::time::Instant::now();
    let samples = session.speak_blocking("hi").expect("tts runs");
    assert_eq!(samples, vec![2.5]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "DONE ends the turn; no wait for close"
    );
    // The stub still holds the connection open: detach it rather than
    // joining a 30 s sleeper. Its only job — outliving the turn — is
    // done; the thread exits on its own.
    let _ = server;
}

#[test]
fn abort_cuts_a_hanging_stream() {
    use std::io::{Read, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).is_err() || request.len() > 1_000_000 {
                break;
            }
            request.extend_from_slice(&byte);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n")
            .expect("head");
        stream.flush().expect("flush");
        // Never send a body: the turn hangs until the client aborts.
        std::thread::sleep(std::time::Duration::from_secs(30));
    });
    let mut session = AudioSession::new(S2sConfig {
        endpoint: format!("http://127.0.0.1:{port}"),
        ..S2sConfig::default()
    });
    let abort = session.abort_handle();
    let worker = std::thread::spawn(move || session.speak_blocking("hi"));
    std::thread::sleep(std::time::Duration::from_millis(300));
    abort.cancel();
    let outcome = worker.join().expect("worker done");
    assert_eq!(
        outcome.expect_err("hanging stream aborts"),
        S2sError::Aborted
    );
    // The stub outlives the turn by design: detach it.
    let _ = server;
}

#[test]
fn oversized_audio_errors_instead_of_growing() {
    let audio_line = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n",
        one_f32()
    );
    let (url, server) = chunked_stub(
        vec![audio_line.clone(), audio_line, "data: [DONE]\n".to_string()],
        0,
    );
    let mut session = AudioSession::new(S2sConfig {
        endpoint: url,
        max_audio_samples: 1,
        ..S2sConfig::default()
    });
    let error = session.speak_blocking("hi").expect_err("over cap errors");
    assert!(
        matches!(error, S2sError::Overflow(_)),
        "bounded, never silent: {error:?}"
    );
    assert!(error.to_string().contains("bound"), "{error}");
    server.join().expect("stub done");
}

#[test]
fn duplex_step_aborts_on_the_session_signal_then_rearms() {
    let (url, server) = chunked_stub(vec![sse_turn("reply", Some(&one_f32()))], 0);
    let mut session = AudioSession::new(S2sConfig {
        endpoint: url,
        turn_window_ms: 100,
        ..S2sConfig::default()
    });
    session.abort_handle().cancel();
    let rate = NonZeroU32::new(16_000).expect("rate");
    let frame = Frame {
        samples: std::sync::Arc::new(vec![0.1; 1600]),
        channel: orator_core::Channel::Mic,
        sample_rate: 16_000,
        stamp: MediaTime::zero(rate),
    };
    assert_eq!(
        session.step(&frame).expect_err("cancelled step aborts"),
        orator_core::DuplexError::Aborted
    );
    // One barge-in cancels exactly the in-flight turn: the next
    // window turns without another cancel (seven 20 ms hot frames to
    // latch the VAD attack and fill the 100 ms cap).
    let small = Frame {
        samples: std::sync::Arc::new(vec![0.1; 320]),
        channel: orator_core::Channel::Mic,
        sample_rate: 16_000,
        stamp: MediaTime::zero(rate),
    };
    let mut out = None;
    for _ in 0..7 {
        match session.step(&small) {
            Ok(done) if !done.audio.samples.is_empty() => {
                out = Some(done);
                break;
            }
            Ok(_) => {}
            Err(error) => panic!("re-armed step must turn, got {error:?}"),
        }
    }
    let out = out.expect("re-armed step turns");
    assert_eq!(*out.audio.samples, vec![2.5]);
    server.join().expect("stub done");
}

/// The CLI condition: a bare thread with no runtime at all. A plain
/// `std` stub serves one canned SSE turn; the speech call must
/// round-trip instead of panicking on the client timeout setup.
/// (Regression: `RequestBuilder::send()` arms its timeout eagerly at
/// construction, so building it in a `block_on` argument panics.)
#[test]
fn speech_round_trips_without_any_runtime() {
    use std::io::{Read, Write as _};
    let sse = sse_turn("hi there", Some(&one_f32()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .expect("timeout");
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).is_err() || request.len() > 1_000_000 {
                break;
            }
            request.extend_from_slice(&byte);
        }
        let body = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
            sse.len()
        );
        stream.write_all(body.as_bytes()).expect("respond");
    });
    let mut session = AudioSession::new(S2sConfig {
        endpoint: format!("http://127.0.0.1:{port}"),
        ..S2sConfig::default()
    });
    let samples = session.speak_blocking("hi there").expect("round trip");
    assert_eq!(samples, vec![2.5]);
    server.join().expect("stub done");
}

#[test]
fn audio_input_unsupported_matches_the_pinned_transcript() {
    let gateway_400 = S2sError::Backend(
        "HTTP 400 Bad Request from http://127.0.0.1:8080/v1/chat/completions \
         for model 'm': audio input is not supported by this server; \
         endpoint serves: [liquidai/lfm3-audio-2b]"
            .to_string(),
    );
    assert!(crate::session::audio_input_unsupported(&gateway_400));
}

#[test]
fn audio_input_unsupported_rejects_other_failures() {
    let controls = [
        S2sError::Backend(
            "HTTP 400 Bad Request from u for model 'm': model 'm' not found".to_string(),
        ),
        S2sError::Backend("HTTP 400 Bad Request from u: unexpected field".to_string()),
        S2sError::Backend("HTTP 500 Internal Server Error from u: sick".to_string()),
        S2sError::Backend("stream read: connection closed".to_string()),
        S2sError::NoAudio,
        S2sError::Aborted,
        S2sError::Overflow("2 samples exceeds the 1-sample bound".to_string()),
    ];
    for control in &controls {
        assert!(
            !crate::session::audio_input_unsupported(control),
            "must not take the audio fallback: {control:?}"
        );
    }
}

#[test]
fn voiced_turns_feed_the_first_audio_histogram() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("hi", Some(&one_f32())));
        });
        let mut session = session_with(&server);
        session.speak_blocking("hi").expect("tts runs");
        assert_eq!(session.first_audio_latency().count(), 1);
        assert_eq!(session.no_audio_turns(), 0);
    });
}

#[test]
fn text_only_turns_count_no_audio_without_histograms() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("text only", None));
        });
        let mut session = session_with(&server);
        assert_eq!(
            session.speak_blocking("hi").expect_err("no audio reports"),
            S2sError::NoAudio
        );
        assert_eq!(session.no_audio_turns(), 1);
        assert_eq!(session.first_audio_latency().count(), 0);
    });
}

#[test]
fn failed_turn_invalidates_the_hint_cache() {
    use crate::ensure::invalidate_hint_cache;
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::ensure::invalidate_hint_cache(&server.base_url());
        let models = server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "swap-model"}],
            }));
        });
        server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(500).body("sick");
        });
        // Prime the cache, then fail a turn: the failure must drop the
        // entry so the next lookup re-probes (at most one stale hit).
        assert!(crate::ensure::serves_model(&server.base_url(), "swap-model").await);
        models.assert_hits(1);
        let mut session = session_with(&server);
        let _ = session.transcribe_blocking(&voiced(1600));
        assert!(crate::ensure::serves_model(&server.base_url(), "swap-model").await);
        models.assert_hits(2);
        invalidate_hint_cache(&server.base_url());
    });
}

fn hot_frame() -> orator_core::Frame {
    use std::num::NonZeroU32;
    let rate = NonZeroU32::new(16_000).expect("rate");
    // Speech-like to the energy detector: loud, low zero-crossing.
    let samples: Vec<f32> = (0..320)
        .map(|index| {
            let time = index as f32 / 16_000.0;
            0.35 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
                + 0.35 * (2.0 * std::f32::consts::PI * 300.0 * time).sin()
        })
        .collect();
    orator_core::Frame {
        samples: std::sync::Arc::new(samples),
        channel: orator_core::Channel::Mic,
        sample_rate: 16_000,
        stamp: orator_core::MediaTime::zero(rate),
    }
}

fn silent_frame() -> orator_core::Frame {
    use std::num::NonZeroU32;
    let rate = NonZeroU32::new(16_000).expect("rate");
    orator_core::Frame {
        samples: std::sync::Arc::new(vec![0.0; 320]),
        channel: orator_core::Channel::Mic,
        sample_rate: 16_000,
        stamp: orator_core::MediaTime::zero(rate),
    }
}

#[test]
fn silence_emits_no_turn_and_no_dial() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::ensure::invalidate_hint_cache(&server.base_url());
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("hi", Some(&one_f32())));
        });
        let mut session = AudioSession::new(S2sConfig {
            endpoint: server.base_url(),
            turn_window_ms: 100,
            ..S2sConfig::default()
        });
        // Twenty seconds of silence: no turn, no HTTP.
        for _ in 0..1000 {
            let out = session.step(&silent_frame()).expect("silence buffers");
            assert!(out.audio.samples.is_empty());
            assert!(!out.end_of_turn);
        }
        mock.assert_hits(0);
        assert_eq!(session.pending_samples(), 0);
    });
}

#[test]
fn max_window_force_commits_a_long_turn() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::ensure::invalidate_hint_cache(&server.base_url());
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("reply", Some(&one_f32())));
        });
        let mut session = AudioSession::new(S2sConfig {
            endpoint: server.base_url(),
            turn_window_ms: 100,
            ..S2sConfig::default()
        });
        // Six hot frames stay under the 100 ms cap once the attack
        // latches: buffering, no dial.
        for _ in 0..6 {
            let out = session.step(&hot_frame()).expect("buffers");
            assert!(out.audio.samples.is_empty());
            assert!(!out.end_of_turn);
        }
        mock.assert_hits(0);
        assert!(session.pending_samples() > 0);
        let out = session.step(&hot_frame()).expect("cap turns");
        assert_eq!(*out.audio.samples, vec![2.5]);
        assert!(out.end_of_turn);
        mock.assert_hits(1);
    });
}

#[test]
fn partial_text_is_visible_before_the_turn_closes() {
    let text_line =
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial hello\"}}]}\n".to_string();
    let audio_line = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n",
        one_f32()
    );
    let done_line = "data: [DONE]\n".to_string();
    let (url, server) = chunked_stub(vec![text_line, audio_line, done_line], 0);
    let mut session = AudioSession::new(S2sConfig {
        endpoint: url,
        turn_window_ms: 100,
        ..S2sConfig::default()
    });
    // Feed the cap in the background: the 7th hot frame triggers the
    // slow turn while the main thread polls the partial.
    let live = session.live_text_handle();
    let worker = std::thread::spawn(move || {
        // Six frames buffer under the cap (attack latches on the third),
        // the seventh turns (blocking on the stub).
        for _ in 0..6 {
            let _ = session.step(&hot_frame());
        }
        session.step(&hot_frame())
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = false;
    while std::time::Instant::now() < deadline {
        let text = common_core::sync::lock(&live).clone();
        if text.contains("partial hello") {
            seen = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(seen, "partial text must be visible pre-close");
    let out = worker.join().expect("worker done").expect("turn runs");
    assert!(out.end_of_turn);
    assert_eq!(*out.audio.samples, vec![2.5]);
    server.join().expect("stub done");
}

#[test]
fn retired_epoch_frames_drop_with_a_counter() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("hi", Some(&one_f32())));
        });
        let mut session = AudioSession::new(S2sConfig {
            endpoint: server.base_url(),
            turn_window_ms: 10_000,
            ..S2sConfig::default()
        });
        // Five hot frames buffer under the huge cap (attack latches on
        // the third, the next three stay buffered).
        for _ in 0..5 {
            let _ = session.step(&hot_frame()).expect("buffers");
        }
        assert!(session.pending_samples() > 0);
        session.set_epoch(Epoch(1));
        assert_eq!(session.epoch_drops(), 1);
        assert_eq!(session.pending_samples(), 0);
        // Old-epoch audio never submits: silence stays silent, no dial.
        for _ in 0..20 {
            let out = session.step(&silent_frame()).expect("drops, never errors");
            assert!(out.audio.samples.is_empty());
        }
    });
}

fn reply_frame(loud: bool) -> orator_core::Frame {
    use std::num::NonZeroU32;
    let rate = NonZeroU32::new(16_000).expect("rate");
    let samples = if loud { vec![0.4; 320] } else { vec![0.0; 320] };
    orator_core::Frame {
        samples: std::sync::Arc::new(samples),
        channel: orator_core::Channel::Reply,
        sample_rate: 16_000,
        stamp: orator_core::MediaTime::zero(rate),
    }
}

#[test]
fn echo_reference_never_opens_a_turn() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::ensure::invalidate_hint_cache(&server.base_url());
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("hi", Some(&one_f32())));
        });
        let mut session = AudioSession::new(S2sConfig {
            endpoint: server.base_url(),
            turn_window_ms: 100,
            ..S2sConfig::default()
        });
        // Loud server-audio frames on the Reply channel: echo, not user.
        for _ in 0..20 {
            let out = session.step(&reply_frame(true)).expect("echo never errors");
            assert!(out.audio.samples.is_empty());
            assert!(!out.end_of_turn);
        }
        mock.assert_hits(0);
        assert_eq!(session.pending_samples(), 0);
        assert!(!session.is_voicing());
    });
}

#[test]
fn mic_speech_during_voicing_barges_the_next_turn() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::ensure::invalidate_hint_cache(&server.base_url());
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(sse_turn("reply", Some(&one_f32())));
        });
        let mut session = AudioSession::new(S2sConfig {
            endpoint: server.base_url(),
            turn_window_ms: 100,
            ..S2sConfig::default()
        });
        // First turn voices: seven hot frames to latch and fill the cap.
        let mut first = None;
        for _ in 0..7 {
            if let Ok(out) = session.step(&hot_frame()) {
                if !out.audio.samples.is_empty() {
                    first = Some(out);
                    break;
                }
            }
        }
        let first = first.expect("first turn voices");
        assert_eq!(*first.audio.samples, vec![2.5]);
        assert!(session.is_voicing());
        // The mic stays open while voicing: hot mic frames barge the
        // next turn instead of waiting for silence.
        let mut second = None;
        for _ in 0..7 {
            if let Ok(out) = session.step(&hot_frame()) {
                if !out.audio.samples.is_empty() {
                    second = Some(out);
                    break;
                }
            }
        }
        let second = second.expect("barge turn voices");
        assert_eq!(*second.audio.samples, vec![2.5]);
        assert!(session.is_voicing());
        mock.assert_hits(2);
    });
}

#[test]
fn fallback_advance_grid_with_must_not_fire_controls() {
    use crate::session::should_advance_chain;
    // Task-value gate: voicelessness and transport failure advance;
    // cancellation never advances (false-advance zero, missed-advance zero).
    for error in [
        S2sError::NoAudio,
        S2sError::Backend("connection refused".to_string()),
        S2sError::Backend("HTTP 500 Internal Server Error from u: sick".to_string()),
        S2sError::Overflow("2 samples exceeds the 1-sample bound".to_string()),
    ] {
        assert!(should_advance_chain(&error), "must advance: {error:?}");
    }
    // Controls that must not fire: cancelled turns never advance.
    // Usage/mistyped failures are a different type (`ExitCode::Usage`
    // at the CLI) and never reach this gate — compile-time control.
    assert!(!should_advance_chain(&S2sError::Aborted));
}

#[test]
fn long_error_bodies_cap_through_the_shared_truncator() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        let big = "e".repeat(orator_core::TOOL_OUTPUT_CHARS + 500);
        server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(500).body(big.clone());
        });
        let mut session = session_with(&server);
        let error = session
            .transcribe_blocking(&voiced(1600))
            .expect_err("500 fails");
        let text = error.to_string();
        assert!(
            text.contains("truncated,"),
            "long bodies cap with the shared marker: {text}"
        );
        assert!(
            text.chars().count() < big.chars().count(),
            "capped shorter than the body"
        );
    });
}

#[test]
fn loading_probe_names_only_the_transient_503() {
    use crate::session::is_loading_body;
    let loading = reqwest::StatusCode::SERVICE_UNAVAILABLE;
    assert!(is_loading_body(
        loading,
        "{\"error\":{\"message\":\"Loading model\",\"type\":\"unavailable_error\"}}"
    ));
    assert!(is_loading_body(loading, "loading MODEL weights"));
    assert!(!is_loading_body(
        loading,
        "{\"error\":{\"message\":\"unknown model\"}}"
    ));
    assert!(!is_loading_body(
        reqwest::StatusCode::NOT_FOUND,
        "Loading model"
    ));
    assert!(!is_loading_body(reqwest::StatusCode::OK, "Loading model"));
}

#[test]
fn post_until_loaded_passes_non_loading_through() {
    use crate::session::{post_until_loaded, S2sError};
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("POST").path("/probe");
            then.status(200).body("fine");
        });
        let client = reqwest::Client::new();
        let abort = StreamAbort::new();
        let body = serde_json::json!({ "input": "hi" });
        let response = post_until_loaded(
            &client,
            &format!("{}/probe", server.base_url()),
            &body,
            &abort,
            |error: reqwest::Error| S2sError::Backend(error.to_string()),
        )
        .expect("200 passes through");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    });
}

#[test]
fn post_until_loaded_stops_on_abort() {
    use crate::session::{post_until_loaded, S2sError};
    let client = reqwest::Client::new();
    let abort = StreamAbort::new();
    abort.cancel();
    let body = serde_json::json!({ "input": "hi" });
    let error = post_until_loaded(
        &client,
        "http://127.0.0.1:1/probe",
        &body,
        &abort,
        |error: reqwest::Error| S2sError::Backend(error.to_string()),
    )
    .expect_err("cancelled aborts");
    assert_eq!(error, S2sError::Aborted);
}

/// M5-S1 characterization: a non-loading 503 is terminal, not a wait.
/// The loader consumes the body for inspection, then re-issues the
/// POST exactly once and returns the terminal reply — pin the status
/// and the two-hit shape so the `PollWithBackoff` audit (M5-S3) cannot
/// silently change the retry count.
#[test]
fn post_until_loaded_returns_terminal_503_without_waiting() {
    use crate::session::{post_until_loaded, S2sError};
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        let terminal = server.mock(|when, then| {
            when.method("POST").path("/probe");
            then.status(503).body("{\"error\":\"unknown model\"}");
        });
        let client = reqwest::Client::new();
        let abort = StreamAbort::new();
        let body = serde_json::json!({ "input": "hi" });
        let began = std::time::Instant::now();
        let response = post_until_loaded(
            &client,
            &format!("{}/probe", server.base_url()),
            &body,
            &abort,
            |error: reqwest::Error| S2sError::Backend(error.to_string()),
        )
        .expect("terminal 503 returns, never waits");
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            began.elapsed() < std::time::Duration::from_secs(10),
            "no loading wait"
        );
        assert_eq!(
            terminal.hits(),
            2,
            "body inspection re-issues the POST exactly once"
        );
    });
}

#[test]
fn speech_gate_skips_inference_on_hiss_and_silence() {
    // Hiss at several energies answers empty without dialing: the
    // stub 500s if hit, so any network means the gate leaked.
    let server = httpmock::MockServer::start();
    let hits = server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(500).body("must not dial");
    });
    let mut session = session_with(&server);
    assert_eq!(
        session.transcribe_blocking(&[]).expect("empty is silence"),
        "",
        "empty input never dials"
    );
    assert_eq!(
        session
            .transcribe_blocking(&[0.0; 3200])
            .expect("zeros are silence"),
        "",
        "digital silence never dials"
    );
    // Observed hallucination band, plus loud hiss: the re-armed
    // verdict keeps even RMS-0.1 broadband hiss under the fraction
    // (measured grid).
    for amplitude in [0.001, 0.003, 0.01, 0.03, 0.1] {
        assert_eq!(
            session
                .transcribe_blocking(&hiss(48_000, amplitude))
                .expect("hiss is not speech"),
            "",
            "hiss at {amplitude} never dials"
        );
    }
    assert_eq!(hits.hits(), 0, "gate skipped every inference");
    // Quiet voiced input still dials: far-mic speech around RMS 0.03
    // opens spans at the gate floor (measured grid).
    let quiet: Vec<f32> = voiced(48_000).iter().map(|s| s * 0.1).collect();
    let before = hits.hits();
    let text = session.transcribe_blocking(&quiet).expect_err("500 fails");
    assert!(text.to_string().contains("500"), "{text}");
    assert_eq!(hits.hits(), before + 1, "quiet speech dials");
    // Voiced input at the same scale still dials: the gate keeps
    // quiet speech, not just loud speech.
    let text = session
        .transcribe_blocking(&voiced(1600))
        .expect_err("500 fails");
    assert!(text.to_string().contains("500"), "{text}");
    assert_eq!(hits.hits(), before + 2, "voiced input dials");
}

/// A loud pure tone is vowel-like to energy alone, so the gate must
/// hold it on the tonal verdict: no inference, no hallucination.
#[test]
fn speech_gate_skips_inference_on_loud_pure_tone() {
    let server = httpmock::MockServer::start();
    let hits = server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(500).body("must not dial");
    });
    let mut session = session_with(&server);
    // The verified shape: 440 Hz sine, peak ~0.3, three seconds.
    let tone: Vec<f32> = (0..48_000)
        .map(|index| 0.3 * (2.0 * std::f32::consts::PI * 440.0 * index as f32 / 16_000.0).sin())
        .collect();
    assert_eq!(
        session
            .transcribe_blocking(&tone)
            .expect("tone is not speech"),
        "",
        "loud pure tone never dials"
    );
    assert_eq!(hits.hits(), 0, "gate skipped every inference");
}

/// M15-S1: the session field surface. `describe()` is the schema
/// golden; the table pins accept (verbatim, numbers) and reject
/// (unknown, garbage numbers).
#[test]
fn session_field_surface_pins_schema_and_table() {
    use fluent_wvr::{Describable, FieldAccess};
    let mut session = AudioSession::new(S2sConfig {
        endpoint: "http://127.0.0.1:1".to_string(),
        ..S2sConfig::default()
    });
    assert_eq!(
        session.describe(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "description": "Turn mode" },
                "voice_prompt": { "type": "string", "description": "TTS voice prompt" },
                "turn_window_ms": { "type": "integer", "description": "Duplex turn window" }
            },
            "required": ["mode", "voice_prompt", "turn_window_ms"]
        })
    );
    assert_eq!(
        session.field_names(),
        &["mode", "voice_prompt", "turn_window_ms"]
    );
    session.set_field("mode", "cascade").expect("mode sets");
    assert_eq!(session.get_field("mode").expect("mode reads"), "cascade");
    session
        .set_field("turn_window_ms", "500")
        .expect("numbers set");
    session
        .set_field("turn_window_ms", "lots")
        .expect_err("garbage rejects");
    session.set_field("nope", "1").expect_err("unknown rejects");
    session.get_field("nope").expect_err("unknown reads reject");
}
