//! End-to-end goldens: the cascade flows, failures stay honest, and
//! configuration is honored — all hermetic, no models, no daemon.
//!
//! The cascade golden composes the real stages over stub transports:
//! VAD verdicts gate the turn, scripted completions stand in for the
//! supervised endpoints, notes roll up the transcript, diarization
//! attributes speakers, and the event store keeps the sequence. The
//! binary goldens run the built CLI against a dead socket and a
//! scratch home.

use std::path::PathBuf;
use std::sync::Arc;

use fluent_llm::protocol::{
    ChatMessage, LlmError as FluentError, LlmQueueConfig, LlmRequestQueue, LlmTask,
};
use orator_core::{Channel, DuplexEngine, Epoch, LlmSession, MediaTime, Sampler};

fn one_f32() -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(2.5f32.to_le_bytes())
}

fn scripted_queue(
    recorded: Arc<std::sync::Mutex<Vec<String>>>,
    reply: &'static str,
) -> Arc<LlmRequestQueue> {
    Arc::new(LlmRequestQueue::new(
        fluent_concurrency::tokio_runtime(),
        &LlmQueueConfig {
            worker_count: 2,
            queue_capacity: 64,
        },
        move |task: LlmTask| {
            let recorded = Arc::clone(&recorded);
            async move {
                let text = task
                    .messages
                    .iter()
                    .map(|message: &ChatMessage| message.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                common_core::sync::lock(&recorded).push(text);
                Ok::<String, FluentError>(reply.to_string())
            }
        },
    ))
}

fn speech_frame() -> Vec<f32> {
    // 150 Hz + 300 Hz at 16 kHz: loud, low zero-crossing — speech-like
    // to the energy detector, unlike pure tones or white noise.
    (0..320)
        .map(|index| {
            let time = index as f32 / 16_000.0;
            0.35 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
                + 0.35 * (2.0 * std::f32::consts::PI * 300.0 * time).sin()
        })
        .collect()
}

fn speech_span(frames: usize) -> Vec<f32> {
    // Multi-frame voiced utterance: the speech-presence gate scores
    // whole turns (attack run plus voiced span), so scripted turns
    // speak in spans, never single frames.
    let mut out = Vec::with_capacity(frames * 320);
    for _ in 0..frames {
        out.extend_from_slice(&speech_frame());
    }
    out
}

#[test]
fn cascade_turn_flows_from_speech_to_store() {
    common_core::runtime::block_on_shared(async {
        // VAD: speech opens on voiced frames, closes after hangover.
        let mut vad = orator_vad::EnergyVad::with_floor(0.02);
        for _ in 0..5 {
            vad.push(&speech_frame());
        }
        assert!(
            vad.push(&speech_frame()).speech,
            "voiced frames open speech"
        );
        for _ in 0..20 {
            vad.push(&vec![0.0; 320]);
        }
        assert!(!vad.push(&vec![0.0; 320]).speech, "silence closes speech");

        // STT stub: one scripted transcript.
        let asr = httpmock::MockServer::start();
        asr.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(
                "data: {\"choices\":[{\"delta\":{\"content\":\"book a table for two\"}}]}\n\
                 data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n",
            );
        });
        let mut listener = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
            endpoint: asr.base_url(),
            ..orator_s2s::S2sConfig::default()
        });
        let transcript = listener
            .transcribe_blocking(&speech_span(8))
            .expect("asr turn runs");
        assert_eq!(transcript, "book a table for two");

        // Supervised LLM stub: the transcript reaches the request.
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let queue = scripted_queue(Arc::clone(&recorded), "noted");
        let mut llm = orator_llm::HttpSession::with_queue(
            queue,
            orator_llm::SessionConfig {
                endpoint_base: "http://127.0.0.1:9".to_string(),
                model: "cascade-test".to_string(),
                ctx_tokens: 8192,
                n_batch: 512,
                system_prompt: "You are a terse assistant.".to_string(),
            },
        );
        llm.push_text(&transcript);
        let out = llm
            .step(&mut Sampler::default())
            .expect("scripted step succeeds");
        assert_eq!(out.delta, "noted");
        let requests = common_core::sync::lock(&recorded);
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].contains("book a table for two"),
            "transcript flows into the LLM request"
        );
        drop(requests);

        // TTS stub: the reply voices audio.
        let tts = httpmock::MockServer::start();
        tts.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(format!(
                "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n\
                 data: [DONE]\n",
                one_f32()
            ));
        });
        let mut speaker = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
            endpoint: tts.base_url(),
            ..orator_s2s::S2sConfig::default()
        });
        let samples = speaker.speak_blocking(&out.delta).expect("tts turn runs");
        assert_eq!(samples, vec![2.5]);
        assert!(speaker.heard_audio());

        // Notes roll the transcript up; diarization attributes speakers.
        let summary = orator_notes::roll_summary("", &transcript);
        assert!(summary.contains("book a table for two"), "{summary}");
        let facts = orator_notes::append_fact("", "reservation at seven");
        assert!(facts.contains("reservation at seven"), "{facts}");
        let segments = orator_diar::segment(Channel::Mic, 3200);
        assert!(!segments.is_empty(), "speech spans segments");
        let labels = vec![0usize; segments.len()];
        let mids: Vec<u64> = segments
            .iter()
            .map(|segment| u64::midpoint(segment.start_sample, segment.end_sample))
            .collect();
        let attributed = orator_diar::align_words(&mids, &segments, &labels);
        assert_eq!(attributed.len(), segments.len());
        assert!(attributed.iter().all(|label| *label == 0));
        assert_eq!(orator_diar::speaker_name(0), "Speaker 1");

        // Event store keeps the turn sequence.
        let log = orator_store::EventLog::in_memory().expect("memory log");
        log.append("s1", 0, "transcript", &transcript, 1)
            .expect("append transcript");
        log.append("s1", 0, "reply", &out.delta, 2)
            .expect("append reply");
        log.append("s1", 0, "note", &summary, 3)
            .expect("append note");
        let events = log.replay(0, 10).expect("replay");
        let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
        assert_eq!(kinds, vec!["transcript", "reply", "note"]);
        assert!(events[0].payload.contains("book a table for two"));
    });
}

