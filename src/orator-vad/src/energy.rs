//! Zero-dependency energy voice activity detector with hysteresis.
//!
//! Probability here is producer confidence in speech presence: it ranks
//! frames, it never decides turns. Endpointing consumes the binary speech
//! verdict plus its own timing; the probability must never set a hold.

use orator_core::Frame;

/// Per-frame detector output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadOutput {
    /// Speech verdict after hysteresis.
    pub speech: bool,
    /// Speech presence in `[0.0, 1.0]`, derived from energy above floor.
    pub probability: f32,
}

/// Energy + zero-crossing detector.
///
/// Speech starts when the RMS probability crosses the high mark for
/// `attack_frames` consecutive frames, and ends when it stays below the low
/// mark for `hangover_frames`. The zero-crossing ceiling rejects tonal and
/// narrowband noise that is loud but not speech-like.
#[derive(Debug, Clone)]
pub struct EnergyVad {
    /// RMS floor for speech, linear amplitude.
    pub floor: f32,
    /// Hysteresis band half-width around `floor`.
    pub hysteresis: f32,
    /// Consecutive hot frames to start speech.
    pub attack_frames: u32,
    /// Consecutive cold frames to end speech.
    pub hangover_frames: u32,
    /// Zero-crossing rate above which a hot frame is rejected as noise.
    pub max_zero_crossing: f32,
    hot_streak: u32,
    cold_streak: u32,
    speaking: bool,
}

impl Default for EnergyVad {
    fn default() -> Self {
        Self {
            floor: 0.02,
            hysteresis: 0.01,
            attack_frames: 3,
            hangover_frames: 15,
            max_zero_crossing: 0.45,
            hot_streak: 0,
            cold_streak: 0,
            speaking: false,
        }
    }
}

impl EnergyVad {
    /// Detector with an explicit floor; hysteresis and hangover stay default.
    #[must_use]
    pub fn with_floor(floor: f32) -> Self {
        Self {
            floor,
            ..Self::default()
        }
    }

    /// Whether the detector currently holds speech open.
    #[must_use]
    pub const fn speaking(&self) -> bool {
        self.speaking
    }

    /// Score one frame of mono f32 audio.
    pub fn push(&mut self, samples: &[f32]) -> VadOutput {
        let rms = root_mean_square(samples);
        let crossings = zero_crossing_rate(samples);
        let high = self.floor + self.hysteresis;
        let low = (self.floor - self.hysteresis).max(0.0);
        let probability = ((rms - low) / (high - low).max(f32::EPSILON)).clamp(0.0, 1.0);
        let hot = rms > high && crossings <= self.max_zero_crossing;
        let cold = rms < low;
        if hot {
            self.hot_streak += 1;
            self.cold_streak = 0;
        } else if cold {
            self.cold_streak += 1;
            self.hot_streak = 0;
        }
        // Inside the band neither streak moves: the verdict holds until
        // energy commits above or below the hysteresis margins.
        if !self.speaking && self.hot_streak >= self.attack_frames {
            self.speaking = true;
        } else if self.speaking && self.cold_streak >= self.hangover_frames {
            self.speaking = false;
        }
        VadOutput {
            speech: self.speaking,
            probability,
        }
    }

    /// Score a full frame by channel-agnostic samples.
    pub fn push_frame(&mut self, frame: &Frame) -> VadOutput {
        self.push(&frame.samples)
    }

    /// Drop held state. Thresholds survive.
    pub fn reset(&mut self) {
        self.hot_streak = 0;
        self.cold_streak = 0;
        self.speaking = false;
    }
}

/// Root mean square amplitude of one frame.
fn root_mean_square(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f32 = samples.iter().map(|sample| sample * sample).sum();
    (energy / samples.len() as f32).sqrt()
}

/// Fraction of adjacent sample pairs with opposite signs.
fn zero_crossing_rate(samples: &[f32]) -> f32 {
    if samples.len() < 2 {
        return 0.0;
    }
    let crossings = samples
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    crossings as f32 / (samples.len() - 1) as f32
}

#[cfg(test)]
#[path = "../tests/energy.rs"]
mod tests;
