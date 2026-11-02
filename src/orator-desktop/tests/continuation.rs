//! Continuation tests: the 10 s same-window rule.

use std::time::{Duration, Instant};

use crate::continuation::{continuation_prefix, CONTINUATION_WINDOW};

#[test]
fn back_to_back_joins_with_space() {
    let first = Instant::now();
    let second = first + Duration::from_secs(3);
    assert_eq!(continuation_prefix(Some(first), true, second), " ");
}

#[test]
fn lapsed_window_starts_fresh() {
    let first = Instant::now();
    let second = first + CONTINUATION_WINDOW + Duration::from_secs(1);
    assert_eq!(continuation_prefix(Some(first), true, second), "");
}

#[test]
fn changed_window_starts_fresh() {
    let first = Instant::now();
    assert_eq!(
        continuation_prefix(Some(first), false, first + Duration::from_secs(1)),
        ""
    );
}

#[test]
fn no_previous_paste_starts_fresh() {
    assert_eq!(continuation_prefix(None, true, Instant::now()), "");
}
