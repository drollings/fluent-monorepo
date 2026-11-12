//! Adaptive drop-oldest jitter buffer.
//!
//! Playout and inference both briefly outrun or lag each other. The buffer
//! absorbs the variance: on overflow it drops the *oldest* frames down to
//! the `initial + partial` target and grows the ceiling; on underrun it
//! grows the floor. Every drop is counted — the counter is the audible
//! glitch budget, not a confidence signal.

use std::collections::VecDeque;

use orator::Frame;

/// Jitter buffer over whole frames.
#[derive(Debug)]
/// Jitter buffer over whole frames.
pub struct JitterBuffer {
    /// Queued frames, oldest first.
    frames: VecDeque<Frame>,
    /// Always-kept prefix, in frames.
    initial: usize,
    /// Adaptive floor, in frames.
    partial: usize,
    /// Adaptive ceiling, in frames.
    max: usize,
    /// Growth step, in frames.
    step: usize,
    /// Ceiling cap, in frames.
    cap: usize,
    /// Frames dropped on overflow, cumulative.
    pub drops: u64,
    /// Underruns observed, cumulative.
    pub underruns: u64,
    /// Shallowest queue depth seen, in samples.
    pub min_depth_samples: usize,
    /// Deepest queue depth seen, in samples.
    pub max_depth_samples: usize,
}

impl JitterBuffer {
    /// Moshi-adapted defaults for 20 ms frames: keep 1 frame, floor 1,
    /// ceiling 1 growing by 1 to a 4-frame (80 ms) cap.
    pub fn for_20ms_frames() -> Self {
        Self {
            frames: VecDeque::new(),
            initial: 1,
            partial: 1,
            max: 1,
            step: 1,
            cap: 4,
            drops: 0,
            underruns: 0,
            // Min-seed: any pushed frame lowers it via `min` below.
            // Tracked for calibration; unreported while empty.
            min_depth_samples: usize::MAX,
            max_depth_samples: 0,
        }
    }

    /// Push one frame. On overflow, drop oldest down to the
    /// `initial + partial` target, count them, and grow the ceiling.
    /// Never blocks; never grows without bound.
    pub fn push(&mut self, frame: Frame) {
        let ceiling = self.initial + self.partial + self.max;
        if self.frames.len() + 1 > ceiling {
            let target = self.initial + self.partial;
            while self.frames.len() > target {
                self.frames.pop_front();
                self.drops += 1;
            }
            self.max = (self.max + self.step).min(self.cap);
        }
        self.frames.push_back(frame);
        self.observe_depth();
    }

    /// Pop the oldest frame. On underrun, grow the floor and count it.
    pub fn pop(&mut self) -> Option<Frame> {
        let frame = self.frames.pop_front();
        if frame.is_none() {
            self.underruns += 1;
            self.partial = (self.partial + self.step).min(self.cap);
        } else {
            self.observe_depth();
        }
        frame
    }

    /// Queued frame count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the buffer holds no frames.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn depth_samples(&self) -> usize {
        self.frames.iter().map(Frame::len).sum()
    }

    fn observe_depth(&mut self) {
        let depth = self.depth_samples();
        self.min_depth_samples = self.min_depth_samples.min(depth);
        self.max_depth_samples = self.max_depth_samples.max(depth);
    }
}

#[cfg(test)]
#[path = "../tests/jitter.rs"]
mod tests;