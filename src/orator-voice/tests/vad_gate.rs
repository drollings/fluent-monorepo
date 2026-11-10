//! Commit-gate tests: tonal isolation plus the scored corpus.
//!
//! Every case is hermetic (synth tones, fixed-seed noise, harmonic
//! voiced proxies — no model, no device). The corpus runs through the
//! shared `calibrate_threshold` gate (precision >= 0.90, FPR <= 0.05).

use common_core::calibration::calibrate_threshold;

use crate::vad::gate::{speech_present_16k, speech_present_16k_with_config, stationary_tone_present};

const RATE: usize = 16_000;

/// Pure tone at `freq` Hz with peak `amplitude`, `secs` seconds.
fn tone(freq: f32, amplitude: f32, secs: usize) -> Vec<f32> {
    (0..RATE * secs)
        .map(|index| {
            amplitude * (2.0 * std::f32::consts::PI * freq * index as f32 / RATE as f32).sin()
        })
        .collect()
}

/// Deterministic voiced proxy: harmonic stack with speech-like energy
/// and low zero-crossing rate. Must commit.
fn voiced(secs: usize, scale: f32) -> Vec<f32> {
    (0..RATE * secs)
        .map(|index| {
            let time = index as f32 / RATE as f32;
            scale
                * (0.35 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
                    + 0.35 * (2.0 * std::f32::consts::PI * 300.0 * time).sin())
        })
        .collect()
}

/// Deterministic uniform noise in `[-amplitude, amplitude]`.
fn noise(seed: u64, amplitude: f32, secs: usize) -> Vec<f32> {
    let mut state = seed;
    (0..RATE * secs)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let unit = (state >> 33) as f32 / i32::MAX as f32;
            (unit * 2.0 - 1.0) * amplitude
        })
        .collect()
}

/// Pure tones read tonal at any frequency or amplitude; voiced proxies
/// and noise never do; silence (no intervals) defers.
#[test]
fn tonal_verdict_isolates_stationary_tones() {
    for (freq, amplitude) in [(440.0, 0.3), (440.0, 0.005), (1000.0, 0.3), (100.0, 0.3)] {
        assert!(
            stationary_tone_present(&tone(freq, amplitude, 1)),
            "{freq} Hz at {amplitude} reads tonal"
        );
    }
    assert!(
        !stationary_tone_present(&voiced(1, 1.0)),
        "voiced proxy is not tonal"
    );
    assert!(
        !stationary_tone_present(&voiced(1, 0.1)),
        "quiet voiced proxy is not tonal"
    );
    assert!(
        !stationary_tone_present(&noise(0x243F_6A88_85A3_08D3, 0.1, 1)),
        "noise is not tonal"
    );
    assert!(
        !stationary_tone_present(&vec![0.0; RATE]),
        "silence defers (no intervals)"
    );
    assert!(
        !stationary_tone_present(&[]),
        "empty input never reads tonal"
    );
}

struct GateCase {
    name: &'static str,
    samples: Vec<f32>,
    speech: bool,
}

fn corpus() -> Vec<GateCase> {
    vec![
        GateCase {
            name: "silence",
            samples: vec![0.0; RATE],
            speech: false,
        },
        GateCase {
            name: "empty",
            samples: Vec::new(),
            speech: false,
        },
        // The verified finding, exact shape: 440 Hz sine, peak ~0.3, 1 s.
        GateCase {
            name: "tone-440-loud",
            samples: tone(440.0, 0.3, 1),
            speech: false,
        },
        GateCase {
            name: "tone-440-quiet",
            samples: tone(440.0, 0.005, 1),
            speech: false,
        },
        GateCase {
            name: "tone-440-mid",
            samples: tone(440.0, 0.05, 1),
            speech: false,
        },
        GateCase {
            name: "tone-1k-loud",
            samples: tone(1000.0, 0.3, 1),
            speech: false,
        },
        GateCase {
            name: "tone-1k-quiet",
            samples: tone(1000.0, 0.005, 1),
            speech: false,
        },
        GateCase {
            name: "tone-1k-mid",
            samples: tone(1000.0, 0.05, 1),
            speech: false,
        },
        GateCase {
            name: "tone-100-loud",
            samples: tone(100.0, 0.3, 1),
            speech: false,
        },
        GateCase {
            name: "tone-4k-loud",
            samples: tone(4000.0, 0.4, 1),
            speech: false,
        },
        GateCase {
            name: "noise-quiet",
            samples: noise(11, 0.01, 1),
            speech: false,
        },
        GateCase {
            name: "noise-mid",
            samples: noise(12, 0.03, 1),
            speech: false,
        },
        GateCase {
            name: "noise-loud",
            samples: noise(13, 0.1, 1),
            speech: false,
        },
        // Must-fire: voiced proxies at full scale and -20 dB.
        GateCase {
            name: "voiced",
            samples: voiced(1, 1.0),
            speech: true,
        },
        GateCase {
            name: "voiced-quiet",
            samples: voiced(1, 0.1),
            speech: true,
        },
        GateCase {
            name: "voiced-long",
            samples: voiced(3, 1.0),
            speech: true,
        },
    ]
}

/// The composed gate scores the full corpus: every non-speech case
/// commits nothing (the loud 440 Hz tone is the fixed finding), every
/// speech case commits (no silent drops, including -20 dB speech).
#[test]
fn composed_gate_passes_the_scored_corpus() {
    let cases = corpus();
    for case in &cases {
        assert_eq!(
            speech_present_16k(&case.samples),
            case.speech,
            "gate case `{}` decided wrong",
            case.name
        );
    }
    let report = calibrate_threshold(
        &cases,
        |case: &GateCase| f64::from(speech_present_16k(&case.samples)),
        |case: &GateCase| case.speech,
        0.5,
    );
    assert_eq!(
        (report.tp, report.fp, report.tn, report.r#fn),
        (3, 0, 13, 0),
        "exact corpus accounting"
    );
    assert!(
        report.passes_gate(),
        "commit gate must pass the shared gate: {report:?}"
    );
}

/// Tuned gates follow the file: a hair-trigger floor commits the
/// quiet-noise case the default sheds, so operators see the tradeoff.
#[test]
fn with_config_tuning_moves_the_gate() {
    let itchy = orator::VadConfig {
        gate_floor: 0.001,
        silence_rms: 0.0,
        min_speech_fraction: 0.0,
        ..orator::VadConfig::default()
    };
    let quiet = noise(11, 0.01, 1);
    assert!(!speech_present_16k(&quiet), "default sheds quiet noise");
    assert!(
        speech_present_16k_with_config(&quiet, &itchy),
        "hair-trigger floor commits it"
    );
}
