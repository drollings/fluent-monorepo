//! Continuation spacing: back-to-back dictations into one window read as
//! one sentence. A second paste within the window joins with a leading
//! space ("One." + "Two." → "One. Two."); the stored history keeps the
//! clean text either way.

use std::time::{Duration, Instant};

/// Join window: pastes into the same window within this long continue.
pub const CONTINUATION_WINDOW: Duration = Duration::from_secs(10);

/// Leading separator for a paste landing `now` after a previous paste at
/// `previous`, both into the same window (`same_window`). Empty when the
/// window lapsed, the window changed, or there was no previous paste.
#[must_use]
/// Leading separator for a paste landing `now` after a previous paste at
/// `previous`, both into the same window (`same_window`). Empty when the
/// window lapsed, the window changed, or there was no previous paste.
pub fn continuation_prefix(
    previous: Option<Instant>,
    same_window: bool,
    now: Instant,
) -> &'static str {
    match previous {
        Some(at) if same_window && now.duration_since(at) <= CONTINUATION_WINDOW => " ",
        _ => "",
    }
}

#[cfg(test)]
#[path = "../tests/continuation.rs"]
mod tests;