fn cli_binary() -> PathBuf {
    std::env::var("CARGO_BIN_EXE_orator_cli").map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/orator-cli"),
        PathBuf::from,
    )
}

struct DeadEnv {
    _runtime: tempfile::TempDir,
    _home: tempfile::TempDir,
    _cwd: tempfile::TempDir,
}

fn dead_env(cmd: &mut std::process::Command) -> DeadEnv {
    let runtime = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    cmd.env("XDG_RUNTIME_DIR", runtime.path());
    cmd.env("HOME", home.path());
    cmd.env("LLAMA_SERVER", "/nonexistent-orator/llama-server");
    // Pin the LLM endpoint at a refused port: the room may have a
    // live gateway on the default 8080, and this golden must fail
    // closed regardless of ambient listeners.
    cmd.env("LLM_BASE_URL", "http://127.0.0.1:1");
    cmd.env("LLM_MODEL", "nonexistent-test-model");
    cmd.current_dir(cwd.path());
    // A bare environment: no daemon socket, no config, no weights.
    cmd.env_remove("LLAMA_AUDIO_SERVER");
    DeadEnv {
        _runtime: runtime,
        _home: home,
        _cwd: cwd,
    }
}

fn run_cli(args: &[&str]) -> (std::process::Output, DeadEnv) {
    let mut cmd = std::process::Command::new(cli_binary());
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let guard = dead_env(&mut cmd);
    let output = cmd.output().expect("cli runs");
    (output, guard)
}

#[test]
fn daemon_down_is_never_success() {
    for args in [
        vec!["say", "--backend", "daemon", "hello"],
        // Config-default backend (follows the file, no flag).
        vec!["say", "hello"],
        vec!["ask", "hi"],
        vec!["ask", "--speak", "hi"],
        vec!["status"],
        vec!["stats"],
        vec!["daemon", "stop"],
        vec!["listen"],
        vec!["ptt", "start"],
        vec!["ptt", "stop"],
        vec!["meeting", "start"],
        vec!["meeting", "list"],
        vec!["meeting", "stop", "dead-id"],
        vec!["approve", "dead-id"],
        // New surface, same honesty: no TTY, no listener, no success.
        vec!["tui"],
        vec!["--transport", "ws", "say", "--backend", "daemon", "hi"],
        vec!["--transport", "ws", "status"],
        vec!["--transport", "ws", "daemon", "stop"],
    ] {
        let (output, _guard) = run_cli(&args);
        assert!(
            !output.status.success(),
            "{args:?} must fail without a daemon: {}",
            output.status
        );
        let code = output.status.code().expect("exit code");
        assert!(
            code == 1 || code == 2 || code == 3,
            "{args:?} exits Error, Usage, or DaemonUnavailable, got {code}"
        );
    }
}

