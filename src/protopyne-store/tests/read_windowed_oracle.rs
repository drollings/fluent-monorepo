//! Windowed stamp planning serves what whole-tape planning served.
//!
//! The oracle in `common/oracle.rs` rebuilds the pre-change algorithm from
//! public pieces (whole-tape coverage, resolved plan, per-span fetches), so
//! every case below proves the read path serves identical bars, windows,
//! provenance spans, and refusals — not merely plausible ones. The two
//! corrupt-row cases pin the one stated behavior change: a row that cannot
//! materialize *outside* the served window no longer refuses the read,
//! while a corrupt row *inside* it still refuses, because served bars stay
//! double-validated.

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::at_ms;
use crate::fold::FoldPolicy;
use crate::key::TapeKey;
use crate::range::TimeRange;
use crate::sqlite_tape::test_setup::{VENUE, seeded, with_memory_caps};
use crate::sqlite_tape::fetch_bars;
use crate::store::{CandleStore, ReadRequest};
use crate::sqlite_tape::test_oracle::{assert_same_outcome, oracle_from_pieces};

const STEP_MS: i64 = 60_000;
const BASE_MS: i64 = 1_767_225_900_000;

fn bars_from(first_ms: i64, count: i64) -> Vec<Bar> {
    (0..count).map(|step| at_ms(first_ms + step * STEP_MS)).collect()
}

fn read_req(
    timeframe: Timeframe,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    limit: Option<usize>,
    policy: FoldPolicy,
) -> ReadRequest {
    ReadRequest {
        symbol: "SPY".to_owned(),
        venue: VENUE.to_owned(),
        timeframe,
        range: TimeRange {
            start_ms,
            end_ms,
        },
        limit,
        policy,
    }
}

fn requests() -> Vec<(&'static str, ReadRequest)> {
    vec![
        (
            "bounded on-grid mid-tape",
            read_req(
                Timeframe::M1,
                Some(BASE_MS + 2 * STEP_MS),
                Some(BASE_MS + 8 * STEP_MS),
                None,
                FoldPolicy::ExtendToFullBuckets,
            ),
        ),
        (
            "bounded off-grid start and end",
            read_req(
                Timeframe::M1,
                Some(BASE_MS + 2 * STEP_MS + 30_000),
                Some(BASE_MS + 8 * STEP_MS + 45_000),
                None,
                FoldPolicy::ExtendToFullBuckets,
            ),
        ),
        (
            "open start with limit",
            read_req(
                Timeframe::M1,
                None,
                Some(BASE_MS + 10 * STEP_MS),
                Some(5),
                FoldPolicy::ExtendToFullBuckets,
            ),
        ),
        (
            "open end with limit",
            read_req(
                Timeframe::M1,
                Some(BASE_MS + 6 * STEP_MS),
                None,
                Some(5),
                FoldPolicy::ExtendToFullBuckets,
            ),
        ),
        (
            "latest-N window",
            read_req(
                Timeframe::M1,
                None,
                None,
                Some(6),
                FoldPolicy::ExtendToFullBuckets,
            ),
        ),
        (
            "folded M5 extend",
            read_req(
                Timeframe::M5,
                Some(BASE_MS),
                Some(BASE_MS + 12 * STEP_MS),
                None,
                FoldPolicy::ExtendToFullBuckets,
            ),
        ),
        (
            "folded M5 truncate",
            read_req(
                Timeframe::M5,
                Some(BASE_MS + STEP_MS),
                Some(BASE_MS + 11 * STEP_MS),
                None,
                FoldPolicy::TruncateEdges,
            ),
        ),
        (
            "folded H1 latest-N",
            read_req(
                Timeframe::H1,
                None,
                None,
                Some(1),
                FoldPolicy::ExtendToFullBuckets,
            ),
        ),
        (
            "unknown symbol",
            ReadRequest {
                symbol: "NOBODY".to_owned(),
                ..read_req(Timeframe::M1, Some(BASE_MS), Some(BASE_MS + STEP_MS), None, FoldPolicy::ExtendToFullBuckets)
            },
        ),
        (
            "unbounded unlimited",
            read_req(Timeframe::M1, None, None, None, FoldPolicy::ExtendToFullBuckets),
        ),
    ]
}

