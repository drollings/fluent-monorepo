//! Scripted bench scenarios: single-turn latency, sustained duplex
//! throughput, N-session contention, memory ceiling on a long session.
//!
//! Stub servers plus synthetic audio, hermetic — no models, no wall
//! clock for correctness. Every scenario asserts completion plus shape
//! with banded timing only; the rendered histograms are the record
//! budgets feed on. Bands are generous ceilings, never exact numbers,
//! and a regression here opens an investigation, never a red gate.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fluent_concurrency::stream::StreamAbort;
use orator_audio::Codec as _;
use orator::TtsStream as _;
use orator_voice::s2s::AudioSession;

use crate::harness::{BenchKind, BenchLog};

/// Synthetic mic audio: 1 s of voiced-like harmonic content at 16 kHz.
fn synthetic_utterance() -> Vec<f32> {
    (0..16_000)
        .map(|index| {
            let time = index as f32 / 16_000.0;
            0.3 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * 300.0 * time).sin()
        })
        .collect()
}

fn one_f32() -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(2.5f32.to_le_bytes())
}

/// Stub audio endpoint: scripted transcript, reply, and voiced chunk,
/// with an optional per-turn delay.
fn stub_audio_server(delay_ms: u64) -> httpmock::MockServer {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200)
            .body(format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"hi there\",\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n\
                 data: [DONE]\n",
                one_f32()
            ))
            .delay(Duration::from_millis(delay_ms));
    });
    server
}

fn session_at(base: &str) -> AudioSession {
    AudioSession::new(orator_voice::s2s::S2sConfig {
        endpoint: base.to_string(),
        ..orator_voice::s2s::S2sConfig::default()
    })
}

/// Scripted synth seam: first chunk pays a scripted cost, the rest are
/// instant — first-audio timing without weights.
struct ScriptSeam {
    first_cost: Duration,
    calls: std::sync::Mutex<u32>,
}

impl ScriptSeam {
    fn first_audio_cost(&self) -> Duration {
        self.first_cost
    }
}

impl orator_voice::tts::pipeline::SynthSeam for ScriptSeam {
    fn synth(
        &self,
        text: &str,
    ) -> Result<orator_voice::tts::pipeline::SynthOut, orator::TtsError> {
        let mut calls = common_core::sync::lock(&self.calls);
        *calls += 1;
        if *calls == 1 {
            std::thread::sleep(self.first_cost);
        }
        let _ = text;
        Ok(orator_voice::tts::pipeline::SynthOut {
            samples: vec![0.5; 4800],
            rate: orator_voice::tts::KOKORO_SAMPLE_RATE,
        })
    }
}

fn render(log: &BenchLog, label: &str) {
    let stats = crate::harness::histograms(log);
    eprintln!("bench scenario {label}:\n{}", crate::harness::render_stats(&stats));
}

#[test]
fn single_turn_latency_completes_in_shape() {
    let server = stub_audio_server(100);
    let mut session = session_at(&server.base_url());
    let utterance = synthetic_utterance();
    let mut log = BenchLog::new();
    let began = Instant::now();
    log.push(BenchKind::InputPcm, "vad", 0);
    // VAD gate over the synthetic utterance: hot spans buffer.
    let mut vad = orator_voice::vad::EnergyVad::default();
    let mut hot = 0u32;
    for frame in utterance.chunks_exact(320) {
        if vad.push(frame).speech {
            hot += 1;
        }
    }
    assert!(hot > 0, "the synthetic utterance opens the gate");
    log.push(BenchKind::SendPcm, "vad", began.elapsed().as_millis() as u64);
    log.push(BenchKind::InputPcm, "stt_llm_audio", began.elapsed().as_millis() as u64);
    let abort = StreamAbort::new();
    let turn = session
        .chat_turn(None, Some(&utterance), &abort)
        .expect("stub turn completes");
    log.push(
        BenchKind::SendPcm,
        "stt_llm_audio",
        began.elapsed().as_millis() as u64,
    );
    assert!(!turn.text.is_empty(), "reply text flows");
    assert!(turn.voiced, "stub voices");
    assert!(!turn.audio.is_empty(), "audio flows");
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "single turn stays banded"
    );
    // First-audio leg through the scripted synth seam.
    let seam = Arc::new(ScriptSeam {
        first_cost: Duration::from_millis(50),
        calls: std::sync::Mutex::new(0),
    });
    let mut pipe = orator_voice::tts::TtsPipeline::new(
        Arc::clone(&seam) as Arc<dyn orator_voice::tts::pipeline::SynthSeam>,
        orator::Epoch(1),
    );
    let first_began = Instant::now();
    log.push(BenchKind::InputPcm, "tts_first_audio", first_began.elapsed().as_millis() as u64);
    pipe.push_text(
        "Hello world, this is the opening line. How are you doing today? Quite well, thank you.",
        orator::Epoch(1),
    );
    let chunk = orator::TtsStream::step(&mut pipe)
        .expect("scripted step holds")
        .expect("first chunk voices");
    let first_audio_ms = first_began.elapsed().as_millis() as u64;
    log.push(BenchKind::SendPcm, "tts_first_audio", first_audio_ms);
    assert!(!chunk.samples.is_empty(), "first audio voices");
    assert_eq!(seam.first_audio_cost(), Duration::from_millis(50));
    assert!(
        first_audio_ms < 5_000,
        "first audio stays banded: {first_audio_ms}ms"
    );
    render(&log, "single-turn");
}

