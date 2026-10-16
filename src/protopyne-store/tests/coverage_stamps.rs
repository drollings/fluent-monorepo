//! Stamp-derived coverage: the same runs from stamps alone.
//!
//! [`Coverage::from_stamps`](crate::coverage::Coverage::from_stamps) must
//! walk exactly the rule [`Coverage::from_bars`](crate::coverage::Coverage::from_bars)
//! walks, over stamps without materializing bars. This suite proves it
//! differentially: contiguous, torn, and empty tapes crossed with bounded,
//! open-start, and beyond-the-edges windows must agree exactly — first,
//! last, bars, and runs. (The stamp-fetch/extent half died with the SQL
//! fetch layer; windowed-stamp equivalence on the surviving engine is
//! proven by `read_windowed_oracle.rs`.)

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::at_ms;
use crate::coverage::Coverage;

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
