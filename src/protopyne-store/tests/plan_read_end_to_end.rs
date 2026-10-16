//! Seed → resolve → read: the planner and the reader agree.
//!
//! The plan promises spans; the read fetches exactly them, folds when the
//! requested spacing is coarser, and proves the output ascending and valid.
//! Both halves run here so a drift between "what the planner said" and
//! "what the reader served" fails loudly instead of reaching a runner.

use super::*;
use protopyne_types::{Bar, Timeframe, validate_new_bar};

use crate::commit::test_tapes::{bar, stamp};
use crate::fold::FoldPolicy;
use crate::plan::SpanPlan;
use crate::range::TimeRange;
use crate::tape::test_setup::{VENUE, seeded, with_registry_caps};
use crate::store::CandleStore;

fn ms(step: i64) -> i64 {
    stamp(step).timestamp_millis()
}

fn read_request(timeframe: Timeframe, start: i64, end: i64) -> ReadRequest {
    ReadRequest {
        symbol: "SPY".to_owned(),
        venue: VENUE.to_owned(),
        timeframe,
        range: TimeRange::bounded(ms(start), ms(end)).expect("ordered fixture bounds"),
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    }
}

fn assert_ascending_and_valid(bars: &[Bar], context: &str) {
    let mut last: Option<&Bar> = None;
    for bar in bars {
        if let Some(prev) = last {
            assert!(
                bar.ts > prev.ts,
                "{context}: stamps ascend, {} follows {}",
                bar.ts.timestamp_millis(),
                prev.ts.timestamp_millis()
            );
        }
        validate_new_bar(last, bar)
            .unwrap_or_else(|e| panic!("{context}: every adjacent pair validates, refused: {e}"));
        last = Some(bar);
    }
}

#[tokio::test]
async fn seed_resolve_read_agrees_on_a_direct_read() {
    with_registry_caps(async {
        let bars: Vec<Bar> = (0..10).map(|step| bar(step, "100")).collect();
        let (tape, key) = seeded("SPY", &bars).await;
        let request = read_request(Timeframe::M1, 0, 10);
        let coverage = tape.coverage(key).expect("coverage reads");
        let plan = ReadPlan::resolve_for(Some((key.symbol_id, key.timeframe)), coverage, &request).expect("plan resolves");
        let series = tape.read(&request).expect("read serves");
        assert_eq!(series.bars.len(), 10);
        assert_ascending_and_valid(&series.bars, "direct read");
        assert_eq!(series.key, key, "the series names the stored tape");
        assert_eq!(
            series.served,
            TimeRange::bounded(ms(0), ms(10)).expect("ordered fixture bounds"),
            "served echoes the bounded window asked for"
        );
        assert_eq!(
            series.provenance,
            plan.spans().to_vec(),
            "the series carries the plan's spans as its provenance"
        );
        assert!(
            series.provenance.iter().all(SpanPlan::complete),
            "a fully covered direct read reports whole spans"
        );
    })
    .await;
}

#[tokio::test]
async fn seed_resolve_read_agrees_on_a_folded_read() {
    with_registry_caps(async {
        let bars: Vec<Bar> = (0..10).map(|step| bar(step, "100")).collect();
        let (tape, key) = seeded("SPY", &bars).await;
        let request = read_request(Timeframe::M5, 0, 10);
        let coverage = tape.coverage(key).expect("coverage reads");
        let plan = ReadPlan::resolve_for(Some((key.symbol_id, key.timeframe)), coverage, &request).expect("plan resolves");
        let series = tape.read(&request).expect("folded read serves");
        assert_eq!(
            series.bars.len(),
            2,
            "ten minute bars fold to two five-minute bars"
        );
        assert_ascending_and_valid(&series.bars, "folded read");
        assert_eq!(series.key, key, "the series still names the stored tape");
        assert_eq!(
            series.provenance,
            plan.spans().to_vec(),
            "folded reads carry provenance too"
        );
        assert!(
            series.provenance.iter().all(SpanPlan::complete),
            "whole buckets report whole"
        );
    })
    .await;
}
