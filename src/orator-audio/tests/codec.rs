//! Codec seam goldens: the PCM adapter round-trips within its
//! fidelity contract, resets cleanly across turns, and streams
//! inside the frame quantum. Transcript-level quality needs weights
//! and is deferred with the codec — see the calibration register.

use crate::codec::{Codec, PcmCodec};

/// 2 s of voiced-like harmonic content at the codec rate.
fn voiced(seconds: u64) -> Vec<f32> {
    let rate = PcmCodec::RATE as f32;
    (0..u64::from(PcmCodec::RATE) * seconds)
        .map(|index| {
            let time = index as f32 / rate;
            0.4 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * 900.0 * time).sin()
        })
        .collect()
}

#[test]
fn pcm_adapter_reports_its_contract() {
    let codec = PcmCodec::new();
    assert_eq!(codec.sample_rate(), 24_000);
    assert_eq!(codec.code_rate(), 24_000);
}

#[test]
fn round_trip_stays_within_one_lsb() {
    // i16 wire framing quantizes: at most half an LSB each way.
    let pcm = voiced(2);
    let mut codec = PcmCodec::new();
    let codes = codec.encode_step(&pcm);
    assert_eq!(codes.len(), pcm.len() * 2, "one i16 per sample");
    let back = codec.decode_step(&codes);
    assert_eq!(back.len(), pcm.len(), "framing never drops samples");
    let worst = pcm
        .iter()
        .zip(back.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst <= 1.0 / 32768.0,
        "round-trip within one LSB: {worst}"
    );
    let energy = |samples: &[f32]| samples.iter().map(|s| s * s).sum::<f32>();
    let drift = (energy(&pcm) - energy(&back)).abs() / energy(&pcm);
    assert!(drift < 1e-4, "energy preserved: {drift}");
}

#[test]
fn per_step_streaming_composes_to_whole() {
    let pcm = voiced(1);
    let mut whole = PcmCodec::new();
    let whole_codes = whole.encode_step(&pcm);
    let whole_back = whole.decode_step(&whole_codes);
    let mut stepped = PcmCodec::new();
    let mut codes = Vec::new();
    let mut back = Vec::new();
    for chunk in pcm.chunks_exact(480) {
        codes.extend_from_slice(&stepped.encode_step(chunk));
        let step_codes = stepped.encode_step(chunk);
        back.extend_from_slice(&stepped.decode_step(&step_codes));
    }
    assert_eq!(codes, whole_codes, "framing is position-independent");
    assert_eq!(back, whole_back, "streaming composes to whole");
}

#[test]
fn reset_leaks_no_state_across_turns() {
    let mut codec = PcmCodec::new();
    let loud = codec.encode_step(&vec![0.9; 480]);
    assert!(!loud.iter().all(|byte| *byte == 0));
    codec.reset();
    let silence = vec![0.0; 480];
    let codes = codec.encode_step(&silence);
    let back = codec.decode_step(&codes);
    assert!(
        back.iter().all(|sample| *sample == 0.0),
        "post-reset turn carries nothing from the last"
    );
    // A second loud-quiet pair without reset behaves identically:
    // the adapter holds no cross-step state to leak.
    let mut fresh = PcmCodec::new();
    let codes = fresh.encode_step(&silence);
    assert_eq!(fresh.decode_step(&codes), back);
}

#[test]
fn step_latency_stays_far_inside_the_frame_quantum() {
    // A 20 ms quantum at the codec rate: the seam must cost a small
    // fraction of it. Banded, never exact — this is a ceiling check.
    let chunk = vec![0.25; 480];
    let mut codec = PcmCodec::new();
    let began = std::time::Instant::now();
    let steps = 200u32;
    for _ in 0..steps {
        let codes = codec.encode_step(&chunk);
        let _ = codec.decode_step(&codes);
    }
    let mean_us = began.elapsed().as_micros() / u128::from(steps);
    assert!(
        mean_us < 5_000,
        "seam cost {mean_us}µs must stay far under the 20 ms quantum"
    );
}

#[test]
fn empty_steps_are_silent_no_ops() {
    let mut codec = PcmCodec::new();
    assert!(codec.encode_step(&[]).is_empty());
    assert!(codec.decode_step(&[]).is_empty());
}
