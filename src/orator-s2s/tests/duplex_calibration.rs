//! Duplex threshold calibration: scripted-frame groups for the VAD,
//! endpointing, and barge-in thresholds, scored through the shared
//! calibration harness.
//!
//! Every group is hermetic: deterministic tones, fixed-seed noise, and
//! silence — never wall-clock audio, never models. Each family scores
//! firing as a predictor of ground truth through
//! `common_core::calibration::calibrate_threshold` and must pass the
//! shared gate (precision >= 0.90, FPR <= 0.05). The endpointing and
//! barge-in groups drive the production paths (`EnergyVad` hangover
//! close, `AudioSession` channel gating and voicing) rather than a
//! model of them; the event-level hold rule stays covered where it
//! lives, in `orator-turn`.

use common_core::calibration::calibrate_threshold;

use crate::config::S2sConfig;
use crate::session::AudioSession;
use orator_core::DuplexEngine;

const FRAME_SAMPLES: usize = 320;
const SAMPLE_RATE: u32 = 16_000;
/// One frame at 16 kHz carries 20 ms of audio.
const FRAME_MS: u64 = 20;

fn mic_frame(samples: Vec<f32>, channel: orator_core::Channel) -> orator_core::Frame {
    use std::num::NonZeroU32;
    let rate = NonZeroU32::new(SAMPLE_RATE).expect("rate");
    orator_core::Frame {
        samples: std::sync::Arc::new(samples),
        channel,
        sample_rate: SAMPLE_RATE,
        stamp: orator_core::MediaTime::zero(rate),
    }
}

fn silence(frames: usize) -> Vec<Vec<f32>> {
    vec![vec![0.0; FRAME_SAMPLES]; frames]
}

/// Voiced-like harmonic content: loud, low zero-crossing.
fn harmonic(frames: usize) -> Vec<Vec<f32>> {
    (0..frames)
        .map(|_| {
            (0..FRAME_SAMPLES)
                .map(|index| {
                    let time = index as f32 / SAMPLE_RATE as f32;
                    0.35 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
                        + 0.35 * (2.0 * std::f32::consts::PI * 300.0 * time).sin()
                })
                .collect()
        })
        .collect()
}

/// Pure tone at `freq` Hz with peak `amplitude`.
fn tone(freq: f32, amplitude: f32, frames: usize) -> Vec<Vec<f32>> {
    (0..frames)
        .map(|_| {
            (0..FRAME_SAMPLES)
                .map(|index| {
                    amplitude
                        * (2.0 * std::f32::consts::PI * freq * index as f32 / SAMPLE_RATE as f32)
                            .sin()
                })
                .collect()
        })
        .collect()
}

/// Deterministic uniform noise in `[-amplitude, amplitude]` from a
/// fixed seed: hermetic across runs, no random crate needed.
fn seeded_noise(seed: u64, amplitude: f32, frames: usize) -> Vec<Vec<f32>> {
    let mut state = seed;
    (0..frames)
        .map(|_| {
            (0..FRAME_SAMPLES)
                .map(|_| {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    // Upper 31 bits over their full range: uniform in [0, 1].
                    let unit = (state >> 33) as f32 / i32::MAX as f32;
                    (unit * 2.0 - 1.0) * amplitude
                })
                .collect()
        })
        .collect()
}

/// Worst-case high zero-crossing noise: alternating sign every sample.
fn alternating_noise(amplitude: f32, frames: usize) -> Vec<Vec<f32>> {
    (0..frames)
        .map(|_| {
            (0..FRAME_SAMPLES)
                .map(|index| {
                    if index % 2 == 0 {
                        amplitude
                    } else {
                        -amplitude
                    }
                })
                .collect()
        })
        .collect()
}

struct SpanCase {
    name: &'static str,
    frames: Vec<Vec<f32>>,
    speech: bool,
}

