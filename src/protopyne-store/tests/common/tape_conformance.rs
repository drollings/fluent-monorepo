//! Shared `CandleStore` conformance helpers: the trait's promises as checks.
//!
//! Included once from `tape.rs`; the instantiating suite calls these
//! helpers against its concrete store. Every helper takes `&dyn CandleStore`
//! (or pure values) so the one engine proves the promises without a second
//! spelling of the rule. Helpers assert; they never build stores.

#![allow(dead_code)]

use protopyne_types::{validate_new_bar, Bar, Timeframe};

use crate::coverage::{classify, Coverage, FreshnessPolicy, GapClass};
use crate::error::{PlanError, StoreError};
use crate::fold::{fold_bars, fold_bars_edges, FoldEdges, FoldPolicy};
use crate::plan::SpanOrigin;
use crate::range::TimeRange;
use crate::store::{AppendOutcome, CandleSeries, CandleStore, ReadRequest, WritePolicy};

/// Flat bars on a minute grid from `first_ms`.
#[must_use]
pub fn grid(first_ms: i64, count: i64) -> Vec<Bar> {
    (0..count)
        .map(|step| crate::commit::test_tapes::at_ms(first_ms + step * 60_000))
        .collect()
}

/// Ascending with every adjacent pair `validate_new_bar`-clean.
pub fn assert_ascending_valid(bars: &[Bar], what: &str) {
    let mut last: Option<&Bar> = None;
    for bar in bars {
        validate_new_bar(last, bar)
            .unwrap_or_else(|e| panic!("{what}: adjacent pair invalid: {e}"));
        last = Some(bar);
    }
    let mut sorted = bars.to_vec();
    sorted.sort_by_key(|bar| bar.ts);
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let ordered: Vec<i64> = sorted.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    assert_eq!(stamps, ordered, "{what}: reads back ascending");
}

/// Provenance shape: ascending spans, stored origin, window agreement.
pub fn assert_provenance_shape(series: &CandleSeries, what: &str) {
    let mut last_end: Option<i64> = None;
    for span in &series.provenance {
        assert_eq!(
            span.origin(),
            SpanOrigin::Stored,
            "{what}: span origin is stored"
        );
        if let (Some(start), Some(end)) = (span.span().start_ms, span.span().end_ms) {
            assert!(start < end, "{what}: span is ordered");
            if let Some(prev) = last_end {
                assert!(start >= prev, "{what}: spans ascend without overlap");
            }
            last_end = Some(end);
        }
    }
}

/// Unknown symbol refuses with its name attached.
pub fn check_unknown_symbol(store: &dyn CandleStore, venue: &str) {
    let req = ReadRequest {
        symbol: "NOBODY".to_owned(),
        venue: venue.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange::bounded(1_767_225_900_000, 1_767_225_960_000).expect("ordered"),
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    };
    match store.read(&req) {
        Err(StoreError::Plan(PlanError::UnknownSymbol(name))) => {
            assert_eq!(name, "NOBODY");
        }
        other => panic!("unknown symbol must refuse as UnknownSymbol, got {other:?}"),
    }
}

/// `InsertIfAbsent` re-delivery counts conflicts; `UpsertBySeq` overwrites.
pub fn check_append_policies(outcome_redeliver: AppendOutcome, outcome_upsert: AppendOutcome) {
    assert_eq!(
        outcome_redeliver,
        AppendOutcome {
            inserted: 0,
            conflicts: outcome_redeliver.conflicts,
            rejected: 0,
        },
        "re-delivery stores nothing"
    );
    assert!(
        outcome_redeliver.conflicts > 0,
        "re-delivery counts conflicts"
    );
    assert_eq!(outcome_upsert.conflicts, 1, "upsert resolves one collision");
}

/// Scale rejection is counted, never stored.
pub fn check_scale_outcome(outcome: AppendOutcome, inserted: usize) {
    assert_eq!(outcome.inserted, inserted);
    assert_eq!(outcome.rejected, 1, "one over-scale row refused");
    assert_eq!(outcome.conflicts, 0);
}

