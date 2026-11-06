//! Acoustic echo cancellation for open-speakerphone capture: the
//! assistant's own playout is subtracted from mic input before the VAD
//! gate ever sees it, so the speakers cannot become the next question.
//!
//! The filter wraps `aec-rs` (speexdsp echo state plus its preprocess
//! noise suppression) over 10 ms / 160-sample subframes at 16 kHz.
//! Playback threads feed known 16 kHz reference audio through a
//! non-blocking tap; the capture thread consumes it behind a bulk
//! delay that absorbs device out+in latency (the adaptive tail covers
//! the residual). Reference older than the delay window, or silence
//! on the speakers, passes mic audio through untouched — stale echo
//! is never subtracted twice.
//!
//! Threading: the filter lives on the capture thread (single owner,
//! no lock on the hot path). Playback threads hold only the tap
//! sender (a bounded drop-oldest channel, never blocking playout).

/// Bulk reference delay in milliseconds: device playout latency plus
/// capture latency on a typical PipeWire loop. Tunable once live
/// loopback measures it; the adaptive tail covers ±100 ms around it.
pub const AEC_REF_DELAY_MS: u64 = 80;

/// Reference audio cap in samples (3 s at 16 kHz): bounds memory
/// when playout runs long; older reference ages out of the window.
pub const AEC_REF_CAP_SAMPLES: usize = 3 * 16_000;

/// One reference chunk: 16 kHz mono samples of known playout.
pub type RefChunk = Vec<f32>;

/// Non-blocking playback reference tap. Playback threads push known
/// 16 kHz audio; a full tap drops the chunk instead of stalling
/// playout (a gap reads as silence downstream, never an error).
#[derive(Debug, Clone)]
pub struct AecRefTap {
    tx: std::sync::mpsc::SyncSender<RefChunk>,
}

impl AecRefTap {
    /// Feed known 16 kHz playout samples. Never blocks; drops on a
    /// full tap (playout always wins over cancellation).
    pub fn feed(&self, samples_16k: &[f32]) {
        let _ = self.tx.try_send(samples_16k.to_vec());
    }
}

/// Capture-side echo filter. Disabled filters pass audio through
/// untouched (zero behavior change); enabled filters subtract fresh
/// reference only.
pub struct AecFilter {
    /// speex state; `None` means passthrough (disabled or unavailable).
    inner: Option<aec_rs::Aec>,
    /// Reference tap receiver (capture thread drains it per poll).
    rx: std::sync::mpsc::Receiver<RefChunk>,
    /// Reference ring in 16 kHz samples (newest at the back).
    ring: std::collections::VecDeque<f32>,
    /// Bulk delay in samples (`AEC_REF_DELAY_MS` at 16 kHz).
    delay_samples: usize,
    /// Fresh (unconsumed-by-capture) reference samples. Stale
    /// reference never cancels: once exhausted, mic passes through.
    fresh: usize,
}

impl AecFilter {
    /// Disabled filter: `process` is a byte-identical passthrough and
    /// the tap still accepts (and drops) feeds. Default state, so
    /// capture without opt-in behaves exactly as before.
    #[must_use]
    pub fn disabled() -> (Self, AecRefTap) {
        let (tx, rx) = std::sync::mpsc::sync_channel::<RefChunk>(8);
        (
            Self {
                inner: None,
                rx,
                ring: std::collections::VecDeque::new(),
                delay_samples: 0,
                fresh: 0,
            },
            AecRefTap { tx },
        )
    }

    /// Enabled filter with `delay_ms` of bulk reference delay.
    #[must_use]
    pub fn new(delay_ms: u64) -> (Self, AecRefTap) {
        let (tx, rx) = std::sync::mpsc::sync_channel::<RefChunk>(8);
        (
            Self {
                inner: Some(aec_rs::Aec::new(&aec_rs::AecConfig::default())),
                rx,
                ring: std::collections::VecDeque::new(),
                delay_samples: (delay_ms as usize * 16_000) / 1_000,
                fresh: 0,
            },
            AecRefTap { tx },
        )
    }

    /// Whether echo cancellation applies (vs passthrough).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Drain fed reference into the ring, aging out anything older
    /// than the delay window plus one frame. Call once per capture
    /// poll, before `process`.
    pub fn pump(&mut self) {
        for chunk in self.rx.try_iter() {
            self.ring.extend(chunk.iter());
            self.fresh += chunk.len();
        }
        let keep = self.delay_samples + 320;
        let excess = self
            .ring
            .len()
            .saturating_sub(keep.max(AEC_REF_CAP_SAMPLES));
        if excess > 0 {
            self.ring.drain(..excess);
            self.fresh = self.fresh.saturating_sub(excess);
        }
    }

    /// Cancel known echo from one 16 kHz mic frame in place. Full
    /// 160-sample subframes run through speex; a trailing partial
    /// subframe passes through (never fabricated). Disabled filters,
    /// empty reference, or exhausted fresh reference pass through
    /// untouched.
    pub fn process(&mut self, frame: &mut [f32]) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if self.ring.len() < self.delay_samples + 160 || self.fresh < 160 {
            return;
        }
        let full = frame.len() / 160;
        for sub in 0..full {
            if self.fresh < 160 {
                break;
            }
            // Successive mic subframes pair with successive reference
            // subframes ending `delay` behind the newest sample, so
            // the windows walk forward in lockstep with the mic.
            let base = sub * 160;
            let end = self
                .ring
                .len()
                .saturating_sub(self.delay_samples + 160 * (full - sub));
            let start = end.saturating_sub(160);
            if end.saturating_sub(start) < 160 {
                break;
            }
            let rec: Vec<i16> = frame[base..base + 160]
                .iter()
                .map(|sample| {
                    common_core::pcm::f32_to_i16_sample(
                        *sample,
                        common_core::pcm::Scale16::Symmetric32767,
                        common_core::pcm::Rounding::TowardZero,
                    )
                })
                .collect();
            let echo: Vec<i16> = self
                .ring
                .range(start..end)
                .map(|sample| {
                    common_core::pcm::f32_to_i16_sample(
                        *sample,
                        common_core::pcm::Scale16::Symmetric32767,
                        common_core::pcm::Rounding::TowardZero,
                    )
                })
                .collect();
            let mut out = [0i16; 160];
            inner.cancel_echo(&rec, &echo, &mut out);
            for (slot, cleaned) in frame[base..base + 160].iter_mut().zip(out.iter()) {
                *slot = common_core::pcm::i16_to_f32_sample(
                    *cleaned,
                    common_core::pcm::Scale16::Symmetric32767,
                );
            }
            self.fresh -= 160;
        }
    }
}

#[cfg(test)]
#[path = "../tests/aec.rs"]
mod tests;
