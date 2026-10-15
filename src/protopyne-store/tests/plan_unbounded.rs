//! Open-ended reads: a limit bounds them, its absence refuses them.
//!
//! An unbounded read with no bar limit would scan the whole tape — the
//! performance cliff this planner removes — so it refuses. With a limit the
//! window resolves: end-anchored to a tail, start-anchored to a head, and
//! fully open to the watermark-anchored latest-N window.

use super::*;
use protopyne_types::{Bar, Timeframe};

use crate::commit::test_tapes::{bar, stamp};
use crate::fold::FoldPolicy;
use crate::sqlite_tape::test_setup::{VENUE, seeded, with_memory_caps};
use crate::store::CandleStore;

fn ms(step: i64) -> i64 {
    stamp(step).timestamp_millis()
}

fn request(start: Option<i64>, end: Option<i64>, limit: Option<usize>) -> ReadRequest {
    ReadRequest {
        symbol: "SPY".to_owned(),
        venue: VENUE.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange {
            start_ms: start,
            end_ms: end,
        },
        limit,
        policy: FoldPolicy::ExtendToFullBuckets,
    }
}

fn resolved_spans(plan: &ReadPlan) -> Vec<TimeRange> {
    plan.spans().iter().map(SpanPlan::span).collect()
}

#[tokio::test]
async fn open_on_both_sides_with_no_limit_refuses() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        let coverage = tape.coverage(key).expect("coverage reads");
        assert_eq!(
            ReadPlan::resolve(tape.catalog(), coverage, &request(None, None, None)),
            Err(PlanError::UnboundedUnlimited),
            "no bound and no limit is the scan-everything query"
        );
    })
    .await;
}

#[tokio::test]
async fn open_below_with_no_limit_refuses() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        let coverage = tape.coverage(key).expect("coverage reads");
        assert_eq!(
            ReadPlan::resolve(tape.catalog(), coverage, &request(None, Some(ms(8)), None)),
            Err(PlanError::UnboundedUnlimited),
            "an end anchor without a limit still scans from the beginning of time"
        );
    })
    .await;
}

#[tokio::test]
async fn open_above_with_no_limit_refuses() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        let coverage = tape.coverage(key).expect("coverage reads");
        assert_eq!(
            ReadPlan::resolve(tape.catalog(), coverage, &request(Some(ms(2)), None, None)),
            Err(PlanError::UnboundedUnlimited),
            "a start anchor without a limit still scans to the end of time"
        );
    })
    .await;
}

#[tokio::test]
async fn end_anchored_with_a_limit_resolves_to_a_tail() {
    with_memory_caps(async {
        let bars: Vec<Bar> = (0..10).map(|step| bar(step, "100")).collect();
        let (tape, key) = seeded("SPY", &bars).await;
        let coverage = tape.coverage(key).expect("coverage reads");
        let plan = ReadPlan::resolve(
            tape.catalog(),
            coverage,
            &request(None, Some(ms(8)), Some(3)),
        )
        .expect("a limited tail resolves");
        assert_eq!(
            resolved_spans(&plan),
            vec![TimeRange {
                start_ms: Some(ms(5)),
                end_ms: Some(ms(8)),
            }],
            "three bars ending at step 8"
        );
    })
    .await;
}

#[tokio::test]
async fn start_anchored_with_a_limit_resolves_to_a_head() {
    with_memory_caps(async {
        let bars: Vec<Bar> = (0..10).map(|step| bar(step, "100")).collect();
        let (tape, key) = seeded("SPY", &bars).await;
        let coverage = tape.coverage(key).expect("coverage reads");
        let plan = ReadPlan::resolve(
            tape.catalog(),
            coverage,
            &request(Some(ms(2)), None, Some(3)),
        )
        .expect("a limited head resolves");
        assert_eq!(
            resolved_spans(&plan),
            vec![TimeRange {
                start_ms: Some(ms(2)),
                end_ms: Some(ms(5)),
            }],
            "three bars starting at step 2"
        );
    })
    .await;
}

#[tokio::test]
async fn fully_open_with_a_limit_resolves_against_the_watermark() {
    with_memory_caps(async {
        let bars: Vec<Bar> = (0..10).map(|step| bar(step, "100")).collect();
        let (tape, key) = seeded("SPY", &bars).await;
        let coverage = tape.coverage(key).expect("coverage reads");
        let plan = ReadPlan::resolve(tape.catalog(), coverage, &request(None, None, Some(4)))
            .expect("latest-N resolves");
        assert_eq!(
            resolved_spans(&plan),
            vec![TimeRange {
                start_ms: Some(ms(6)),
                end_ms: Some(ms(10)),
            }],
            "the latest four bars include the watermark bar"
        );
    })
    .await;
}

#[tokio::test]
async fn latest_n_against_an_empty_tape_reports_everything_uncovered() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        let coverage = tape.coverage(key).expect("coverage reads");
        assert_eq!(coverage, None);
        assert_eq!(
            ReadPlan::resolve(tape.catalog(), coverage, &request(None, None, Some(4))),
            Err(PlanError::UncoveredRange {
                span: TimeRange::unbounded(),
                want: Timeframe::M1,
            }),
            "no watermark to anchor against, no coverage to plan from"
        );
    })
    .await;
}
