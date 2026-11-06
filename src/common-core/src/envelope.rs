//! Playout envelope math: zero-domain gain ramps and fades.
//!
//! Pure sample arithmetic with no audio device or session state. The
//! calibrated constants below are the shared playout targets (about
//! −12 dB of duck over 100 ms on barge-in detect, a 20 ms fade on
//! cut): `orator-audio` and `orator-turn` previously declared the same
//! numbers independently, which is the duplication this module ends.
//! Values stay calibrated — retuning is a calibration decision, never
//! a refactor side effect (see `orator_audio_envelope_vectors`).

/// Duck depth in dB on barge-in detect.
pub const DUCK_DEPTH_DB: f32 = -12.0;
/// Duck ramp length in ms.
pub const DUCK_RAMP_MS: u64 = 100;
/// Cut fade length in ms.
pub const CUT_FADE_MS: u64 = 20;

/// dB gain to linear amplitude.
#[must_use]
pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Playout gain `elapsed_ms` after detect: `1.0` ramping linearly to
/// `floor` over `ramp_ms`, then holding.
#[must_use]
pub fn linear_ramp_gain(elapsed_ms: u64, ramp_ms: u64, floor: f32) -> f32 {
    if elapsed_ms >= ramp_ms {
        return floor;
    }
    let t = elapsed_ms as f32 / ramp_ms as f32;
    1.0 + t * (floor - 1.0)
}

/// Fade the last `fade_samples` of `samples` linearly to zero (the cut
/// flush). A fade longer than the buffer fades the whole buffer; an
/// empty fade is a no-op.
pub fn apply_fade_out(samples: &mut [f32], fade_samples: usize) {
    let fade = fade_samples.min(samples.len());
    if fade == 0 {
        return;
    }
    let start = samples.len() - fade;
    for (index, sample) in samples[start..].iter_mut().enumerate() {
        let t = 1.0 - (index as f32 + 1.0) / fade as f32;
        *sample *= t;
    }
}
