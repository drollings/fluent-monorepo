//! The ordering regression test: the store sorts, callers inherit safety.
//!
//! The runner never sorts or validates its input — it trusts whatever
//! order its history source produced. A store arm that returned rows in
//! index or repair order would feed unordered bars straight into kernels,
//! silently. This suite pins the boundary that prevents it: a batch
//! written shuffled reads back ascending, with every adjacent pair
//! passing the shared monotonicity check. The existing runner path is
//! untouched by this milestone, so its own suites passing in the gate is
//! the second half of the regression proof.

use super::*;
use protopyne_types::{Timeframe, validate_new_bar};

use crate::commit::test_tapes::at_ms;
use crate::fold::FoldPolicy;
use crate::range::TimeRange;
use crate::sqlite_tape::test_setup::{VENUE, seeded, with_memory_caps};

/// Ten minute bars, deliberately shuffled: reversed with two neighbours
/// swapped back, so the input is neither sorted nor reversed.
fn shuffled() -> Vec<Bar> {
    let mut bars: Vec<Bar> = (0..10_i64)
        .map(|step| at_ms(1_767_225_900_000 + step * 60_000))
        .collect();
    bars.reverse();
    bars.swap(2, 3);
    bars
}

#[tokio::test]
async fn shuffled_insert_reads_back_ascending_and_valid() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        let outcome = tape
            .append(key, &shuffled(), WritePolicy::InsertIfAbsent)
            .expect("shuffled batch appends");
        assert_eq!(outcome.inserted, 10);
        let series = tape
            .read(&ReadRequest {
                symbol: "SPY".to_owned(),
                venue: VENUE.to_owned(),
                timeframe: Timeframe::M1,
                range: TimeRange::unbounded(),
                limit: Some(10),
                policy: FoldPolicy::ExtendToFullBuckets,
            })
            .expect("read back");
        assert_eq!(series.bars.len(), 10);
        let stamps: Vec<i64> = series
            .bars
            .iter()
            .map(|bar| bar.ts.timestamp_millis())
            .collect();
        let mut sorted = stamps.clone();
        sorted.sort_unstable();
        assert_eq!(
            stamps, sorted,
            "read establishes ascending order; it never assumes it"
        );
        let mut last: Option<&Bar> = None;
        for bar in &series.bars {
            validate_new_bar(last, bar).expect("every adjacent pair validates");
            last = Some(bar);
        }
        // The served window is the resolved latest-10 window, bounded at
        // the watermark; the bars prove the order.
        assert_eq!(
            series.served,
            TimeRange::bounded(1_767_225_900_000, 1_767_225_900_000 + 10 * 60_000)
                .expect("ordered bounds")
        );
    })
    .await;
}

#[tokio::test]
async fn ranged_read_returns_only_its_window_still_ascending() {
    with_memory_caps(async {
        let (tape, _key) = seeded("SPY", &shuffled()).await;
        let start = 1_767_225_900_000 + 3 * 60_000;
        let end = 1_767_225_900_000 + 7 * 60_000;
        let series = tape
            .read(&ReadRequest {
                symbol: "SPY".to_owned(),
                venue: VENUE.to_owned(),
                timeframe: Timeframe::M1,
                range: TimeRange::bounded(start, end).expect("bounded"),
                limit: None,
                policy: FoldPolicy::ExtendToFullBuckets,
            })
            .expect("ranged read");
        assert_eq!(
            series.bars.len(),
            4,
            "half-open [start, end): bars 3, 4, 5, 6"
        );
        assert_eq!(series.bars[0].ts.timestamp_millis(), start);
        assert_eq!(series.bars[3].ts.timestamp_millis(), end - 60_000);
    })
    .await;
}