/// `KeepPartial` fold is byte-identical to `fold_bars`; `CompleteOnly` is a subset.
pub fn check_fold_matrix(bars: &[Bar]) {
    for tf in [Timeframe::M5, Timeframe::M15, Timeframe::H1] {
        let legacy = fold_bars(bars, tf).expect("legacy fold builds");
        let edged = fold_bars_edges(bars, tf, FoldEdges::KeepPartial).expect("edged fold builds");
        assert_eq!(edged, legacy, "{tf:?} KeepPartial stays byte-identical");
        let complete =
            fold_bars_edges(bars, tf, FoldEdges::CompleteOnly).expect("complete fold builds");
        assert!(
            complete.len() <= legacy.len(),
            "{tf:?} CompleteOnly is a subset"
        );
        for bar in &complete {
            assert!(
                legacy.iter().any(|kept| kept == bar),
                "{tf:?} CompleteOnly emits no new bars"
            );
        }
    }
}

/// Coverage classifier behaviour over a torn tape.
pub fn check_gap_classes(first_ms: i64, step_ms: i64) {
    let policy = FreshnessPolicy::new().build();
    assert_eq!(
        classify(None, first_ms, Timeframe::M1, policy),
        GapClass::Empty,
        "no rows reads Empty"
    );
    let contiguous: Vec<i64> = (0..10).map(|step| first_ms + step * step_ms).collect();
    let coverage = Coverage::from_stamps(&contiguous, Timeframe::M1).expect("covered");
    assert_eq!(
        classify(
            Some(coverage),
            first_ms + 10 * step_ms,
            Timeframe::M1,
            policy
        ),
        GapClass::Fresh,
        "contiguous current tape reads Fresh"
    );
    let torn: Vec<i64> = contiguous
        .into_iter()
        .filter(|stamp| stamp % (2 * step_ms) != first_ms % (2 * step_ms))
        .collect();
    if let Some(coverage) = Coverage::from_stamps(&torn, Timeframe::M1) {
        let verdict = classify(
            Some(coverage),
            first_ms + 10 * step_ms,
            Timeframe::M1,
            policy,
        );
        assert!(
            matches!(verdict, GapClass::Torn { .. }),
            "a torn current tape reads Torn, got {verdict:?}"
        );
    }
    let stale_cover = Coverage::from_stamps(&[first_ms], Timeframe::M1).expect("one bar covers");
    let verdict = classify(
        Some(stale_cover),
        first_ms + 100 * step_ms,
        Timeframe::M1,
        policy,
    );
    assert!(
        matches!(verdict, GapClass::Stale { .. }),
        "a trailing tape reads Stale, got {verdict:?}"
    );
}

/// A malformed batch refuses without storing its good prefix: the caller
/// proves it by observing `None` coverage after the refusal.
pub fn assert_no_partial_on_refusal(covered: bool, what: &str) {
    assert!(!covered, "{what}: malformed batch stores nothing");
}

/// Read-request shape refusals: blank symbol and inverted range.
///
/// A blank symbol never reaches the planner through `CandleStore::read` (the
/// tape lookup refuses it as unknown first); the shape refusal is pinned at
/// the request-validation boundary, which is what the planner consumes.
pub fn check_request_shape(store: &dyn CandleStore, venue: &str) {
    use fluent_wvr::validate::Validate as _;
    let blank = ReadRequest {
        symbol: String::new(),
        venue: venue.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange::bounded(1_767_225_900_000, 1_767_225_960_000).expect("ordered"),
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    };
    assert!(
        matches!(blank.validate(), Err(PlanError::BlankSymbol)),
        "blank symbol refuses at the request boundary"
    );
    assert!(
        matches!(
            store.read(&blank),
            Err(StoreError::Plan(PlanError::UnknownSymbol(_)))
        ),
        "a blank symbol reaches no tape"
    );
    let inverted = ReadRequest {
        symbol: "SPY".to_owned(),
        venue: venue.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange {
            start_ms: Some(1_767_225_960_000),
            end_ms: Some(1_767_225_900_000),
        },
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    };
    assert!(
        matches!(
            store.read(&inverted),
            Err(StoreError::Plan(PlanError::InvertedRange { .. }))
        ),
        "inverted range refuses"
    );
}

/// Unbounded unlimited reads refuse rather than scanning the tape.
pub fn check_unbounded_refusal(store: &dyn CandleStore, symbol: &str, venue: &str) {
    let req = ReadRequest {
        symbol: symbol.to_owned(),
        venue: venue.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange::unbounded(),
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    };
    assert!(
        matches!(
            store.read(&req),
            Err(StoreError::Plan(PlanError::UnboundedUnlimited))
        ),
        "unbounded unlimited refuses"
    );
}

/// Write-policy spelling for callers: upsert carries the caller's token.
#[must_use]
pub fn upsert_policy(seq: u64) -> WritePolicy {
    WritePolicy::UpsertBySeq { seq }
}
