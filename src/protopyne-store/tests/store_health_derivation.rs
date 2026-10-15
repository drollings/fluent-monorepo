//! `StoreHealth` derivation pins: every probe outcome maps to exactly one
//! reading, and the mapping never confuses "no rows" with "unreachable".
//!
//! The suite composes the real vocabulary — `Coverage::from_bars` for the
//! probe answer, `classify(…).repair_spans().len()` for the gap count — so
//! the derivation is tested as the repair worker will call it, not against
//! hand-built counts.

use super::*;
use crate::commit::test_tapes::at_ms;
use crate::coverage::{Coverage, FreshnessPolicy, classify};

/// Minute zero of the derivation clock, in epoch milliseconds.
const T0: i64 = 1_767_225_900_000;

/// A contiguous one-minute tape of `count` bars starting at `T0`.
fn tape(count: i64) -> Vec<protopyne_types::Bar> {
    (0..count).map(|n| at_ms(T0 + n * 60_000)).collect()
}

fn policy() -> FreshnessPolicy {
    FreshnessPolicy::new().build()
}

#[test]
fn probe_error_reads_unreachable_and_discards_the_gap_count() {
    let probe: Result<Option<Coverage>, StoreError> = Err(StoreError::Busy("locked".to_owned()));
    let reading = StoreHealth::of(&probe, 3, T0 + 600_000);
    assert!(!reading.reachable);
    assert_eq!(reading.watermark_age_secs, None);
    // A failed probe knows no gap count: the stale `3` must not survive.
    assert_eq!(reading.open_gaps, 0);
}

#[test]
fn empty_tape_is_reachable_with_no_age() {
    let probe: Result<Option<Coverage>, StoreError> = Ok(None);
    let reading = StoreHealth::of(&probe, 0, T0 + 600_000);
    assert!(reading.reachable);
    assert_eq!(reading.watermark_age_secs, None);
    assert_eq!(reading.open_gaps, 0);
}

#[test]
fn covered_tape_reports_the_watermark_age_and_the_classified_gaps() {
    let bars = tape(3);
    let coverage = Coverage::from_bars(&bars, protopyne_types::Timeframe::M1)
        .expect("three contiguous bars cover");
    let now_ms = T0 + 3 * 60_000 + 90_000;
    let gaps = classify(
        Some(coverage.clone()),
        now_ms,
        protopyne_types::Timeframe::M1,
        policy(),
    )
    .repair_spans()
    .len() as u64;
    let reading = StoreHealth::of(&Ok(Some(coverage)), gaps, now_ms);
    assert!(reading.reachable);
    // Last bar at `T0 + 2min`, now 150s later: a 150-second age.
    assert_eq!(reading.watermark_age_secs, Some(150));
    assert_eq!(reading.open_gaps, gaps);
}

#[test]
fn watermark_ahead_of_now_saturates_to_zero_instead_of_wrapping() {
    let bars = tape(2);
    let coverage = Coverage::from_bars(&bars, protopyne_types::Timeframe::M1)
        .expect("two contiguous bars cover");
    let probe: Result<Option<Coverage>, StoreError> = Ok(Some(coverage));
    let reading = StoreHealth::of(&probe, 0, T0);
    assert!(reading.reachable);
    assert_eq!(reading.watermark_age_secs, Some(0));
}

#[test]
fn default_is_no_reading_and_therefore_no_objection() {
    assert_eq!(
        StoreHealth::default(),
        StoreHealth {
            reachable: true,
            watermark_age_secs: None,
            open_gaps: 0,
        }
    );
}
