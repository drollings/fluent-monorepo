//! Speaker-turn segmentation: fixed windows with overlap.
//!
//! Windows are 1.5 s with 50% overlap over 16 kHz audio. The mic
//! channel shortcuts to the user label (no embedding needed); mixed
//! or system audio goes through embeddings. Short tails below half a
//! window merge into the previous segment instead of standing alone.

use orator::Channel;

/// Window length in samples at 16 kHz (1.5 s).
pub const WINDOW_SAMPLES: u64 = 24_000;

/// Window hop in samples (50% overlap).
pub const HOP_SAMPLES: u64 = 12_000;

/// One audio segment to attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One audio segment to attribute.
pub struct Segment {
    /// Segment index in emission order.
    pub index: usize,
    /// First sample (channel clock).
    pub start_sample: u64,
    /// One past the last sample.
    pub end_sample: u64,
    /// Which input this segment came from.
    pub channel: Channel,
}

impl Segment {
    /// Midpoint sample, for overlap alignment.
    #[must_use]
    pub const fn midpoint(&self) -> u64 {
        self.start_sample + (self.end_sample - self.start_sample) / 2
    }

    /// Whether this segment shortcuts to the user label (mic channel).
    #[must_use]
    pub const fn is_user_channel(&self) -> bool {
        matches!(self.channel, Channel::Mic)
    }
}

/// Cut `total_samples` of `channel` into overlapping windows.
#[must_use]
/// Cut `total_samples` of `channel` into overlapping windows.
pub fn segment(channel: Channel, total_samples: u64) -> Vec<Segment> {
    let mut segments = Vec::new();
    if total_samples == 0 {
        return segments;
    }
    let mut start = 0u64;
    let mut index = 0usize;
    while start < total_samples {
        let end = (start + WINDOW_SAMPLES).min(total_samples);
        // A tail below half a window joins the previous segment.
        if total_samples - start < WINDOW_SAMPLES / 2 && !segments.is_empty() {
            segments.last_mut().expect("non-empty").end_sample = total_samples;
            break;
        }
        segments.push(Segment {
            index,
            start_sample: start,
            end_sample: end,
            channel,
        });
        index += 1;
        if end == total_samples {
            break;
        }
        start += HOP_SAMPLES;
    }
    segments
}

#[cfg(test)]
#[path = "../../tests/diary_segment.rs"]
mod tests;