#[test]
fn configured_values_are_honored() {
    let home = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        cwd.path().join("orator.json"),
        r#"{"tts": {"backend": "kokoro", "voice": "bf_emma"}}"#,
    )
    .expect("config writes");

    let mut get = std::process::Command::new(cli_binary());
    get.args(["config", "get", "tts.backend"]);
    get.env("HOME", home.path());
    get.current_dir(cwd.path());
    get.stdin(std::process::Stdio::null());
    let out = get.output().expect("cli runs");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "kokoro",
        "the file value wins over the default"
    );

    let mut set = std::process::Command::new(cli_binary());
    set.args(["config", "set", "stt.backend", "whisper"]);
    set.env("HOME", home.path());
    set.current_dir(cwd.path());
    set.stdin(std::process::Stdio::null());
    assert!(set.output().expect("cli runs").status.success());

    let mut get_stt = std::process::Command::new(cli_binary());
    get_stt.args(["config", "get", "stt.backend"]);
    get_stt.env("HOME", home.path());
    get_stt.current_dir(cwd.path());
    get_stt.stdin(std::process::Stdio::null());
    let out = get_stt.output().expect("cli runs");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "whisper",
        "set values persist to the hierarchical file"
    );

    let mut doctor = std::process::Command::new(cli_binary());
    doctor.arg("doctor");
    doctor.env("HOME", home.path());
    doctor.current_dir(cwd.path());
    doctor.stdin(std::process::Stdio::null());
    let out = doctor.output().expect("cli runs");
    assert!(out.status.success());
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(
        report.contains("config: orator.json"),
        "doctor names the loaded hierarchical file:\n{report}"
    );
}

/// New leaves round-trip through `config get/set` and reach their
/// components: GPU flags gate the backends, WS listen/token open the
/// listener, the admission bound gates turns.
#[test]
fn new_leaves_are_honored_end_to_end() {
    let home = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        cwd.path().join("orator.json"),
        r#"{"daemon": {"ws_listen": "", "ws_token": ""}}"#,
    )
    .expect("config writes");

    fn get(home: &std::path::Path, cwd: &std::path::Path, key: &str) -> String {
        let mut get = std::process::Command::new(cli_binary());
        get.args(["config", "get", key]);
        get.env("HOME", home);
        get.current_dir(cwd);
        get.stdin(std::process::Stdio::null());
        let out = get.output().expect("cli runs");
        assert!(out.status.success(), "{key} reads");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn set(home: &std::path::Path, cwd: &std::path::Path, key: &str, value: &str) {
        let mut set = std::process::Command::new(cli_binary());
        set.args(["config", "set", key, value]);
        set.env("HOME", home);
        set.current_dir(cwd);
        set.stdin(std::process::Stdio::null());
        assert!(
            set.output().expect("cli runs").status.success(),
            "{key}={value} writes"
        );
    }

    // GPU flags default off everywhere (CPU parity of record).
    assert_eq!(get(home.path(), cwd.path(), "stt.gpu"), "false");
    assert_eq!(get(home.path(), cwd.path(), "llm.gpu"), "false");
    assert_eq!(get(home.path(), cwd.path(), "tts.gpu"), "false");
    set(home.path(), cwd.path(), "tts.gpu", "true");
    assert_eq!(get(home.path(), cwd.path(), "tts.gpu"), "true");
    // The WS listener is socket-only by default.
    assert_eq!(get(home.path(), cwd.path(), "daemon.ws_listen"), "");
    set(
        home.path(),
        cwd.path(),
        "daemon.ws_listen",
        "127.0.0.1:18081",
    );
    assert_eq!(
        get(home.path(), cwd.path(), "daemon.ws_listen"),
        "127.0.0.1:18081"
    );
    set(home.path(), cwd.path(), "daemon.ws_token", "secret");
    assert_eq!(get(home.path(), cwd.path(), "daemon.ws_token"), "secret");
    // The admission bound ships calibrated and stays in band.
    assert_eq!(get(home.path(), cwd.path(), "daemon.admission_max"), "2");
    set(home.path(), cwd.path(), "daemon.admission_max", "4");
    assert_eq!(get(home.path(), cwd.path(), "daemon.admission_max"), "4");
}

