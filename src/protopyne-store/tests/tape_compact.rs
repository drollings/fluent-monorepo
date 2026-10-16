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
