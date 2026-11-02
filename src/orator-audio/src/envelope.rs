//! Playout envelopes: duck on barge-in detect, fade on cut.
//!
//! Both are linear sample math, no allocation beyond the output: duck
//! scales by a time-varying gain, fade zeroes the flushed tail. Numbers
//! follow the duplex targets (about −12 dB over 100 ms on detect, a
//! 20 ms fade on cut).

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

/// Playout gain `elapsed_ms` after detect: 1.0 ramping linearly to the
/// duck floor over [`DUCK_RAMP_MS`], then holding.
#[must_use]
pub fn duck_gain_at(elapsed_ms: u64) -> f32 {
    let floor = db_to_linear(DUCK_DEPTH_DB);
    if elapsed_ms >= DUCK_RAMP_MS {
        return floor;
    }
    let t = elapsed_ms as f32 / DUCK_RAMP_MS as f32;
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

#[cfg(test)]
#[path = "../tests/envelope.rs"]
mod tests;
