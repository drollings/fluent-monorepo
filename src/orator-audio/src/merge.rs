//! Time-correct mic/sys word merge for live transcripts.
//!
//! Both STT lanes push into one bounded queue keyed by capture-clock
//! time; the transcript pops oldest-first. The push path waits on a
//! full queue (`push_wait` blocks the slower lane) — words are never
//! dropped here, only paced. Priorities are negative milliseconds so
//! the highest priority is the earliest word; meetings past ~596 h
//! saturate the key instead of wrapping (documented, never silent).

use std::sync::Arc;

use fluent_concurrency::queue::BoundedPriorityQueue;
use orator::{Channel, MediaTime};

/// One transcript word with its channel, timing, confidence, and an
/// optional speaker label (filled by diarization later).
#[derive(Debug, Clone, PartialEq)]
pub struct WordRecord {
    /// Surface text.
    pub text: String,
    /// Which input produced the word.
    pub channel: Channel,
    /// Word start on the capture clock.
    pub start: MediaTime,
    /// Word end on the capture clock.
    pub end: MediaTime,
    /// Backend confidence in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Speaker label when known.
    pub speaker: Option<String>,
}

impl WordRecord {
    /// Start time in whole milliseconds on the capture clock.
    #[must_use]
    pub fn start_ms(&self) -> u64 {
        self.start.samples * 1000 / u64::from(self.start.rate.get())
    }
}

/// Priority key for `word`: negative milliseconds, saturated at the
/// `i32` floor so ancient meetings pin instead of wrapping.
fn priority_of(word: &WordRecord) -> i32 {
    let ms = word.start_ms().min(i32::MAX as u64) as i32;
    ms.saturating_neg()
}

/// The merged transcript queue. One per meeting session.
pub struct MergeQueue {
    queue: BoundedPriorityQueue<WordRecord>,
}

impl MergeQueue {
    /// Queue holding at most `capacity` words (backpressure, not loss).
    #[must_use]
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            queue: BoundedPriorityQueue::new(capacity.max(1)),
        })
    }

    /// Push one word, waiting when full. Never drops.
    pub async fn push_wait(
        &self,
        word: WordRecord,
    ) -> Result<(), fluent_concurrency::pool::PoolError> {
        let priority = priority_of(&word);
        self.queue.push_wait(word, priority).await
    }

    /// Pop the earliest word. `None` once closed and drained.
    pub async fn pop(&self) -> Option<WordRecord> {
        self.queue.pop().await
    }

    /// Close the queue: waiters wake, remaining words still pop.
    pub fn close(&self) {
        self.queue.close();
    }

    /// Queued words right now.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether no words are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
#[path = "../tests/merge.rs"]
mod tests;
