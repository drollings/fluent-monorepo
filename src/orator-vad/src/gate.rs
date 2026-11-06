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

use crate::energy::EnergyVad;

/// 16 kHz frame quantum the gate scores (20 ms).
const FRAME_SAMPLES: usize = 320;
/// Below this overall RMS the input is digital silence: no detector runs.
const SILENCE_RMS: f32 = 0.002;
/// Gate detector floor: far-mic speech around RMS 0.01 must still open
/// spans while the zero-crossing ceiling rejects broadband hiss.
const GATE_FLOOR: f32 = 0.01;
/// Minimum speech-frame fraction over full frames.
const MIN_SPEECH_FRACTION: f32 = 0.15;
/// Verdict re-arm cadence in frames: a latched span re-proves every
/// second, so stationary hiss that wins one attack lottery cannot ride
/// it for the whole utterance while voiced spans re-latch at once.
const REARM_FRAMES: usize = 50;
/// Minimum zero-crossing intervals for a tonal verdict: shorter spans
/// defer to the energy gate (which already rejects sub-attack blips).
const MIN_TONE_INTERVALS: usize = 8;
/// Interval coefficient of variation at or below this reads as a
/// stationary pure tone. Pure sines measure 0.008–0.046 at any
/// frequency or amplitude; voiced proxies measure ≥0.33; noise ≥0.69.
const TONE_MAX_CV: f32 = 0.1;

/// Whether 16 kHz mono `samples` carry committable speech: the energy
/// fraction holds and no stationary pure tone owns the span.
#[must_use]
pub fn speech_present_16k(samples: &[f32]) -> bool {
    if !energy_gate(samples) {
        return false;
    }
    !stationary_tone_present(samples)
}

/// Whether `samples` are a stationary pure tone: enough zero-crossing
/// intervals with near-zero variation. Amplitude-invariant (a ratio,
/// never a level) and limiter-invariant (limiting keeps crossing
/// times); silence and sparse clicks defer (too few intervals).
#[must_use]
pub fn stationary_tone_present(samples: &[f32]) -> bool {
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
    if count < MIN_TONE_INTERVALS as u64 {
        return false;
    }
    let variance = m2 / count as f64;
    (variance.sqrt() / mean) as f32 <= TONE_MAX_CV
}

/// Calibrated energy fraction: overall RMS above digital silence plus
/// a minimum share of detector speech frames over full 20 ms frames.
fn energy_gate(samples: &[f32]) -> bool {
    if samples.is_empty() {
        return false;
    }
    if common_core::pcm::root_mean_square_f32(samples) < SILENCE_RMS {
        return false;
    }
    let mut vad = EnergyVad::with_floor(GATE_FLOOR);
    let mut speech_frames = 0usize;
    let mut total_frames = 0usize;
    for frame in samples.chunks(FRAME_SAMPLES) {
        if frame.len() < FRAME_SAMPLES {
            break;
        }
        if total_frames % REARM_FRAMES == 0 {
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
    speech_frames as f32 / total_frames as f32 >= MIN_SPEECH_FRACTION
}

#[cfg(test)]
#[path = "../tests/gate.rs"]
mod tests;
