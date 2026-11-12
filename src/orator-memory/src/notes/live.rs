//! Live-notes scheduler stub: when the summarizer should run.
//!
//! The scheduler fires every 60–90 s or on a topic shift, whichever
//! comes first, at background priority. The summarizer behind the
//! call is a seam: this stub records the request so the scheduling
//! rule is pinned before the real map-reduce lands.

/// Earliest live-notes tick in seconds.
pub const NOTES_MIN_SECS: u64 = 60;

/// Latest live-notes tick in seconds.
pub const NOTES_MAX_SECS: u64 = 90;

/// Summarizer seam. The real map-reduce implements this later.
pub trait Summarize: Send + Sync {
    /// Summarize `transcript` for `meeting`.
    fn summarize(&self, meeting: &str, transcript: &str) -> String;
}

/// Scheduler state for one meeting.
#[derive(Debug, Clone, Default)]
/// Scheduler state for one meeting.
pub struct NotesScheduler {
    /// Seconds since the last notes update.
    pub elapsed_secs: u64,
}

impl NotesScheduler {
    /// Scheduler with a fresh clock.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the summarizer should run now: the window elapsed or
    /// the transcript changed topic.
    #[must_use]
    pub const fn due(&self, topic_shift: bool) -> bool {
        if topic_shift {
            return true;
        }
        self.elapsed_secs >= NOTES_MIN_SECS
    }

    /// Advance the clock by `secs`, saturating at the window ceiling.
    pub fn advance(&mut self, secs: u64) {
        self.elapsed_secs = self.elapsed_secs.saturating_add(secs).min(NOTES_MAX_SECS);
    }

    /// Reset the clock after a notes update.
    pub fn reset(&mut self) {
        self.elapsed_secs = 0;
    }
}

#[cfg(test)]
#[path = "../../tests/notes_live.rs"]
mod tests;