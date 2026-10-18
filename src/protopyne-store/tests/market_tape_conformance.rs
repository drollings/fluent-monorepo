//! `CandleStore` conformance instantiated against the file-backed engine.
//!
//! The trait's promises, exercised through the public interface: proved
//! through the shared helpers rather than plausible behaviour through a
//! second spelling of the rule.

use super::*;
use protopyne_types::Timeframe;

use crate::catalog::PriceScale;
use crate::commit::test_tapes::{at_ms, bar};
use crate::coverage::{classify, FreshnessPolicy, GapClass};
use crate::fold::FoldPolicy;
use crate::key::TapeKey;
use crate::range::TimeRange;
use crate::store::{AppendOutcome, CandleStore, ReadRequest, WritePolicy};
use crate::tape::test_conformance::{
    assert_ascending_valid, assert_no_partial_on_refusal, assert_provenance_shape,
    check_append_policies, check_fold_matrix, check_gap_classes, check_request_shape,
    check_scale_outcome, check_unbounded_refusal, check_unknown_symbol, grid, upsert_policy,
};
use crate::tape::test_setup::{m1_key, memory_tape, seeded, with_registry_caps, VENUE};

fn window_req(symbol: &str, start_ms: i64, end_ms: i64) -> ReadRequest {
    ReadRequest {
        symbol: symbol.to_owned(),
        venue: VENUE.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange::bounded(start_ms, end_ms).expect("ordered"),
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    }
}

#[tokio::test]
async fn reads_come_back_ascending_and_valid_regardless_of_write_order() {
    with_registry_caps(async {
        let (tape, _key) = seeded("SPY", &[]).await;
        let mut shuffled: Vec<Bar> = grid(1_767_225_900_000, 10);
        shuffled.reverse();
        shuffled.swap(2, 3);
        tape.append(
            tape.registry()
                .tape_of("SPY", VENUE)
                .map(|(id, tf)| TapeKey {
                    symbol_id: id,
                    timeframe: tf,
                })
                .expect("registered"),
            &shuffled,
            WritePolicy::InsertIfAbsent,
        )
        .expect("shuffled batch appends");
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
        assert_ascending_valid(&series.bars, "shuffled write");
        assert_provenance_shape(&series, "shuffled write");
    })
    .await;
}

#[tokio::test]
async fn unknown_symbol_refuses_by_name() {
    with_registry_caps(async {
        let (tape, _key) = seeded("SPY", &grid(1_767_225_900_000, 4)).await;
        check_unknown_symbol(&tape, VENUE);
    })
    .await;
}

#[tokio::test]
async fn append_policies_count_insert_conflict_reject() {
    with_registry_caps(async {
        let (tape, key) = seeded("SPY", &grid(1_767_225_900_000, 4)).await;
        let redeliver = tape
            .append(
                key,
                &grid(1_767_225_900_000, 4),
                WritePolicy::InsertIfAbsent,
            )
            .expect("re-delivery appends");
        assert_eq!(
            redeliver,
            AppendOutcome {
                inserted: 0,
                conflicts: 4,
                rejected: 0,
            }
        );
        let mut restated = grid(1_767_225_900_000, 1).pop().expect("one bar");
        restated.c = "101".parse().expect("decimal");
        restated.h = "101".parse().expect("decimal");
        let upsert = tape
            .append(key, &[restated], upsert_policy(7))
            .expect("upsert appends");
        check_append_policies(redeliver, upsert);
        let series = tape
            .read(&window_req(
                "SPY",
                1_767_225_900_000 - 60_000,
                1_767_225_900_000 + 4 * 60_000,
            ))
            .expect("read back");
        assert_ascending_valid(&series.bars, "upsert read");
    })
    .await;
}

#[tokio::test]
async fn scale_rejection_is_counted_not_stored() {
    with_registry_caps(async {
        let tape = memory_tape();
        let symbol_id = tape
            .register(
                "SCALE",
                VENUE,
                crate::catalog::AssetClass::Stock,
                PriceScale::of(2).expect("valid scale"),
                "UTC",
                Timeframe::M1,
            )
            .expect("registration succeeds");
        let key = m1_key(symbol_id);
        let batch = vec![bar(0, "100.10"), bar(1, "100.555"), bar(2, "100.20")];
        let outcome = tape
            .append(key, &batch, WritePolicy::InsertIfAbsent)
            .expect("partial append lands");
        check_scale_outcome(outcome, 2);
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.bars, 2);
    })
    .await;
}

