//! Explicit compaction: seal, then truncate, in that order.
//!
//! Compaction is an operator action, never automatic. It seals the whole
//! tape (artifact plus manifest row durable) and only then truncates the
//! log. A crash between the two steps must converge on reopen: the sealed
//! artifact plus the untruncated log replay to the same bars, and the next
//! compaction finishes the truncation.

use super::*;
use protopyne_types::Timeframe;

use crate::fold::FoldPolicy;
use crate::key::TapeKey;
use crate::range::TimeRange;
use crate::tape::test_conformance::grid;
use crate::store::{CandleStore, ReadRequest, WritePolicy};
use crate::tape::test_setup::with_registry_caps;

const VENUE: &str = "test-venue";
const FIRST_MS: i64 = 1_767_225_900_000;
const SEAL_NOW_MS: i64 = 1_767_225_600_000;
/// The log file for the first-registered market's minute tape.
const LOG_FILE: &str = "0000000001-M1.csv";

fn all_bars(tape: &MarketTape) -> Vec<protopyne_types::Bar> {
    tape.read(&ReadRequest {
        symbol: "SPY".to_owned(),
        venue: VENUE.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange::unbounded(),
        limit: Some(10_000),
        policy: FoldPolicy::ExtendToFullBuckets,
    })
    .expect("read back")
    .bars
}

fn spy_key(tape: &MarketTape) -> TapeKey {
    TapeKey {
        symbol_id: tape.resolve("SPY", VENUE).expect("resolve").expect("known"),
        timeframe: Timeframe::M1,
    }
}

fn register_and_seed(tape: &MarketTape) -> TapeKey {
    let symbol_id = tape
        .register(
            "SPY",
            VENUE,
            crate::catalog::AssetClass::Stock,
            crate::catalog::PriceScale::of(2).expect("valid scale"),
            "UTC",
            Timeframe::M1,
        )
        .expect("register");
    let key = TapeKey {
        symbol_id,
        timeframe: Timeframe::M1,
    };
    tape.append(key, &grid(FIRST_MS, 16), WritePolicy::InsertIfAbsent)
        .expect("seed appends");
    key
}

#[tokio::test]
async fn compact_seals_then_truncates_and_reopen_sees_every_bar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let before = with_registry_caps(async {
        let tape = MarketTape::open(&root).expect("tape opens");
        register_and_seed(&tape);
        let sealed = tape
            .compact(spy_key(&tape), crate::snapshot::SealOptions::new().build(), SEAL_NOW_MS)
            .expect("compact seals");
        assert_eq!(sealed.row_count, 16);
        all_bars(&tape)
    })
    .await;
    let probe = std::fs::read_to_string(root.join("tapes").join(LOG_FILE)).expect("log reads");
    assert_eq!(
        probe.lines().count(),
        2,
        "the log truncates to version + header, got {} bytes",
        probe.len()
    );
    // Reopen loads the sealed artifact: every bar survives the truncation.
    let after = with_registry_caps(async { MarketTape::open(&root).expect("tape reopens") }).await;
    let after_bars = with_registry_caps(async { all_bars(&after) }).await;
    assert_eq!(after_bars, before, "reopen after compact sees every bar");
    // Appends continue past the compaction point.
    with_registry_caps(async {
        after
            .append(spy_key(&after), &grid(FIRST_MS + 16 * 60_000, 4), WritePolicy::InsertIfAbsent)
            .expect("post-compact append lands");
        assert_eq!(all_bars(&after).len(), 20);
    })
    .await;
    let reopened = with_registry_caps(async { MarketTape::open(&root).expect("tape reopens") }).await;
    let reopened_bars = with_registry_caps(async { all_bars(&reopened) }).await;
    assert_eq!(reopened_bars.len(), 20, "post-compact rows replay too");
}

#[tokio::test]
async fn crash_between_seal_and_truncate_converges_on_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let before = with_registry_caps(async {
        let tape = MarketTape::open(&root).expect("tape opens");
        let key = register_and_seed(&tape);
        // Step one of compaction without step two: the seal is durable,
        // the log is untruncated. A crash here must lose nothing.
        let sealed = crate::snapshot::seal(
            tape.registry(),
            key,
            &all_bars(&tape),
            &root.join("seals"),
            crate::snapshot::SealOptions::new().build(),
            SEAL_NOW_MS,
        )
        .expect("manual seal");
        assert_eq!(sealed.row_count, 16);
        all_bars(&tape)
    })
    .await;
    // Reopen loads the seal and replays the untruncated log over it: the
    // same bars, none doubled, none lost.
    let after = with_registry_caps(async { MarketTape::open(&root).expect("tape reopens") }).await;
    let after_bars = with_registry_caps(async { all_bars(&after) }).await;
    assert_eq!(after_bars, before, "seal plus untruncated log converges");
    // The next compaction finishes the truncation.
    with_registry_caps(async {
        after
            .compact(spy_key(&after), crate::snapshot::SealOptions::new().build(), SEAL_NOW_MS)
            .expect("second compact finishes");
    })
    .await;
    let finished = with_registry_caps(async { MarketTape::open(&root).expect("tape reopens") }).await;
    let finished_bars = with_registry_caps(async { all_bars(&finished) }).await;
    assert_eq!(finished_bars, before, "the finished compaction keeps every bar");
}