/// Bound golden: over-bound inputs across playback/prompt/history
/// surfaces yield counters and markers, never unbounded growth — one
/// composed pass over the same primitives the unit goldens pin
/// individually, all hermetic (scripted queue, in-memory store, pure
/// dictate math; no audio device, no models).
#[test]
fn over_bound_inputs_stay_bounded_everywhere() {
    common_core::runtime::block_on_shared(async {
        // Prompt/prefix: a 100 k-char turn rolls under the ceiling
        // with a loud marker; the newest text is what the endpoint sees.
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let queue = scripted_queue(Arc::clone(&recorded), "ok");
        let mut llm = orator_llm::HttpSession::with_queue(
            queue,
            orator_llm::SessionConfig {
                endpoint_base: "http://127.0.0.1:9".to_string(),
                model: "bound-test".to_string(),
                ctx_tokens: 8192,
                n_batch: 512,
                system_prompt: "You are a terse assistant.".to_string(),
            },
        );
        llm.push_text(&"w ".repeat(50_000));
        let out = llm
            .step(&mut Sampler::default())
            .expect("scripted step succeeds");
        assert_eq!(out.delta, "ok");
        let requests = common_core::sync::lock(&recorded);
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].contains("truncated"),
            "over-cap prefix carries its marker into the request"
        );
        assert!(
            requests[0].trim_end().ends_with('w'),
            "newest text protected at the tail"
        );
        drop(requests);

        // Event store: replay pages at REPLAY_PAGE however many rows exist.
        let log = orator_store::EventLog::in_memory().expect("memory log");
        let over = orator_store::REPLAY_PAGE + 10;
        for index in 0..over {
            log.append("s1", 0, "note", &format!("row {index}"), index as u64)
                .expect("append");
        }
        let page = log.replay(0, over + 1000).expect("replay");
        assert_eq!(
            page.len(),
            orator_store::REPLAY_PAGE,
            "replay never exceeds one page"
        );
        assert_eq!(
            log.count_kind("note", 0).expect("count"),
            over as u64,
            "counters see everything pages do not"
        );

        // Dictate math: absurd durations refuse or clamp, never allocate.
        assert!(orator_cli::dictate::validate_seconds(0).is_err());
        assert!(orator_cli::dictate::validate_seconds(121).is_err());
        assert_eq!(
            orator_cli::dictate::reserve_for(u64::MAX, 48_000),
            orator_cli::dictate::MAX_DICTATE_SAMPLES,
            "reserve clamps at the sample ceiling"
        );
    });
}

fn mic_frame(samples: Vec<f32>) -> orator_core::Frame {
    use std::num::NonZeroU32;
    let rate = NonZeroU32::new(16_000).expect("rate");
    orator_core::Frame {
        samples: std::sync::Arc::new(samples),
        channel: Channel::Mic,
        sample_rate: 16_000,
        stamp: MediaTime::zero(rate),
    }
}

fn hot_mic() -> orator_core::Frame {
    mic_frame(speech_frame())
}

fn voiced_duplex_body(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\",\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n\
         data: [DONE]\n",
        one_f32()
    )
}