#[tokio::test]
async fn repair_one_overwrites_and_refuses_when_absent() {
    with_registry_caps(async {
        let first = grid(1_767_225_900_000, 1).pop().expect("one bar");
        let (tape, key) = seeded("SPY", std::slice::from_ref(&first)).await;
        let mut replacement = first.clone();
        replacement.c = "101".parse().expect("decimal");
        replacement.h = "101".parse().expect("decimal");
        tape.repair_one(key, replacement.ts.timestamp_millis(), &replacement)
            .expect("repair overwrites");
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
        assert_eq!(series.bars[0].c.to_string(), "101");
        let missing_ts = replacement.ts.timestamp_millis() + 60_000;
        let missing_bar = at_ms(missing_ts);
        tape.repair_one(key, missing_ts, &missing_bar)
            .expect_err("repair of an absent row refuses");
        check_request_shape(&tape, VENUE);
        check_unbounded_refusal(&tape, "SPY", VENUE);
    })
    .await;
}

#[tokio::test]
async fn coverage_and_gap_classes_hold() {
    check_gap_classes(1_767_225_900_000, 60_000);
    with_registry_caps(async {
        let (tape, key) = seeded("SPY", &grid(1_767_225_900_000, 10)).await;
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.bars, 10);
        let policy = FreshnessPolicy::new().build();
        let verdict = classify(
            Some(coverage),
            1_767_225_900_000 + 10 * 60_000,
            Timeframe::M1,
            policy,
        );
        assert_eq!(verdict, GapClass::Fresh);
    })
    .await;
}

#[tokio::test]
async fn fold_policy_matrix_holds_over_a_real_run() {
    let bars = grid(1_700_000_000_000, 120);
    check_fold_matrix(&bars);
}

#[tokio::test]
async fn malformed_batch_stores_nothing() {
    with_registry_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        let good = bar(0, "100");
        let mut malformed = bar(1, "100");
        malformed.h = "99".parse().expect("decimal");
        tape.append(key, &[good, malformed], WritePolicy::InsertIfAbsent)
            .expect_err("malformed batch refuses");
        let covered = tape.coverage(key).expect("coverage").is_some();
        assert_no_partial_on_refusal(covered, "malformed batch");
    })
    .await;
}

#[tokio::test]
async fn whole_tape_coverage_shape_holds() {
    with_registry_caps(async {
        let bars = grid(1_767_225_900_000, 6);
        let (tape, key) = seeded("SPY", &bars).await;
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.bars, 6);
        let series = tape
            .read(&ReadRequest {
                symbol: "SPY".to_owned(),
                venue: VENUE.to_owned(),
                timeframe: Timeframe::M1,
                range: TimeRange::bounded(1_767_225_900_000, 1_767_225_900_000 + 6 * 60_000)
                    .expect("ordered"),
                limit: None,
                policy: FoldPolicy::ExtendToFullBuckets,
            })
            .expect("bounded read");
        assert_eq!(series.bars.len(), 6);
        assert_provenance_shape(&series, "bounded read");
        assert_ascending_valid(&series.bars, "bounded read");
    })
    .await;
}

#[tokio::test]
async fn upsert_restatement_folds_through_the_one_restatement_rule() {
    // A divergent restatement (moved open) folds rather than overwrites:
    // the open stays the first print's, the close becomes the latest
    // print's, the range only widens. This is the `KeyedSnapshot::merge`
    // invariant the tape log shares with ingest and the emitter.
    with_registry_caps(async {
        let (tape, key) = seeded("SPY", &grid(1_767_225_900_000, 2)).await;
        let mut restated = grid(1_767_225_900_000, 1).pop().expect("one bar");
        restated.o = "99".parse().expect("decimal");
        restated.l = "99".parse().expect("decimal");
        restated.c = "101".parse().expect("decimal");
        restated.h = "101".parse().expect("decimal");
        let outcome = tape
            .append(key, &[restated], upsert_policy(9))
            .expect("upsert appends");
        assert_eq!(outcome.conflicts, 1);
        let series = tape
            .read(&window_req(
                "SPY",
                1_767_225_900_000 - 60_000,
                1_767_225_900_000 + 2 * 60_000,
            ))
            .expect("read back");
        let first = &series.bars[0];
        assert_eq!(
            first.o.to_string(),
            "100",
            "the open stays the first print's"
        );
        assert_eq!(
            first.c.to_string(),
            "101",
            "the close becomes the latest print's"
        );
        assert_eq!(first.h.to_string(), "101", "the range widens up");
        assert_eq!(first.l.to_string(), "99", "the range widens down");
    })
    .await;
}
