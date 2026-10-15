//! Append idempotence: re-delivery is counted, never duplicated.
//!
//! The control group matters here. A batch delivered twice must store
//! once and say so — `inserted == 0, conflicts == n` — with the series
//! byte-identical afterwards. An interior duplicate inside one batch is
//! the same fact at a smaller scale: no error, one row. The negative
//! control proves the other direction: a batch that precedes the
//! watermark is not silently inserted, it is counted as conflicting so
//! the caller can route to repair.

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::{at_ms, bar};
use crate::fold::FoldPolicy;
use crate::range::TimeRange;
use crate::sqlite_tape::test_setup::{VENUE, seeded, with_memory_caps};

/// Ten flat minute bars starting at `first_ms`, one per 60_000 ms.
fn grid(first_ms: i64, count: i64) -> Vec<Bar> {
    (0..count)
        .map(|step| at_ms(first_ms + step * 60_000))
        .collect()
}

#[tokio::test]
async fn double_append_counts_conflicts_and_stores_once() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &grid(1_767_225_900_000, 10)).await;
        let batch = grid(1_767_225_900_000, 10);
        let outcome = tape
            .append(key, &batch, WritePolicy::InsertIfAbsent)
            .expect("re-delivery appends");
        assert_eq!(
            outcome,
            AppendOutcome {
                inserted: 0,
                conflicts: 10,
                rejected: 0,
            },
            "re-delivery stores nothing and says so in counts, not silence"
        );
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
        assert_eq!(
            series.bars.len(),
            10,
            "the series holds one copy of each bar"
        );
    })
    .await;
}

#[tokio::test]
async fn interior_duplicate_in_one_batch_stores_one_row() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        let one = bar(0, "100");
        let outcome = tape
            .append(key, &[one.clone(), one], WritePolicy::InsertIfAbsent)
            .expect("interior duplicate appends");
        assert_eq!(outcome.inserted, 1);
        assert_eq!(outcome.conflicts, 1);
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.bars, 1, "one row, not two");
    })
    .await;
}

#[tokio::test]
async fn upsert_overwrites_and_counts_the_collision() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[bar(0, "100")]).await;
        let restated = bar(0, "101");
        let outcome = tape
            .append(key, &[restated], WritePolicy::UpsertBySeq { seq: 7 })
            .expect("upsert appends");
        assert_eq!(outcome.inserted, 0);
        assert_eq!(
            outcome.conflicts, 1,
            "an overwrite is a resolved collision, not an insert"
        );
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.bars, 1, "still one row");
        let series = tape
            .read(&ReadRequest {
                symbol: "SPY".to_owned(),
                venue: VENUE.to_owned(),
                timeframe: Timeframe::M1,
                range: TimeRange::unbounded(),
                limit: Some(1),
                policy: FoldPolicy::ExtendToFullBuckets,
            })
            .expect("read back");
        assert_eq!(
            series.bars[0].c.to_string(),
            "101",
            "the restated close overwrote the stored one"
        );
    })
    .await;
}

#[tokio::test]
async fn batch_preceding_the_watermark_conflicts_instead_of_inserting() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &grid(1_767_225_900_000, 10)).await;
        // Every bar precedes the watermark: nothing new to store.
        let stale = grid(1_767_225_900_000, 5);
        let outcome = tape
            .append(key, &stale, WritePolicy::InsertIfAbsent)
            .expect("stale append appends");
        assert_eq!(
            outcome.inserted, 0,
            "a batch below the watermark inserts nothing silently"
        );
        assert_eq!(
            outcome.conflicts, 5,
            "the caller learns the count and can route to repair"
        );
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.bars, 10, "the tape is unchanged");
    })
    .await;
}

#[tokio::test]
async fn malformed_batch_refuses_without_storing_its_prefix() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        let good = bar(0, "100");
        let mut malformed = bar(1, "100");
        malformed.h = "99".parse().expect("decimal");
        let err = tape
            .append(key, &[good, malformed], WritePolicy::InsertIfAbsent)
            .expect_err("malformed batch refuses");
        assert!(
            matches!(err, StoreError::InvalidBar(_)),
            "a bad bar is a caller bug with a reason: {err:?}"
        );
        let coverage = tape.coverage(key).expect("coverage");
        assert!(
            coverage.is_none(),
            "the good prefix is not stored ahead of the refusal: atomic, not partial"
        );
    })
    .await;
}
