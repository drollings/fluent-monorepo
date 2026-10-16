//! Capacity-bound calibration: the one new threshold and its measurement.
//!
//! The bound caps producer memory (the whole store replays into memory at
//! boot), never data trust. These suites pin the refusal shape (tape, bound,
//! and observed count named; nothing partially written), the boundary (exactly
//! `max_bars` loads), the reopen-after-refusal invariant, and the recall
//! control (expected working-set shapes load under the default). The
//! measurement harness prints the resident-bytes table the default is
//! calibrated from; run with `--nocapture` to see it.

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

fn spy_key(tape: &MarketTape) -> TapeKey {
    TapeKey {
        symbol_id: tape.resolve("SPY", VENUE).expect("resolve").expect("known"),
        timeframe: Timeframe::M1,
    }
}

fn register_spy(tape: &MarketTape) -> TapeKey {
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
    TapeKey {
        symbol_id,
        timeframe: Timeframe::M1,
    }
}

fn capacity_message(err: crate::error::StoreError) -> String {
    match err {
        crate::error::StoreError::Capacity(message) => message,
        other => panic!("expected a capacity refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn exactly_max_bars_loads_and_one_more_refuses() {
    with_registry_caps(async {
        let tape = MarketTape::open_in_memory_with_options(
            crate::tape::TapeOptions::new().max_bars(4).build(),
        )
        .expect("opens");
        let key = register_spy(&tape);
        tape.append(key, &grid(FIRST_MS, 4), WritePolicy::InsertIfAbsent)
            .expect("exactly max loads");
        let message = capacity_message(
            tape.append(key, &grid(FIRST_MS + 4 * 60_000, 1), WritePolicy::InsertIfAbsent)
                .expect_err("one over max refuses"),
        );
        assert!(message.contains("SPY"), "names the tape: {message}");
        assert!(message.contains('4'), "names the bound: {message}");
        assert!(message.contains('5'), "names the observed count: {message}");
        // Nothing partially written: the tape still holds exactly four.
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.bars, 4);
    })
    .await;
}

#[tokio::test]
async fn a_restatement_merge_at_capacity_still_lands() {
    // Merges replace in place: they grow nothing, so a full tape still
    // accepts restatements of stamps it already holds.
    with_registry_caps(async {
        let tape = MarketTape::open_in_memory_with_options(
            crate::tape::TapeOptions::new().max_bars(2).build(),
        )
        .expect("opens");
        let key = register_spy(&tape);
        tape.append(key, &grid(FIRST_MS, 2), WritePolicy::InsertIfAbsent)
            .expect("fills to max");
        let mut restated = grid(FIRST_MS, 1).pop().expect("one bar");
        restated.c = "101".parse().expect("decimal");
        restated.h = "101".parse().expect("decimal");
        tape.append(key, &[restated], WritePolicy::UpsertBySeq { seq: 5 })
            .expect("in-place merge lands at capacity");
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.bars, 2);
    })
    .await;
}

#[tokio::test]
async fn reopen_after_refusal_sees_the_unmodified_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    with_registry_caps(async {
        let tape = MarketTape::open(&root).expect("tape opens");
        let key = register_spy(&tape);
        tape.append(key, &grid(FIRST_MS, 6), WritePolicy::InsertIfAbsent)
            .expect("six bars land under the default bound");
    })
    .await;
    // Reopening with a bound below the replayed total refuses with no
    // handle handed out — there is no partial state to serve from.
    let message = with_registry_caps(async {
        capacity_message(
            MarketTape::open_with_options(&root, crate::tape::TapeOptions::new().max_bars(4).build())
                .expect_err("over-budget open refuses"),
        )
    })
    .await;
    assert!(message.contains('4'), "names the bound: {message}");
    assert!(message.contains('6'), "names the replayed total: {message}");
    // And the log on disk is unmodified: a full-bound reopen serves all six.
    with_registry_caps(async {
        let tape = MarketTape::open(&root).expect("full-bound reopen serves");
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
        assert_eq!(series.bars.len(), 6);
    })
    .await;
}

