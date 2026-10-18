//! Windowed stamp planning serves what whole-tape planning served.
//!
//! The oracle in `common/oracle.rs` rebuilds whole-tape coverage plus plan
//! plus per-span fetches from public pieces, so every case below proves the
//! windowed read path serves identical bars, windows, provenance spans, and
//! refusals — not merely plausible ones. The per-span fetches run through
//! the tape's own bounded reads, so the comparison is windowed planning
//! against whole-tape planning on the same engine. (The two corrupt-row
//! cases died with the SQL fetch layer: an undecodable row cannot exist on
//! this engine — appends validate, and log tampering refuses at open.)

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::at_ms;
use crate::fold::FoldPolicy;
use crate::key::TapeKey;
use crate::range::TimeRange;
use crate::store::{CandleStore, ReadRequest};
use crate::tape::test_oracle::{assert_same_outcome, oracle_from_pieces};
use crate::tape::test_setup::{seeded, with_registry_caps, VENUE};

const STEP_MS: i64 = 60_000;
const BASE_MS: i64 = 1_767_225_900_000;

fn bars_from(first_ms: i64, count: i64) -> Vec<Bar> {
    (0..count)
        .map(|step| at_ms(first_ms + step * STEP_MS))
        .collect()
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
        range: TimeRange { start_ms, end_ms },
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
                ..read_req(
                    Timeframe::M1,
                    Some(BASE_MS),
                    Some(BASE_MS + STEP_MS),
                    None,
                    FoldPolicy::ExtendToFullBuckets,
                )
            },
        ),
        (
            "unbounded unlimited",
            read_req(
                Timeframe::M1,
                None,
                None,
                None,
                FoldPolicy::ExtendToFullBuckets,
            ),
        ),
    ]
}

fn check_all(tape: &MarketTape, key: TapeKey, tape_name: &str) {
    for (name, req) in requests() {
        let what = format!("{tape_name}: {name}");
        let lookup = tape
            .resolve(&req.symbol, &req.venue)
            .expect("oracle lookup reads")
            .map(|symbol_id| (symbol_id, Timeframe::M1));
        let expected = oracle_from_pieces(
            lookup,
            tape.coverage(TapeKey {
                symbol_id: key.symbol_id,
                timeframe: Timeframe::M1,
            })
            .expect("oracle coverage reads"),
            &|start, end| {
                tape.read(&ReadRequest {
                    symbol: "SPY".to_owned(),
                    venue: VENUE.to_owned(),
                    timeframe: Timeframe::M1,
                    range: TimeRange {
                        start_ms: start,
                        end_ms: end,
                    },
                    limit: None,
                    policy: FoldPolicy::ExtendToFullBuckets,
                })
                .map(|series| series.bars)
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
    with_registry_caps(async {
        let (tape, key) = seeded("SPY", &bars_from(BASE_MS, 12)).await;
        check_all(&tape, key, "contiguous");
    })
    .await;
}

#[tokio::test]
async fn windowed_reads_match_the_oracle_on_a_torn_tape() {
    with_registry_caps(async {
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
    with_registry_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        check_all(&tape, key, "empty");
    })
    .await;
}
