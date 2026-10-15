//! Stamp-derived coverage: the same runs from stamps alone.
//!
//! [`Coverage::from_stamps`](crate::coverage::Coverage::from_stamps) must
//! walk exactly the rule [`Coverage::from_bars`](crate::coverage::Coverage::from_bars)
//! walks, over stamps without materializing bars. This suite proves it
//! differentially: contiguous, torn, and empty tapes crossed with bounded,
//! open-start, and beyond-the-edges windows must agree exactly — first,
//! last, bars, and runs. The database half proves the stamp fetch and the
//! extent query return exactly what the bar fetch returns, and that the
//! extent of an empty tape is `None`, never a zeroed triple.

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::at_ms;
use crate::coverage::Coverage;
use crate::sqlite_tape::test_setup::{seeded, with_memory_caps};

/// Deterministic xorshift: randomized corpus without a dev-dependency.
fn next_rand(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

const STEP_MS: i64 = 60_000;
const BASE_MS: i64 = 1_767_225_900_000;

fn bars_of(stamps: &[i64]) -> Vec<Bar> {
    stamps.iter().map(|stamp| at_ms(*stamp)).collect()
}

fn in_window(stamps: &[i64], start: Option<i64>, end: Option<i64>) -> Vec<i64> {
    stamps
        .iter()
        .copied()
        .filter(|stamp| {
            start.is_none_or(|lo| *stamp >= lo) && end.is_none_or(|hi| *stamp < hi)
        })
        .collect()
}

#[test]
fn stamps_and_bars_agree_on_contiguous_tapes() {
    let stamps: Vec<i64> = (0..200).map(|step| BASE_MS + step * STEP_MS).collect();
    let windows = [
        (None, None),
        (Some(BASE_MS + 10 * STEP_MS), Some(BASE_MS + 50 * STEP_MS)),
        (None, Some(BASE_MS + 30 * STEP_MS)),
        (Some(BASE_MS + 250 * STEP_MS), Some(BASE_MS + 300 * STEP_MS)),
    ];
    for (start, end) in windows {
        let windowed = in_window(&stamps, start, end);
        let bars = bars_of(&windowed);
        assert_eq!(
            Coverage::from_stamps(&windowed, Timeframe::M1),
            Coverage::from_bars(&bars, Timeframe::M1),
            "stamps and bars agree over window {start:?}..{end:?}"
        );
    }
}

#[test]
fn stamps_and_bars_agree_on_torn_tapes() {
    let mut rng = 0x9E3779B97F4A7C15_u64;
    for round in 0..8 {
        let mut stamps: Vec<i64> = Vec::new();
        for step in 0..120 {
            // Roughly one hole in six, plus a wider torn stretch per round.
            if next_rand(&mut rng) % 6 == 0 || (40 + round * 3..48 + round * 3).contains(&step) {
                continue;
            }
            stamps.push(BASE_MS + step * STEP_MS);
        }
        assert!(!stamps.is_empty(), "the torn tape holds rows");
        let windows = [
            (None, None),
            (
                Some(BASE_MS + 20 * STEP_MS),
                Some(BASE_MS + 90 * STEP_MS),
            ),
            (None, Some(BASE_MS + 60 * STEP_MS)),
            (Some(BASE_MS + 200 * STEP_MS), None),
        ];
        for (start, end) in windows {
            let windowed = in_window(&stamps, start, end);
            let bars = bars_of(&windowed);
            assert_eq!(
                Coverage::from_stamps(&windowed, Timeframe::M1),
                Coverage::from_bars(&bars, Timeframe::M1),
                "stamps and bars agree over a torn tape, window {start:?}..{end:?}"
            );
        }
    }
}

#[test]
fn stamps_of_nothing_is_no_coverage() {
    assert_eq!(Coverage::from_stamps(&[], Timeframe::M1), None);
}

#[test]
fn stamps_walk_pre_epoch_and_coarse_grids() {
    // A pre-epoch hour grid and a coarse daily grid: the run rule is grid
    // arithmetic, never calendar arithmetic.
    let hourly: Vec<i64> = (0..30).map(|step| -9 * 3_600_000 + step * 3_600_000).collect();
    let bars = bars_of(&hourly);
    assert_eq!(
        Coverage::from_stamps(&hourly, Timeframe::H1),
        Coverage::from_bars(&bars, Timeframe::H1),
    );
    let daily: Vec<i64> = (0..14).map(|step| BASE_MS + step * 86_400_000).collect();
    let bars = bars_of(&daily);
    assert_eq!(
        Coverage::from_stamps(&daily, Timeframe::D1),
        Coverage::from_bars(&bars, Timeframe::D1),
    );
}

#[tokio::test]
async fn stamp_fetch_matches_the_bar_fetch() {
    with_memory_caps(async {
        let mut rng = 0x123456789ABCDEF_u64;
        let mut stamps: Vec<i64> = Vec::new();
        for step in 0..120 {
            if next_rand(&mut rng) % 5 == 0 {
                continue;
            }
            stamps.push(BASE_MS + step * STEP_MS);
        }
        let (tape, key) = seeded("SPY", &bars_of(&stamps)).await;
        let conn = common_core::sync::lock(tape.catalog().connection());
        let windows = [
            (None, None),
            (
                Some(BASE_MS + 10 * STEP_MS),
                Some(BASE_MS + 100 * STEP_MS),
            ),
            (None, Some(BASE_MS + 40 * STEP_MS)),
            (Some(BASE_MS + 500 * STEP_MS), Some(BASE_MS + 600 * STEP_MS)),
        ];
        for (start, end) in windows {
            let fetched = crate::sqlite_tape::fetch_stamps(&conn, key.symbol_id, start, end)
                .expect("stamp fetch");
            assert_eq!(
                fetched,
                in_window(&stamps, start, end),
                "the stamp fetch returns exactly the window's stamps, ascending"
            );
            let fetched_bars =
                crate::sqlite_tape::fetch_bars(&conn, key.symbol_id, start, end)
                    .expect("bar fetch");
            let ordered = crate::sqlite_tape::order_and_validate(fetched_bars)
                .expect("stored rows validate");
            assert_eq!(
                Coverage::from_stamps(&fetched, Timeframe::M1),
                Coverage::from_bars(&ordered, Timeframe::M1),
                "stamp-derived and bar-derived coverage agree over {start:?}..{end:?}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn stamp_fetch_sorts_what_the_table_did_not() {
    with_memory_caps(async {
        let stamps: Vec<i64> = (0..24).map(|step| BASE_MS + step * STEP_MS).collect();
        let mut shuffled = bars_of(&stamps);
        // Reverse pairs: written out of order, still the same set.
        for pair in shuffled.chunks_exact_mut(2) {
            pair.swap(0, 1);
        }
        let (tape, key) = seeded("SPY", &shuffled).await;
        let conn = common_core::sync::lock(tape.catalog().connection());
        let fetched = crate::sqlite_tape::fetch_stamps(&conn, key.symbol_id, None, None)
            .expect("stamp fetch");
        assert_eq!(fetched, stamps, "stamps ascend regardless of write order");
    })
    .await;
}

#[tokio::test]
async fn extent_names_bounds_without_counting_rows() {
    with_memory_caps(async {
        let (tape, key) = seeded(
            "SPY",
            &bars_of(&(0..64).map(|step| BASE_MS + step * STEP_MS).collect::<Vec<_>>()),
        )
        .await;
        let conn = common_core::sync::lock(tape.catalog().connection());
        let extent = crate::sqlite_tape::tape_extent(&conn, key.symbol_id).expect("extent");
        let extent = extent.expect("a seeded tape has an extent");
        assert_eq!(extent.first_ms, BASE_MS);
        assert_eq!(extent.last_ms, BASE_MS + 63 * STEP_MS);
    })
    .await;
}

#[tokio::test]
async fn extent_of_an_empty_tape_is_none() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        let conn = common_core::sync::lock(tape.catalog().connection());
        let extent = crate::sqlite_tape::tape_extent(&conn, key.symbol_id).expect("extent");
        assert_eq!(extent, None, "no rows is no extent, never a zeroed triple");
    })
    .await;
}
