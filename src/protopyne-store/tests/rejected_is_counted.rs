//! Rejection is counted: an unrepresentable row is a hole the caller can
//! see, never a silent drop.
//!
//! A batch with one over-scale value stores the representable remainder
//! and reports `rejected == 1, inserted == n-1`; the refused row leaves a
//! gap the planner names on a read, and the tape's coverage shows the same
//! gap. The control group proves the other direction: a fully
//! representable batch reports `rejected == 0` and covers without holes.

use super::*;
use protopyne_types::Timeframe;

use crate::catalog::PriceScale;
use crate::commit::test_tapes::bar;
use crate::fold::FoldPolicy;
use crate::range::TimeRange;
use crate::sqlite_tape::test_setup::{VENUE, m1_key, memory_tape, with_memory_caps};
use crate::store::CandleStore;

fn read_window(
    tape: &SqliteTape,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<protopyne_types::Bar>, StoreError> {
    tape.read(&ReadRequest {
        symbol: symbol.to_owned(),
        venue: VENUE.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange::bounded(start_ms, end_ms).expect("ordered window"),
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    })
    .map(|series| series.bars)
}

#[tokio::test]
async fn one_bad_row_counts_and_leaves_a_visible_gap() {
    with_memory_caps(async {
        let tape = memory_tape();
        let symbol_id = tape
            .register(
                "COUNT",
                VENUE,
                crate::catalog::AssetClass::Stock,
                PriceScale::of(2).expect("valid scale"),
                "UTC",
                protopyne_types::Timeframe::M1,
            )
            .expect("registration succeeds");
        let key = m1_key(symbol_id);
        // Four minute bars; the second runs one digit past the scale.
        let batch = vec![
            bar(0, "100.10"),
            bar(1, "100.555"),
            bar(2, "100.20"),
            bar(3, "100.30"),
        ];
        let bad_ts = batch[1].ts.timestamp_millis();
        let outcome = tape
            .append(key, &batch, WritePolicy::InsertIfAbsent)
            .expect("partial refusal still appends");
        assert_eq!(
            outcome,
            AppendOutcome {
                inserted: 3,
                conflicts: 0,
                rejected: 1
            },
            "the remainder lands and the refusal is counted"
        );
        let bars = read_window(
            &tape,
            "COUNT",
            batch[0].ts.timestamp_millis(),
            batch[0].ts.timestamp_millis() + 60_000,
        )
        .expect("the hole-free head reads");
        assert_eq!(bars.len(), 1, "the first row stored");
        let bars = read_window(
            &tape,
            "COUNT",
            batch[2].ts.timestamp_millis(),
            batch[3].ts.timestamp_millis() + 60_000,
        )
        .expect("the hole-free tail reads");
        assert_eq!(bars.len(), 2, "the remainder stored");
        assert!(
            bars.iter().all(|b| b.ts.timestamp_millis() != bad_ts),
            "the refused row is absent from both sides of the gap"
        );
        // A read spanning the gap refuses with the gap named: the refusal
        // is the absence proof, stronger than an empty read.
        match read_window(
            &tape,
            "COUNT",
            batch[0].ts.timestamp_millis(),
            batch[3].ts.timestamp_millis() + 60_000,
        ) {
            Err(StoreError::Plan(PlanError::UncoveredRange { span, want })) => {
                assert_eq!(
                    span,
                    TimeRange::bounded(bad_ts, bad_ts + 60_000).expect("ordered gap"),
                    "the planner names the refused row's span"
                );
                assert_eq!(want, Timeframe::M1);
            }
            other => panic!("a torn window refuses with its gap, got {other:?}"),
        }
        let coverage = tape
            .coverage(key)
            .expect("coverage answers")
            .expect("rows cover");
        assert_eq!(coverage.bars, 3, "coverage counts stored rows");
        let extent = TimeRange {
            start_ms: Some(batch[0].ts.timestamp_millis()),
            end_ms: Some(batch[3].ts.timestamp_millis() + 60_000),
        };
        let holes = coverage.holes(&extent, Timeframe::M1);
        assert!(
            holes.iter().any(|hole| {
                hole.start_ms.is_some_and(|start| start <= bad_ts)
                    && hole.end_ms.is_some_and(|end| bad_ts < end)
            }),
            "coverage reflects the gap the refusal left, got {holes:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn fully_representable_batch_counts_no_rejections() {
    with_memory_caps(async {
        let tape = memory_tape();
        let symbol_id = tape
            .register(
                "CLEAN",
                VENUE,
                crate::catalog::AssetClass::Stock,
                PriceScale::of(2).expect("valid scale"),
                "UTC",
                protopyne_types::Timeframe::M1,
            )
            .expect("registration succeeds");
        let key = m1_key(symbol_id);
        let batch = vec![bar(0, "100.10"), bar(1, "100.20"), bar(2, "100.30")];
        let outcome = tape
            .append(key, &batch, WritePolicy::InsertIfAbsent)
            .expect("clean batch appends");
        assert_eq!(
            outcome,
            AppendOutcome {
                inserted: 3,
                conflicts: 0,
                rejected: 0
            },
            "control group: nothing refused"
        );
        let coverage = tape
            .coverage(key)
            .expect("coverage answers")
            .expect("rows cover");
        assert_eq!(coverage.bars, 3);
        let extent = TimeRange {
            start_ms: Some(batch[0].ts.timestamp_millis()),
            end_ms: Some(batch[2].ts.timestamp_millis() + 60_000),
        };
        assert!(
            coverage.holes(&extent, Timeframe::M1).is_empty(),
            "control group: no holes"
        );
    })
    .await;
}
