//! Playout envelopes: duck on barge-in detect, fade on cut.
//!
//! Both are linear sample math, no allocation beyond the output: duck
//! scales by a time-varying gain, fade zeroes the flushed tail. Numbers
//! follow the duplex targets (about −12 dB over 100 ms on detect, a
//! 20 ms fade on cut).

/// Cut fade length in ms.
/// Canonical home: `common_core::envelope` (re-exported for API compat).
pub use common_core::envelope::CUT_FADE_MS;
/// Duck depth in dB on barge-in detect.
/// Canonical home: `common_core::envelope` (re-exported for API compat).
pub use common_core::envelope::DUCK_DEPTH_DB;
/// Duck ramp length in ms.
/// Canonical home: `common_core::envelope` (re-exported for API compat).
pub use common_core::envelope::DUCK_RAMP_MS;

/// dB gain to linear amplitude.
/// Shared math home: `common_core::envelope`.
#[must_use]
pub fn db_to_linear(db: f32) -> f32 {
    common_core::envelope::db_to_linear(db)
}

/// Playout gain `elapsed_ms` after detect: 1.0 ramping linearly to the
/// duck floor over [`DUCK_RAMP_MS`], then holding.
/// Shared math home: `common_core::envelope::linear_ramp_gain`.
#[must_use]
pub fn duck_gain_at(elapsed_ms: u64) -> f32 {
    let floor = db_to_linear(DUCK_DEPTH_DB);
    common_core::envelope::linear_ramp_gain(elapsed_ms, DUCK_RAMP_MS, floor)
}

/// Fade the last `fade_samples` of `samples` linearly to zero (the cut
/// flush). A fade longer than the buffer fades the whole buffer; an
/// empty fade is a no-op.
/// Shared math home: `common_core::envelope`.
pub fn apply_fade_out(samples: &mut [f32], fade_samples: usize) {
    common_core::envelope::apply_fade_out(samples, fade_samples);
}

#[cfg(test)]
#[path = "../tests/envelope.rs"]
mod tests;