/// Whether any frame in the span fires on a fresh detector.
fn span_fires(frames: &[Vec<f32>]) -> bool {
    let mut vad = orator_vad::EnergyVad::default();
    frames.iter().any(|frame| vad.push(frame).speech)
}

#[test]
fn vad_spans_fire_only_on_speech() {
    let cases = vec![
        SpanCase {
            name: "silence-10",
            frames: silence(10),
            speech: false,
        },
        SpanCase {
            name: "silence-50",
            frames: silence(50),
            speech: false,
        },
        SpanCase {
            name: "quiet-440hz",
            frames: tone(440.0, 0.005, 30),
            speech: false,
        },
        SpanCase {
            name: "tone-4k",
            frames: tone(4000.0, 0.4, 20),
            speech: false,
        },
        SpanCase {
            name: "tone-5k",
            frames: tone(5000.0, 0.4, 20),
            speech: false,
        },
        SpanCase {
            name: "tone-6k",
            frames: tone(6000.0, 0.4, 20),
            speech: false,
        },
        SpanCase {
            name: "tone-7k",
            frames: tone(7000.0, 0.4, 20),
            speech: false,
        },
        SpanCase {
            name: "alternating-noise",
            frames: alternating_noise(0.52, 20),
            speech: false,
        },
        SpanCase {
            name: "white-noise",
            frames: seeded_noise(0x243F_6A88_85A3_08D3, 0.52, 20),
            speech: false,
        },
        SpanCase {
            name: "harmonic-5",
            frames: harmonic(5),
            speech: true,
        },
        SpanCase {
            name: "harmonic-10",
            frames: harmonic(10),
            speech: true,
        },
        SpanCase {
            name: "harmonic-20",
            frames: harmonic(20),
            speech: true,
        },
    ];
    for case in &cases {
        assert_eq!(
            span_fires(&case.frames),
            case.speech,
            "span `{}` decided wrong",
            case.name
        );
    }
    let report = calibrate_threshold(
        &cases,
        |case: &SpanCase| f64::from(span_fires(&case.frames)),
        |case: &SpanCase| case.speech,
        0.5,
    );
    assert_eq!((report.tp, report.fp, report.tn, report.r#fn), (3, 0, 9, 0));
    assert!(
        report.passes_gate(),
        "VAD spans must pass the shared gate: {report:?}"
    );
}

#[test]
fn silence_only_spans_yield_zero_turns() {
    for length in [1, 10, 50, 200] {
        assert!(
            !span_fires(&silence(length)),
            "{length} silent frames must never fire"
        );
    }
}

#[test]
fn tone_sweeps_yield_zero_speech_verdicts() {
    // Loud high-frequency tones: above the floor, rejected by the
    // zero-crossing ceiling. A loud low tone is genuinely vowel-like
    // to an energy detector and is not part of this control.
    for freq in [4000.0, 4500.0, 5000.0, 5500.0, 6000.0, 6500.0, 7000.0] {
        assert!(
            !span_fires(&tone(freq, 0.4, 10)),
            "loud {freq} Hz tone must not read as speech"
        );
    }
}

#[test]
fn voiced_span_opens_and_closes_inside_its_bands() {
    let mut sequence = silence(10);
    sequence.extend(harmonic(20));
    sequence.extend(silence(30));
    let mut vad = orator_vad::EnergyVad::default();
    let mut onset = None;
    let mut last_open = None;
    for (index, frame) in sequence.iter().enumerate() {
        if vad.push(frame).speech {
            if onset.is_none() {
                onset = Some(index);
            }
            last_open = Some(index);
        }
    }
    // No fire across leading silence; onset exactly after the attack
    // run (3 hot frames); close exactly after the hangover run
    // (15 cold frames) past the last hot frame (index 29).
    assert_eq!(onset, Some(12), "onset must sit one attack past span start");
    assert_eq!(
        last_open,
        Some(43),
        "close must sit one hangover past span end"
    );
}

struct GapCase {
    name: &'static str,
    gap_frames: usize,
    tail_frames: usize,
    must_close: bool,
}

/// Whether the turn closes across the gap: 10 hot frames latch the
/// detector, then `gap_frames` silent frames elapse.
fn gap_closes(gap_frames: usize, tail_frames: usize) -> bool {
    let mut vad = orator_vad::EnergyVad::default();
    for frame in harmonic(10) {
        vad.push(&frame);
    }
    assert!(vad.speaking(), "lead-in must latch");
    for frame in silence(gap_frames) {
        vad.push(&frame);
    }
    let closed = !vad.speaking();
    for frame in harmonic(tail_frames) {
        vad.push(&frame);
    }
    closed
}

#[test]
fn endpointing_holds_through_pauses_and_cuts_past_the_band() {
    let cases = vec![
        GapCase {
            name: "mid-word pause 100ms",
            gap_frames: 5,
            tail_frames: 10,
            must_close: false,
        },
        GapCase {
            name: "slow-speaker gap 200ms",
            gap_frames: 10,
            tail_frames: 10,
            must_close: false,
        },
        GapCase {
            name: "hangover boundary minus one",
            gap_frames: 14,
            tail_frames: 10,
            must_close: false,
        },
        GapCase {
            name: "hangover boundary",
            gap_frames: 15,
            tail_frames: 0,
            must_close: true,
        },
        GapCase {
            name: "end of utterance 800ms",
            gap_frames: 40,
            tail_frames: 0,
            must_close: true,
        },
    ];
    for case in &cases {
        assert_eq!(
            gap_closes(case.gap_frames, case.tail_frames),
            case.must_close,
            "gap `{}` decided wrong",
            case.name
        );
    }
    let report = calibrate_threshold(
        &cases,
        |case: &GapCase| f64::from(gap_closes(case.gap_frames, case.tail_frames)),
        |case: &GapCase| case.must_close,
        0.5,
    );
    assert_eq!((report.tp, report.fp, report.tn, report.r#fn), (2, 0, 3, 0));
    assert!(
        report.passes_gate(),
        "endpointing gaps must pass the shared gate: {report:?}"
    );
}

fn one_f32() -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(2.5f32.to_le_bytes())
}

fn voiced_body() -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"reply\",\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n\
         data: [DONE]\n",
        one_f32()
    )
}

fn voiced_session(server: &httpmock::MockServer) -> (AudioSession, httpmock::Mock<'_>) {
    let mock = server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).body(voiced_body());
    });
    let session = AudioSession::new(S2sConfig {
        endpoint: server.base_url(),
        turn_window_ms: 100,
        ..S2sConfig::default()
    });
    (session, mock)
}

