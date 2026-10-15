//! Edge policy on the one fold core: which buckets may emit.
//!
//! [`KeepPartial`](crate::fold::FoldEdges::KeepPartial) is the legacy
//! behavior — every bucket with at least one child emits, partial edges
//! included — and must stay bar-for-bar identical to
//! [`fold_bars`](crate::fold::fold_bars). `CompleteOnly` keeps a bucket only
//! when its children tile the bucket grid contiguously (first child on the
//! bucket start, uniform child spacing, children covering the whole span);
//! anything else is omitted, never emitted torn or partial. A lone child can
//! never evidence a full bucket, so single-child buckets are omitted too.

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::at_ms;
use crate::fold::{FoldEdges, fold_bars, fold_bars_edges};

/// One priced bar at an absolute millisecond stamp.
fn bar_at(ts_ms: i64, o: &str, h: &str, l: &str, c: &str, volume: u64) -> Bar {
    Bar::new()
        .ts(DateTime::from_timestamp_millis(ts_ms).expect("representable stamp"))
        .o(o.parse().expect("valid decimal"))
        .h(h.parse().expect("valid decimal"))
        .l(l.parse().expect("valid decimal"))
        .c(c.parse().expect("valid decimal"))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

/// Eight quarter-hour bars from an hour boundary with distinct legs, so edge
/// values stay attributable.
fn eight_quarter_hours(hour_ms: i64) -> Vec<Bar> {
    vec![
        bar_at(hour_ms, "100", "101", "99", "100.5", 10),
        bar_at(hour_ms + 900_000, "100.5", "102", "100", "101", 20),
        bar_at(hour_ms + 1_800_000, "101", "101.5", "98", "99", 30),
        bar_at(hour_ms + 2_700_000, "99", "100", "97.5", "98.5", 40),
        bar_at(hour_ms + 3_600_000, "200", "205", "199", "201", 50),
        bar_at(hour_ms + 4_500_000, "201", "203", "200", "202.5", 60),
        bar_at(hour_ms + 5_400_000, "202.5", "210", "202", "209", 70),
        bar_at(hour_ms + 6_300_000, "209", "209.5", "207", "208", 80),
    ]
}

#[test]
fn keep_partial_remains_the_default_policy() {
    assert_eq!(FoldEdges::default(), FoldEdges::KeepPartial);
}

/// Deterministic xorshift: randomized corpus without a dev-dependency.
fn next_rand(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

/// Minute-grid tapes with holes and off-boundary starts, laid over several
/// anchors (hour boundary, off-boundary minute, pre-epoch).
fn corpus_tapes() -> Vec<Vec<Bar>> {
    let anchors = [
        Timeframe::H1.bucket_start_ms(1_700_000_000_000),
        Timeframe::H1.bucket_start_ms(1_700_000_000_000) + 7 * 60_000,
        Timeframe::W1.bucket_start_ms(-1_000_000_000),
    ];
    let mut tapes = Vec::new();
    let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
    for anchor in anchors {
        for holes in [0_u64, 1, 7] {
            let mut bars = Vec::new();
            let mut step = 0_i64;
            for _ in 0..200 {
                step += 1;
                if holes != 0 && next_rand(&mut rng) % 11 < holes {
                    continue;
                }
                bars.push(at_ms(anchor + step * 60_000));
            }
            tapes.push(bars);
        }
    }
    tapes
}

#[test]
fn keep_partial_matches_fold_bars_bar_for_bar() {
    for bars in corpus_tapes() {
        for tf in [
            Timeframe::M5,
            Timeframe::M15,
            Timeframe::H1,
            Timeframe::H4,
            Timeframe::D1,
            Timeframe::W1,
        ] {
            let legacy = fold_bars(&bars, tf).expect("legacy fold builds");
            let edged =
                fold_bars_edges(&bars, tf, FoldEdges::KeepPartial).expect("edged fold builds");
            assert_eq!(edged, legacy, "{tf:?} KeepPartial stays byte-identical");
        }
    }
}

#[test]
fn complete_only_serves_whole_buckets_with_window_edge_values() {
    let hour = Timeframe::H1.bucket_start_ms(1_700_000_000_000);
    let folded =
        fold_bars_edges(&eight_quarter_hours(hour), Timeframe::H1, FoldEdges::CompleteOnly)
            .expect("aligned fold builds");
    assert_eq!(folded.len(), 2, "eight quarter-hours fill two whole hours");
    let first = &folded[0];
    assert_eq!(first.ts.timestamp_millis(), hour, "stamped at the bucket start");
    assert_eq!(first.o.to_string(), "100", "open is the first child's open");
    assert_eq!(first.c.to_string(), "98.5", "close is the last child's close");
    assert_eq!(first.h.to_string(), "102", "high is the bucket maximum");
    assert_eq!(first.l.to_string(), "97.5", "low is the bucket minimum");
    assert_eq!(first.volume, 100, "volume sums the bucket's children");
    assert!(first.adj.is_none(), "folds never invent an adjustment");
    let second = &folded[1];
    assert_eq!(second.ts.timestamp_millis(), hour + 3_600_000);
    assert_eq!(second.o.to_string(), "200");
    assert_eq!(second.c.to_string(), "208");
    assert_eq!(second.volume, 260);
}

#[test]
fn complete_only_omits_a_short_trailing_edge() {
    let hour = Timeframe::H1.bucket_start_ms(1_700_000_000_000);
    let mut bars = eight_quarter_hours(hour);
    bars.extend([
        bar_at(hour + 7_200_000, "300", "301", "299", "300.5", 10),
        bar_at(hour + 8_100_000, "300.5", "302", "300", "301", 20),
        bar_at(hour + 9_000_000, "301", "303", "300.5", "302", 30),
    ]);
    let folded =
        fold_bars_edges(&bars, Timeframe::H1, FoldEdges::CompleteOnly).expect("fold builds");
    assert_eq!(folded.len(), 2, "three children cannot evidence the third hour");
    assert_eq!(folded[1].ts.timestamp_millis(), hour + 3_600_000);
}

#[test]
fn complete_only_omits_a_short_leading_edge() {
    let hour = Timeframe::H1.bucket_start_ms(1_700_000_000_000);
    // Start one quarter-hour into the hour: the first bucket holds three
    // children and must not emit.
    let bars: Vec<Bar> = eight_quarter_hours(hour + 900_000)[..7].to_vec();
    let folded =
        fold_bars_edges(&bars, Timeframe::H1, FoldEdges::CompleteOnly).expect("fold builds");
    assert_eq!(folded.len(), 1, "only the whole second hour emits");
    assert_eq!(folded[0].ts.timestamp_millis(), hour + 3_600_000);
    assert_eq!(folded[0].o.to_string(), "99", "the kept bucket opens at its first child");
}

#[test]
fn complete_only_omits_torn_buckets_but_keeps_whole_neighbors() {
    let hour = Timeframe::H1.bucket_start_ms(1_700_000_000_000);
    let mut bars = eight_quarter_hours(hour);
    // Knock out the second quarter of the first hour: a torn bucket between
    // two whole ones.
    bars.remove(1);
    let folded =
        fold_bars_edges(&bars, Timeframe::H1, FoldEdges::CompleteOnly).expect("fold builds");
    assert_eq!(folded.len(), 1, "the torn hour is omitted, not emitted partial");
    assert_eq!(folded[0].ts.timestamp_millis(), hour + 3_600_000);
    assert_eq!(folded[0].volume, 260, "the whole neighbor keeps its own sum");
}

#[test]
fn complete_only_omits_lone_child_buckets() {
    let hour = Timeframe::H1.bucket_start_ms(1_700_000_000_000);
    assert!(
        fold_bars_edges(&[at_ms(hour)], Timeframe::H1, FoldEdges::CompleteOnly)
            .expect("fold builds")
            .is_empty(),
        "one child cannot evidence a whole hour"
    );
    assert!(
        fold_bars_edges(
            &[at_ms(hour), at_ms(hour + 900_000)],
            Timeframe::H1,
            FoldEdges::CompleteOnly
        )
        .expect("fold builds")
        .is_empty(),
        "two children still span a quarter of the hour"
    );
}

#[test]
fn complete_only_empty_is_empty() {
    assert!(
        fold_bars_edges(&[], Timeframe::H1, FoldEdges::CompleteOnly)
            .expect("empty fold builds")
            .is_empty()
    );
}

#[test]
fn complete_only_refuses_unsorted_input_like_the_legacy_fold() {
    let hour = Timeframe::H1.bucket_start_ms(1_700_000_000_000);
    let mut bars = eight_quarter_hours(hour);
    bars.swap(0, 7);
    let err = fold_bars_edges(&bars, Timeframe::H1, FoldEdges::CompleteOnly)
        .expect_err("shuffled input refuses");
    assert!(matches!(err, StoreError::InvalidBar(_)), "misordered input is a caller bug: {err:?}");
}
