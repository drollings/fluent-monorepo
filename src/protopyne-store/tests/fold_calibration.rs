//! The fold-calibration gate: frozen boundaries hold exactly, the sweep is
//! idempotent, the gap oracle agrees in both directions, the genuine hole
//! survives the fold, the cross-timeframe agreement matches the recorded
//! figure, and a mid-bucket start folds like the complete bucket.
//!
//! The goldens carry every recorded figure — 39 boundaries, the 346-bar head
//! run, the 23-of-116 agreement — so a later change that shifts any of them
//! fails here instead of silently re-freezing. Green is every tally row
//! accepted, mirroring the kernels' parity-dashboard gate.

use super::*;

use crate::fold::test_fold::{load_goldens, load_spy, seed_spy_slice, FoldTally};
use crate::fold::FoldPolicy;
use crate::plan::SpanPlan;
use crate::range::TimeRange;
use crate::store::{CandleStore, ReadRequest};
use crate::tape::test_setup::{with_registry_caps, VENUE};

/// Render the measured tally for assertion messages: one line per check, so
/// a drift names the check that moved.
fn render_tally(tally: &FoldTally) -> String {
    use std::fmt::Write as _;
    let mut table = String::from("check | recorded | measured\n");
    for row in &tally.rows {
        writeln!(table, "{} | {} | {}", row.name, row.expected, row.actual)
            .expect("writing to a String never fails");
    }
    table
}

#[test]
fn goldens_have_the_recorded_shape() {
    let goldens = load_goldens();
    assert_eq!(
        goldens.boundaries.len(),
        39,
        "39 frozen boundaries: a dropped boundary weakens the table"
    );
    assert_eq!(
        goldens.complete_run.bars, 346,
        "the head run is 346 bars: a shorter run folds fewer buckets"
    );
    assert_eq!(
        goldens.complete_run.total_bars, 436,
        "the file is 436 bars: a changed file is a changed corpus"
    );
    assert_eq!(
        goldens.agreement.vendor_buckets, 116,
        "116 vendor buckets: a dropped bucket inflates agreement"
    );
    assert_eq!(
        goldens.agreement.fields,
        ["open", "high", "low", "close", "volume"],
        "the comparison contract is prices plus volume, exactly"
    );
}

#[test]
fn every_check_accepts() {
    let goldens = load_goldens();
    let tally = FoldTally::measure(&goldens);
    for row in &tally.rows {
        assert!(
            row.accepted,
            "check '{}': recorded '{}' but measured '{}'",
            row.name, row.expected, row.actual
        );
    }
    assert!(
        tally.is_green(),
        "the tally must satisfy the green predicate:\n{}",
        render_tally(&tally)
    );
}

fn spy_request(
    timeframe: Timeframe,
    start_ms: i64,
    end_ms: i64,
    policy: FoldPolicy,
) -> ReadRequest {
    ReadRequest {
        symbol: "SPY".to_owned(),
        venue: VENUE.to_owned(),
        timeframe,
        range: TimeRange::bounded(start_ms, end_ms).expect("ordered fixture bounds"),
        limit: None,
        policy,
    }
}

#[tokio::test]
async fn mid_bucket_start_folds_like_the_complete_bucket() {
    with_registry_caps(async {
        let goldens = load_goldens();
        let minute = load_spy(&goldens.complete_run.fixture);
        let slice = &minute[..10];
        let (tape, _) = seed_spy_slice("SPY", slice).await;
        let bucket_ms = slice[0].ts.timestamp_millis();
        // The whole first bucket, read exactly: the reference fold.
        let whole = tape
            .read(&spy_request(
                Timeframe::M5,
                bucket_ms,
                bucket_ms + 300_000,
                FoldPolicy::ExtendToFullBuckets,
            ))
            .expect("the complete bucket reads");
        assert_eq!(whole.bars.len(), 1, "one bucket in, one bar out");
        // The same bucket entered mid-way: the fetch widens to the bucket floor,
        // folds, and narrows back to the window.
        let mid = tape
            .read(&spy_request(
                Timeframe::M5,
                bucket_ms + 120_000,
                bucket_ms + 600_000,
                FoldPolicy::ExtendToFullBuckets,
            ))
            .expect("the mid-bucket read serves");
        assert_eq!(
            mid.bars.len(),
            2,
            "both overlapping buckets survive the narrow-back"
        );
        assert_eq!(
            mid.bars[0], whole.bars[0],
            "the overlapping bar is identical to the complete-bucket fold"
        );
        assert_eq!(
            mid.served,
            TimeRange::bounded(bucket_ms + 120_000, bucket_ms + 600_000).expect("ordered bounds"),
            "served echoes the requested window, not the widened fetch"
        );
        assert!(
            mid.provenance.iter().all(SpanPlan::complete),
            "whole buckets report whole"
        );
        // The default policy is the extend policy: a regression to truncate by
        // default fails here, not just in the negative suite.
        let defaulted = tape
            .read(&spy_request(
                Timeframe::M5,
                bucket_ms + 120_000,
                bucket_ms + 600_000,
                FoldPolicy::default(),
            ))
            .expect("the default read serves");
        assert_eq!(
            defaulted.bars, mid.bars,
            "the default policy folds like the extend policy"
        );
    })
    .await;
}