#[tokio::test]
async fn recall_control_expected_working_sets_load_under_the_default() {
    // A SPY-like session block (390 bars) plus the fixture-universe shape
    // (a second small tape) load under the default with room to spare; the
    // default itself holds million-bar working sets.
    const {
        assert!(
            crate::tape::DEFAULT_MAX_BARS >= 1_000_000,
            "the default holds million-bar working sets"
        );
    }
    with_registry_caps(async {
        let tape = MarketTape::open_in_memory().expect("opens");
        let key = register_spy(&tape);
        tape.append(key, &grid(FIRST_MS, 390), WritePolicy::InsertIfAbsent)
            .expect("one session block loads");
        let symbol_id = tape
            .register(
                "QQQ",
                VENUE,
                crate::catalog::AssetClass::Stock,
                crate::catalog::PriceScale::of(2).expect("valid scale"),
                "UTC",
                Timeframe::M1,
            )
            .expect("second market registers");
        let other = TapeKey {
            symbol_id,
            timeframe: Timeframe::M1,
        };
        tape.append(other, &grid(FIRST_MS, 120), WritePolicy::InsertIfAbsent)
            .expect("fixture-universe shape loads");
        assert_eq!(tape.coverage(key).expect("coverage").expect("covered").bars, 390);
        assert_eq!(tape.coverage(other).expect("coverage").expect("covered").bars, 120);
        let _ = spy_key(&tape);
    })
    .await;
}

/// Resident VmRSS in bytes, read from the process status.
fn rss_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("proc status reads");
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .expect("VmRSS present");
    let kb: u64 = line
        .split_whitespace()
        .nth(1)
        .expect("VmRSS value")
        .parse()
        .expect("VmRSS numeric");
    kb * 1024
}

/// One flat bar stamped `ts_ms`, cloned from a shared template: the same
/// bytes the tape holds per row (no adjustment), without paying Decimal
/// parsing per row.
fn flat_at(template: &protopyne_types::Bar, ts_ms: i64) -> protopyne_types::Bar {
    let mut bar = template.clone();
    bar.ts = chrono::DateTime::from_timestamp_millis(ts_ms).expect("representable stamp");
    bar
}

#[test]
fn calibration_resident_bytes_per_bar_and_rss() {
    let template = grid(FIRST_MS, 1).pop().expect("one bar");
    let bar_size = std::mem::size_of::<protopyne_types::Bar>() as u64;
    let stamp_size = std::mem::size_of::<i64>() as u64;
    println!("calibration: size_of Bar = {bar_size} bytes, stamp = {stamp_size} bytes");
    let mut previous_rss = rss_bytes();
    let mut previous_n: u64 = 0;
    // Allocate raw bars plus the parallel stamp slice: exactly the two
    // allocations the tape holds per row, without log I/O.
    for n in [1_000_u64, 100_000, 1_000_000, 5_000_000] {
        let mut bars = Vec::with_capacity(n as usize);
        let mut stamps = Vec::with_capacity(n as usize);
        for i in 0..n {
            let ts = FIRST_MS + (i as i64) * 60_000;
            bars.push(flat_at(&template, ts));
            stamps.push(ts);
        }
        let rss = rss_bytes();
        let delta = rss.saturating_sub(previous_rss);
        let grown = n - previous_n;
        let per_bar = delta as f64 / grown as f64;
        println!(
            "calibration: n = {n:>8}, VmRSS = {rss:>12} bytes, delta = {delta:>12} bytes over {grown:>8} bars, per-bar = {per_bar:>8.1} bytes",
        );
        previous_rss = rss;
        previous_n = n;
        drop((bars, stamps));
    }
    println!(
        "calibration: size_of total per row = {} bytes (Bar + stamp)",
        bar_size + stamp_size
    );
}