#[test]
fn sustained_duplex_throughput_keeps_order() {
    let server = stub_audio_server(0);
    let mut session = session_at(&server.base_url());
    let turns = 10usize;
    let mut log = BenchLog::new();
    let began = Instant::now();
    let mut texts = Vec::new();
    for index in 0..turns {
        let text = format!("turn {index}");
        let turn_began = Instant::now();
        log.push(
            BenchKind::InputPcm,
            "turn",
            began.elapsed().as_millis() as u64,
        );
        let turn = session
            .chat_turn(Some(&text), None, &StreamAbort::new())
            .expect("stub turn completes");
        log.push(
            BenchKind::SendPcm,
            "turn",
            began.elapsed().as_millis() as u64,
        );
        assert!(turn.voiced, "every turn voices");
        texts.push((text, turn.text, turn_began.elapsed()));
    }
    let total = began.elapsed();
    assert!(
        total < Duration::from_secs(60),
        "sustained throughput stays banded: {total:?}"
    );
    assert_eq!(texts.len(), turns);
    eprintln!(
        "bench scenario throughput: {turns} turns in {total:?} ({} turns/min)",
        (turns as f64 / total.as_secs_f64() * 60.0) as u64
    );
    render(&log, "throughput");
}

#[test]
fn n_session_contention_measures_refusal_and_completion() {
    let server = stub_audio_server(300);
    let url = server.base_url();
    let owner = Arc::new(orator_daemon::voice::VoiceOwner::disabled());
    let mut log = BenchLog::new();
    let began = Instant::now();
    let completed = std::sync::atomic::AtomicU32::new(0);
    let refused_ms = std::sync::Mutex::new(Vec::new());
    let done_ms = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..3 {
            let (owner, url) = (Arc::clone(&owner), url.clone());
            let (completed, refused_ms, done_ms) = (&completed, &refused_ms, &done_ms);
            scope.spawn(move || {
                let turn_began = began.elapsed().as_millis() as u64;
                match owner.say_via(&url, "hello", None) {
                    Ok((samples, _)) => {
                        assert_eq!(samples.len(), 1);
                        completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        common_core::sync::lock(done_ms)
                            .push(began.elapsed().as_millis() as u64 - turn_began);
                    }
                    Err(orator_daemon::voice::VoiceError::Saturated(_)) => {
                        common_core::sync::lock(refused_ms)
                            .push(began.elapsed().as_millis() as u64 - turn_began);
                    }
                    Err(other) => panic!("only saturation refuses here: {other:?}"),
                }
            });
        }
    });
    assert_eq!(completed.load(std::sync::atomic::Ordering::SeqCst), 2);
    let refused = common_core::sync::lock(&refused_ms);
    assert_eq!(refused.len(), 1, "K+1 refuses");
    let done = common_core::sync::lock(&done_ms);
    for (index, ms) in done.iter().enumerate() {
        log.push(BenchKind::InputPcm, "contention_turn", 0);
        log.push(BenchKind::SendPcm, "contention_turn", *ms);
        let _ = index;
    }
    for ms in refused.iter() {
        assert!(
            *ms < 2_000,
            "refusal stays banded: {ms}ms"
        );
        log.push(BenchKind::InputPcm, "contention_refusal", 0);
        log.push(BenchKind::SendPcm, "contention_refusal", *ms);
    }
    render(&log, "contention");
    owner.shutdown();
}

/// Resident set size in bytes from `/proc/self/statm` (Linux-only,
/// like the rest of the capture path).
fn resident_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("statm readable on Linux");
    let resident_pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|field| field.parse().ok())
        .expect("resident field parses");
    resident_pages * 4096
}

#[test]
fn long_session_memory_ceiling_holds() {
    let before = resident_bytes();
    let mut vad = orator_voice::vad::EnergyVad::default();
    let hot: Vec<f32> = (0..320)
        .map(|index| {
            let time = index as f32 / 16_000.0;
            0.35 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
        })
        .collect();
    let mut codec = orator_audio::PcmCodec::new();
    let gate = orator_daemon::admission::AdmissionGate::new(8);
    let mut log = BenchLog::new();
    let began = Instant::now();
    log.push(BenchKind::InputPcm, "session_loop", 0);
    for _ in 0..200 {
        for frame in [&hot[..], &vec![0.0; 320][..]] {
            let _ = vad.push(frame);
        }
        let codes = codec.encode_step(&hot);
        let _ = codec.decode_step(&codes);
        if let Ok(_permit) = gate.admit() {}
        codec.reset();
    }
    log.push(
        BenchKind::SendPcm,
        "session_loop",
        began.elapsed().as_millis() as u64,
    );
    let growth = resident_bytes().saturating_sub(before);
    eprintln!("bench scenario memory: 200 scripted turns, RSS growth {growth} bytes");
    assert!(
        growth < 128 * 1024 * 1024,
        "long session stays banded: {growth} bytes growth"
    );
    assert!(
        began.elapsed() < Duration::from_secs(60),
        "loop stays banded"
    );
    render(&log, "memory");
}