fn hot_mic() -> orator_core::Frame {
    mic_frame(
        harmonic(1).pop().expect("one harmonic frame"),
        orator_core::Channel::Mic,
    )
}

fn loud_reply() -> orator_core::Frame {
    mic_frame(vec![0.4; FRAME_SAMPLES], orator_core::Channel::Reply)
}

/// Voice one setup turn so the session marks itself voicing, then run
/// `stimulus` frames and report whether a second turn dialed.
fn second_turn_dials(stimulus: &[orator_core::Frame]) -> bool {
    let server = httpmock::MockServer::start();
    crate::ensure::invalidate_hint_cache(&server.base_url());
    let (mut session, mock) = voiced_session(&server);
    let mut voiced = false;
    for _ in 0..10 {
        if let Ok(out) = session.step(&hot_mic()) {
            if !out.audio.samples.is_empty() {
                voiced = true;
                break;
            }
        }
    }
    assert!(voiced, "setup turn must voice before the stimulus");
    assert!(session.is_voicing());
    mock.assert_hits(1);
    for frame in stimulus {
        let _ = session.step(frame);
    }
    // Any second dial means the stimulus opened a turn; sustained
    // speech may voice twice (second and third turns), so the gate is
    // strictly more than the setup hit, not exactly one more.
    mock.hits() > 1
}