/// Live-conversation golden: VAD opens on speech, ASR transcribes, the
/// LLM reply contains the transcript, TTS voices, user speech
/// mid-reply barges (abort cut, epoch retired and re-armed), the
/// resumed turn completes, and notes/diarization/store land in order.
#[test]
fn live_conversation_with_barge_in_completes_in_order() {
    common_core::runtime::block_on_shared(async {
        // ASR stub: one scripted transcript.
        let asr = httpmock::MockServer::start();
        asr.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(
                "data: {\"choices\":[{\"delta\":{\"content\":\"book a table for two\"}}]}\n\
                 data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n",
            );
        });
        let mut listener = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
            endpoint: asr.base_url(),
            ..orator_s2s::S2sConfig::default()
        });
        // VAD opens on the voiced span (attack run, then speech).
        let mut vad = orator_vad::EnergyVad::with_floor(0.02);
        for _ in 0..2 {
            assert!(!vad.push(&speech_frame()).speech, "attack runs first");
        }
        assert!(vad.push(&speech_frame()).speech, "hot span opens");
        let transcript = listener
            .transcribe_blocking(&speech_span(8))
            .expect("asr turn runs");
        assert_eq!(transcript, "book a table for two");

        // Supervised LLM stub echoes the transcript into its reply.
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_task = Arc::clone(&recorded);
        let echo = Arc::new(LlmRequestQueue::new(
            fluent_concurrency::tokio_runtime(),
            &LlmQueueConfig {
                worker_count: 2,
                queue_capacity: 64,
            },
            move |task: LlmTask| {
                let recorded = Arc::clone(&recorded_task);
                async move {
                    let text = task
                        .messages
                        .iter()
                        .map(|message: &ChatMessage| message.content.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    common_core::sync::lock(&recorded).push(text.clone());
                    Ok::<String, FluentError>(format!("noted: {text}"))
                }
            },
        ));
        let mut llm = orator_llm::HttpSession::with_queue(
            echo,
            orator_llm::SessionConfig {
                endpoint_base: "http://127.0.0.1:9".to_string(),
                model: "conversation-test".to_string(),
                ctx_tokens: 8192,
                n_batch: 512,
                system_prompt: "You are a terse assistant.".to_string(),
            },
        );
        llm.push_text(&transcript);
        let out = llm
            .step(&mut Sampler::default())
            .expect("scripted step succeeds");
        assert!(
            out.delta.contains(&transcript),
            "the reply carries the transcript: {}",
            out.delta
        );

        // Duplex voice stub: content plus voiced chunks.
        let voice = httpmock::MockServer::start();
        let mock = voice.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).body(voiced_duplex_body("first reply"));
        });
        let mut session = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
            endpoint: voice.base_url(),
            turn_window_ms: 100,
            ..orator_s2s::S2sConfig::default()
        });
        // First turn voices: seven hot frames latch and fill the cap.
        let mut first = None;
        for _ in 0..7 {
            if let Ok(output) = session.step(&hot_mic()) {
                if !output.audio.samples.is_empty() {
                    first = Some(output);
                    break;
                }
            }
        }
        let first = first.expect("first turn voices");
        assert!(first.end_of_turn, "voice turn commits");
        assert!(session.is_voicing(), "reply marks voicing");

        // User speech mid-reply: three hot frames buffer, then the cut.
        for _ in 0..3 {
            let _ = session.step(&hot_mic());
        }
        session.abort_handle().cancel();
        session.set_epoch(Epoch(2));
        assert!(
            session.epoch_drops() >= 1,
            "the retired epoch drops its buffer"
        );
        let cut = session.step(&hot_mic()).expect_err("cut turn aborts");
        assert_eq!(
            cut,
            orator_core::DuplexError::Aborted,
            "barge-in cuts exactly the in-flight turn"
        );
        // Re-armed: the resumed turn completes on fresh hot frames.
        let mut resumed = None;
        for _ in 0..7 {
            if let Ok(output) = session.step(&hot_mic()) {
                if !output.audio.samples.is_empty() {
                    resumed = Some(output);
                    break;
                }
            }
        }
        let resumed = resumed.expect("resumed turn voices");
        assert!(resumed.end_of_turn);
        assert!(!resumed.audio.samples.is_empty());
        mock.assert_hits(2);
        let live_handle = session.live_text_handle();
        let live = common_core::sync::lock(&live_handle);
        assert!(
            live.contains("first reply"),
            "live text tracks the voiced turn"
        );
        drop(live);

        // Notes roll up, diarization attributes, the store sequences —
        // transcript, reply, barge, resumed reply, note, in order.
        let summary = orator_notes::roll_summary("", &transcript);
        assert!(summary.contains("book a table for two"), "{summary}");
        let segments = orator_diar::segment(Channel::Mic, 3200);
        assert!(!segments.is_empty(), "speech spans segments");
        let log = orator_store::EventLog::in_memory().expect("memory log");
        log.append("s1", 0, "transcript", &transcript, 1)
            .expect("append transcript");
        log.append("s1", 0, "reply", &out.delta, 2)
            .expect("append reply");
        log.append("s1", 0, "barge", "user cut the reply", 3)
            .expect("append barge");
        log.append("s1", 0, "reply", "first reply", 4)
            .expect("append resumed reply");
        log.append("s1", 0, "note", &summary, 5)
            .expect("append note");
        let events = log.replay(0, 10).expect("replay");
        let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
        assert_eq!(kinds, vec!["transcript", "reply", "barge", "reply", "note"]);
        assert!(events[0].payload.contains("book a table for two"));
    });
}
