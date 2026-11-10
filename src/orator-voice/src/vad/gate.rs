//! Commit gate for transcription: whether 16 kHz mono samples carry
//! committable speech before anything pays for inference.
//!
//! Two independent verdicts compose by AND. The energy fraction (gate
//! floor plus a minimum share of detector speech frames) rejects
//! silence, hiss, and noise. The tonal verdict (zero-crossing
//! interval regularity) rejects stationary pure tones, which are
//! genuinely vowel-like to an energy detector — a loud low sine fires
//! every energy frame, so energy alone would commit it and the model
//! would voice a hallucination for it. Speech always carries
//! interval irregularity (harmonic stacks, pitch movement, onsets,
//! consonants); a pure tone carries none, at any amplitude.
//!
//! Single home: the daemon adapter and the file path call this exact
//! function, so live and offline gating never disagree.

use crate::vad::energy::EnergyVad;

/// 16 kHz frame quantum the gate scores (20 ms).
const FRAME_SAMPLES: usize = 320;

/// Whether 16 kHz mono `samples` carry committable speech: the energy
/// fraction holds and no stationary pure tone owns the span.
#[must_use]
pub fn speech_present_16k(samples: &[f32]) -> bool {
    speech_present_16k_with_config(samples, &orator::VadConfig::default())
}

/// Whether 16 kHz mono `samples` carry committable speech under the
/// configured VAD tunables. All gating paths compose this so live
/// and offline gating never disagree under operator tuning.
#[must_use]
pub fn speech_present_16k_with_config(samples: &[f32], config: &orator::VadConfig) -> bool {
    if !energy_gate_with_config(samples, config) {
        return false;
    }
    !stationary_tone_present_with_config(samples, config)
}

/// Whether `samples` are a stationary pure tone: enough zero-crossing
/// intervals with near-zero variation. Amplitude-invariant (a ratio,
/// never a level) and limiter-invariant (limiting keeps crossing
/// times); silence and sparse clicks defer (too few intervals).
#[must_use]
pub fn stationary_tone_present(samples: &[f32]) -> bool {
    stationary_tone_present_with_config(samples, &orator::VadConfig::default())
}

/// Tonal verdict under the configured intervals and ceiling.
#[must_use]
pub fn stationary_tone_present_with_config(
    samples: &[f32],
    config: &orator::VadConfig,
) -> bool {
    if samples.len() < 2 {
        return false;
    }
    // Single-pass mean/variance (Welford): intervals never materialize.
    let mut count = 0u64;
    let mut mean = 0.0f64;
    let mut m2 = 0.0f64;
    let mut last_cross = 0usize;
    let mut have_cross = false;
    let mut prev_neg = samples[0] < 0.0;
    for (index, sample) in samples.iter().enumerate().skip(1) {
        let neg = *sample < 0.0;
        if neg != prev_neg {
            if have_cross {
                let gap = (index - last_cross) as f64;
                count += 1;
                let delta = gap - mean;
                mean += delta / count as f64;
                m2 += delta * (gap - mean);
            }
            last_cross = index;
            have_cross = true;
        }
        prev_neg = neg;
    }
    if count < u64::from(config.min_tone_intervals) {
        return false;
    }
    let variance = m2 / count as f64;
    (variance.sqrt() / mean) as f32 <= config.tone_max_cv
}

/// Calibrated energy fraction: overall RMS above digital silence plus
/// a minimum share of detector speech frames over full 20 ms frames.
fn energy_gate_with_config(samples: &[f32], config: &orator::VadConfig) -> bool {
    if samples.is_empty() {
        return false;
    }
    if common_core::pcm::root_mean_square_f32(samples) < config.silence_rms {
        return false;
    }
    let mut vad = EnergyVad::with_config(&orator::VadConfig {
        floor: config.gate_floor,
        ..config.clone()
    });
    let mut speech_frames = 0usize;
    let mut total_frames = 0usize;
    let rearm = config.rearm_frames.max(1) as usize;
    for frame in samples.chunks(FRAME_SAMPLES) {
        if frame.len() < FRAME_SAMPLES {
            break;
        }
        if total_frames % rearm == 0 {
            vad.reset();
        }
        total_frames += 1;
        if vad.push(frame).speech {
            speech_frames += 1;
        }
    }
    if total_frames == 0 {
        return false;
    }
    speech_frames as f32 / total_frames as f32 >= config.min_speech_fraction
}

#[cfg(test)]
#[path = "../../tests/vad_gate.rs"]
mod tests;