#[test]
fn barge_in_opens_on_speech_never_on_echo_or_bursts() {
    struct Case {
        name: &'static str,
        stimulus: Vec<orator_core::Frame>,
        must_open: bool,
    }
    let echo_only: Vec<orator_core::Frame> = (0..20).map(|_| loud_reply()).collect();
    let noise_burst: Vec<orator_core::Frame> = seeded_noise(0xB529_7A4D_16FF_0C39, 0.52, 3)
        .into_iter()
        .map(|samples| mic_frame(samples, orator_core::Channel::Mic))
        .collect();
    let blip: Vec<orator_core::Frame> = (0..2).map(|_| hot_mic()).collect();
    let speech: Vec<orator_core::Frame> = (0..10).map(|_| hot_mic()).collect();
    let mut speech_after_echo = echo_only.clone();
    speech_after_echo.extend((0..10).map(|_| hot_mic()));
    let cases = vec![
        Case {
            name: "echo-only",
            stimulus: echo_only,
            must_open: false,
        },
        Case {
            name: "noise-burst",
            stimulus: noise_burst,
            must_open: false,
        },
        Case {
            name: "short-blip",
            stimulus: blip,
            must_open: false,
        },
        Case {
            name: "sustained-speech",
            stimulus: speech,
            must_open: true,
        },
        Case {
            name: "speech-after-echo",
            stimulus: speech_after_echo,
            must_open: true,
        },
    ];
    let outcomes: Vec<bool> = cases
        .iter()
        .map(|case| second_turn_dials(&case.stimulus))
        .collect();
    for (case, opened) in cases.iter().zip(outcomes.iter()) {
        assert_eq!(
            *opened, case.must_open,
            "barge case `{}` decided wrong",
            case.name
        );
    }
    let report = calibrate_threshold(
        &cases,
        |case: &Case| {
            let index = cases
                .iter()
                .position(|other| other.name == case.name)
                .expect("case");
            f64::from(outcomes[index])
        },
        |case: &Case| case.must_open,
        0.5,
    );
    assert_eq!((report.tp, report.fp, report.tn, report.r#fn), (2, 0, 3, 0));
    assert!(
        report.passes_gate(),
        "barge-in groups must pass the shared gate: {report:?}"
    );
}

#[test]
fn echo_only_frames_open_zero_candidates() {
    let echo: Vec<orator_core::Frame> = (0..20).map(|_| loud_reply()).collect();
    assert!(
        !second_turn_dials(&echo),
        "echo alone must never open a turn"
    );
}

#[test]
fn calibrated_constants_stay_pinned() {
    let vad = orator_vad::EnergyVad::default();
    assert!(
        (vad.floor - 0.02).abs() < f32::EPSILON,
        "floor moves only with numbers recorded"
    );
    assert!(
        (vad.hysteresis - 0.01).abs() < f32::EPSILON,
        "hysteresis bands stay put"
    );
    assert_eq!(vad.attack_frames, 3, "attack run stays three frames");
    assert_eq!(vad.hangover_frames, 15, "hangover run stays fifteen frames");
    assert!(
        (vad.max_zero_crossing - 0.45).abs() < f32::EPSILON,
        "ceiling stays put"
    );
    let turn = orator_core::TurnConfig::default();
    assert_eq!(turn.hold_min_ms, 300, "hold floor stays 300 ms");
    assert_eq!(turn.hold_max_ms, 700, "hold ceiling stays 700 ms");
    assert_eq!(turn.barge_in_ms, 150, "barge-in sustain stays 150 ms");
    assert_eq!(
        S2sConfig::default().turn_window_ms,
        2000,
        "window cap stays 2 s"
    );
    assert_eq!(
        FRAME_MS * 15,
        300,
        "hangover run matches the hold floor in ms"
    );
}
