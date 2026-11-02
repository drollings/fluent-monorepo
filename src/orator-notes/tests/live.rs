//! Live-notes scheduler tests: window rule and topic-shift fast path.

use crate::live::{NotesScheduler, NOTES_MAX_SECS, NOTES_MIN_SECS};

#[test]
fn window_bounds_hold() {
    assert_eq!(NOTES_MIN_SECS, 60);
    assert_eq!(NOTES_MAX_SECS, 90);
    let mut scheduler = NotesScheduler::new();
    assert!(!scheduler.due(false));
    scheduler.advance(60);
    assert!(scheduler.due(false));
    scheduler.reset();
    assert!(!scheduler.due(false));
}

#[test]
fn topic_shift_fires_immediately() {
    let scheduler = NotesScheduler::new();
    assert!(scheduler.due(true));
}