fn check_all(tape: &SqliteTape, key: TapeKey, tape_name: &str) {
    // The connection guard is taken per fetch, never held across a
    // `coverage` or `read` call: those lock the same mutex internally, and
    // holding it here would deadlock against the non-reentrant guard.
    for (name, req) in requests() {
        let what = format!("{tape_name}: {name}");
        let lookup = tape.catalog().tape_of(&req.symbol, &req.venue);
        let expected = oracle_from_pieces(
            lookup,
            tape.coverage(TapeKey {
                symbol_id: key.symbol_id,
                timeframe: Timeframe::M1,
            })
            .expect("oracle coverage reads"),
            &|start, end| {
                let conn = common_core::sync::lock(tape.catalog().connection());
                fetch_bars(&conn, key.symbol_id, start, end).map_err(StoreError::from)
            },
            &req,
        );
        // The oracle resolves unknown symbols itself; the live read must
        // refuse identically through its own lookup.
        let actual = tape.read(&req);
        assert_same_outcome(&actual, &expected, &what);
    }
}

#[tokio::test]
async fn windowed_reads_match_the_oracle_on_a_contiguous_tape() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &bars_from(BASE_MS, 12)).await;
        check_all(&tape, key, "contiguous");
    })
    .await;
}

#[tokio::test]
async fn windowed_reads_match_the_oracle_on_a_torn_tape() {
    with_memory_caps(async {
        let bars = bars_from(BASE_MS, 12);
        // Every third bar missing, plus one wide torn stretch.
        let bars = bars
            .into_iter()
            .enumerate()
            .filter(|(index, _)| index % 3 != 0 && !(5..8).contains(index))
            .map(|(_, bar)| bar)
            .collect::<Vec<_>>();
        let (tape, key) = seeded("SPY", &bars).await;
        check_all(&tape, key, "torn");
        // A window crossing the torn stretch refuses exactly as before.
        let req = read_req(
            Timeframe::M1,
            Some(BASE_MS + 2 * STEP_MS),
            Some(BASE_MS + 10 * STEP_MS),
            None,
            FoldPolicy::ExtendToFullBuckets,
        );
        let outcome = tape.read(&req);
        let refusal = outcome.expect_err("a torn window refuses");
        assert!(
            matches!(
                refusal,
                StoreError::Plan(crate::error::PlanError::UncoveredRange { .. })
            ),
            "the torn window refuses uncovered, not otherwise: {refusal:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn windowed_reads_match_the_oracle_on_an_empty_tape() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        check_all(&tape, key, "empty");
    })
    .await;
}

fn plant_undecodable_row(symbol_id: crate::key::SymbolId, ts_ms: i64, tape: &SqliteTape) {
    let conn = common_core::sync::lock(tape.catalog().connection());
    // Replace whatever the seed wrote at this stamp: the corrupt row must
    // sit where a served fetch visits it, and the primary key forbids a
    // second row at the same stamp.
    conn.execute(
        "DELETE FROM candle WHERE symbol_id = ?1 AND ts_ms = ?2",
        rusqlite::params![i64::from(symbol_id.0), ts_ms],
    )
    .expect("clear the stamp");
    conn.execute(
        "INSERT INTO candle(symbol_id, ts_ms, open, high, low, close, volume, adj)
         VALUES (?1, ?2, 'bogus', '100', '100', '100', 1000, NULL)",
        rusqlite::params![i64::from(symbol_id.0), ts_ms],
    )
    .expect("plant an undecodable row");
}

#[tokio::test]
async fn a_corrupt_row_outside_the_window_no_longer_refuses_the_read() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &bars_from(BASE_MS, 12)).await;
        plant_undecodable_row(key.symbol_id, BASE_MS - STEP_MS, &tape);
        let req = read_req(
            Timeframe::M1,
            Some(BASE_MS + 2 * STEP_MS),
            Some(BASE_MS + 8 * STEP_MS),
            None,
            FoldPolicy::ExtendToFullBuckets,
        );
        let series = tape
            .read(&req)
            .expect("a corrupt row outside the window does not refuse the read");
        assert_eq!(series.bars.len(), 6);
        assert_eq!(
            series.bars.first().expect("first bar").ts.timestamp_millis(),
            BASE_MS + 2 * STEP_MS
        );
        assert_eq!(
            series.bars.last().expect("last bar").ts.timestamp_millis(),
            BASE_MS + 7 * STEP_MS
        );
    })
    .await;
}

#[tokio::test]
async fn a_corrupt_row_inside_the_window_still_refuses() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &bars_from(BASE_MS, 12)).await;
        plant_undecodable_row(key.symbol_id, BASE_MS + 4 * STEP_MS, &tape);
        let req = read_req(
            Timeframe::M1,
            Some(BASE_MS + 2 * STEP_MS),
            Some(BASE_MS + 8 * STEP_MS),
            None,
            FoldPolicy::ExtendToFullBuckets,
        );
        tape.read(&req)
            .expect_err("a corrupt row inside the served window still refuses");
    })
    .await;
}