#[tokio::test]
async fn reopen_with_duplicate_stamps_and_a_torn_tail_replays_every_bar() {
    // Boot-replay equivalence fixture: duplicate-stamp rows (inserts meeting
    // a present stamp, one replace) plus a torn tail must converge to the
    // live state on reopen — the indexed replay folds to the identical tape
    // the naive per-row scan produced.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let expected = with_registry_caps(async {
        let tape = MarketTape::open(&root).expect("tape opens");
        let key = register_and_seed(&tape);
        // Duplicate stamps under `InsertIfAbsent`: the first row wins, the
        // rerun only counts conflicts.
        let outcome = tape
            .append(key, &grid(FIRST_MS, 16), WritePolicy::InsertIfAbsent)
            .expect("duplicate append runs");
        assert_eq!(outcome.inserted, 0, "every stamp was already present");
        // One committed row corrected through the replace path.
        let mut fixed = grid(FIRST_MS, 1).pop().expect("one bar");
        fixed.c = "101".parse().expect("decimal");
        fixed.h = "101".parse().expect("decimal");
        tape.repair_one(key, fixed.ts.timestamp_millis(), &fixed).expect("repair lands");
        // Four novel bars past the seed.
        tape.append(key, &grid(FIRST_MS + 16 * 60_000, 4), WritePolicy::InsertIfAbsent)
            .expect("tail append lands");
        all_bars(&tape)
    })
    .await;
    assert_eq!(expected.len(), 20, "seed plus tail, duplicates absorbed");
    // Tear the log tail after the handle drops: a partial record with no
    // terminator. Reopen repairs it and replays to the same bars.
    {
        use std::io::Write as _;
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("tapes").join(LOG_FILE))
            .expect("log appends");
        log.write_all(b"insert,1799").expect("torn bytes land");
        log.flush().expect("torn bytes flush");
    }
    let after = with_registry_caps(async { MarketTape::open(&root).expect("tape reopens") }).await;
    let after_bars = with_registry_caps(async { all_bars(&after) }).await;
    assert_eq!(after_bars, expected, "torn tail plus duplicates converge on reopen");
}

#[test]
fn indexed_replay_folds_duplicate_stamps_like_the_naive_rule() {
    // The boot fold, both ways: the indexed replay must produce exactly the
    // tape the naive per-row scan produces — inserts keep the first row,
    // replaces overwrite (or push when novel) — over duplicate stamps.
    use super::{TapeOp, replay_log_records};
    use crate::store::order_and_validate;
    let sealed = grid(FIRST_MS, 8);
    let novel = grid(FIRST_MS + 8 * 60_000, 4);
    let mut fixed = sealed[3].clone();
    fixed.c = "101".parse().expect("decimal");
    fixed.h = "101".parse().expect("decimal");
    let rows = vec![
        (TapeOp::Insert, sealed[0].clone()), // duplicate insert: ignored
        (TapeOp::Insert, novel[0].clone()),  // novel insert: pushes
        (TapeOp::Replace, fixed.clone()),    // replace present: overwrites
        (TapeOp::Insert, sealed[0].clone()), // duplicate again: still ignored
        (TapeOp::Replace, novel[1].clone()), // replace novel: pushes
        (TapeOp::Insert, novel[0].clone()),  // now-present insert: ignored
        (TapeOp::Replace, fixed.clone()),    // same replace twice: idempotent
    ];
    // The naive per-row scan: the rule the live path documents.
    let mut linear = sealed.clone();
    for (op, bar) in &rows {
        match op {
            TapeOp::Insert => {
                if !linear.iter().any(|held| held.ts == bar.ts) {
                    linear.push(bar.clone());
                }
            }
            TapeOp::Replace => {
                if let Some(held) = linear.iter_mut().find(|held| held.ts == bar.ts) {
                    *held = bar.clone();
                } else {
                    linear.push(bar.clone());
                }
            }
        }
    }
    let indexed = replay_log_records(sealed, &rows);
    let linear = order_and_validate(linear).expect("naive fold validates");
    let indexed = order_and_validate(indexed).expect("indexed fold validates");
    assert_eq!(indexed, linear, "the index folds to the identical tape");
    assert_eq!(indexed.len(), 8 + 2, "eight sealed plus two novel stamps");
}
