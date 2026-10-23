//! History-source adapter tests: the confirmed-bar gate, backfill from
//! either load arm, and whole-suffix rebuild on correction.
//!
//! The server owns parsing and fallback (`load_history` over either
//! `HistorySource` arm); this suite proves the adapter is indifferent to
//! which arm produced its rows, admits confirmed bars only, and rebuilds a
//! corrected suffix into exactly what a from-scratch build yields.
//! `#[allow(clippy::float_cmp)]` covers the exact decimal literals.
#![allow(clippy::float_cmp)]

use super::*;
use chrono::DateTime;
use fluent_wvr::validate::Validate;
use protopyne_types::{Bar, Timeframe};
use rust_decimal::Decimal;
use std::path::PathBuf;

/// Frozen agreement tolerance shared with the Pine goldens.
const EPS: f64 = 1e-9;

/// Minute-aligned base for synthetic histories.
const T0: i64 = 1_699_999_980_000;
const MIN: i64 = 60_000;

fn assert_close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() <= EPS * want.abs().max(1.0),
        "{what}: got {got}, want {want}"
    );
}

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

/// One synthetic closed bar: bucket-open stamp, distinct OHLCV.
fn bar_at(open_ms: i64, open: i64, close: i64, volume: u64) -> Bar {
    Bar::new()
        .ts(DateTime::from_timestamp(open_ms / 1000, 0).expect("in range"))
        .o(Decimal::new(open, 2))
        .h(Decimal::new(open.max(close) + 10, 2))
        .l(Decimal::new(open.min(close) - 10, 2))
        .c(Decimal::new(close, 2))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

/// Ten aligned minute bars: opens `T0 + i*MIN`, volumes `100*(i+1)`.
fn ten_bars() -> Vec<Bar> {
    (0..10)
        .map(|i| {
            bar_at(
                T0 + i * MIN,
                10_000 + i * 5,
                10_000 + i * 5 + 2,
                100 * (i as u64 + 1),
            )
        })
        .collect()
}

/// Parse the first `count` rows of a history fixture into `Bar` rows (the
/// CSV arm's output shape: string-scale decimals, epoch-second stamps).
fn csv_arm(name: &str, count: usize) -> Vec<Bar> {
    let content =
        std::fs::read_to_string(data_root().join("historical").join(name)).expect("history loads");
    content
        .lines()
        .skip(1)
        .take(count)
        .map(|line| {
            let mut cells = line.split(',');
            let secs: i64 = cells.next().expect("time").parse().expect("int time");
            let dec = |cell: Option<&str>| cell.expect("cell").parse::<Decimal>().expect("decimal");
            Bar::new()
                .ts(DateTime::from_timestamp(secs, 0).expect("in range"))
                .o(dec(cells.next()))
                .h(dec(cells.next()))
                .l(dec(cells.next()))
                .c(dec(cells.next()))
                .volume(cells.next().expect("volume").parse().expect("u64 volume"))
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// Feed confirmed rows into a fresh minute ring through the LTF-row shape.
fn ring_of(rows: &[ClosedBar]) -> BarRing {
    let config = BarsConfig::new(MIN, MIN, 10 * MIN, 86_400_000);
    let mut ring = BarRing::new(&config).expect("config valid");
    let ltf: Vec<LtfRow> = rows
        .iter()
        .map(|bar| LtfRow {
            ts_ms: bar.ts_ms,
            o: bar.o,
            h: bar.h,
            l: bar.l,
            c: bar.c,
            volume: bar.volume,
        })
        .collect();
    let total: f64 = rows.iter().map(|bar| bar.volume).sum();
    ring.reconcile(&ltf, total, rows.first().map_or(0.0, |bar| bar.o));
    ring
}

#[test]
fn tail_never_finalizes_control() {
    let history = BarsHistory::from_bars(&ten_bars(), Timeframe::M1).expect("aligned loads");
    // `now` sits 1ms inside the last bar: nine confirmed, one forming.
    let now = T0 + 9 * MIN + 1;
    let gated: Vec<ClosedBar> = history
        .bars()
        .iter()
        .copied()
        .filter(|bar| history.is_confirmed(bar.ts_ms, now, Lookahead::Off))
        .collect();
    assert_eq!(gated.len(), 9, "forming tail never passes the gate");
    let ring = ring_of(&gated);
    assert_eq!(ring.len(), 9, "nine finalized candles");
    assert_close(
        ring.total_volume(),
        100.0 * 45.0,
        "prefix volume sums exactly",
    );
    // Tail-only input yields zero finalized candles.
    let tail_only =
        BarsHistory::from_bars(&ten_bars()[9..], Timeframe::M1).expect("single bar loads");
    let tail_gated: Vec<ClosedBar> = tail_only
        .bars()
        .iter()
        .copied()
        .filter(|bar| tail_only.is_confirmed(bar.ts_ms, now, Lookahead::Off))
        .collect();
    assert!(tail_gated.is_empty(), "unconfirmed tail admits nothing");
    let tail_ring = ring_of(&tail_gated);
    assert!(
        tail_ring.is_empty(),
        "zero finalized candles from tail only"
    );
}

#[test]
fn confirmed_prefix_finalizes_exactly() {
    let history = BarsHistory::from_bars(&ten_bars(), Timeframe::M1).expect("aligned loads");
    // Bar closes exactly at `now` count as confirmed (the fence is `<=`).
    let now = T0 + 9 * MIN;
    assert!(
        history.is_confirmed(T0 + 8 * MIN, now, Lookahead::Off),
        "closed bar passes"
    );
    assert!(
        !history.is_confirmed(T0 + 9 * MIN, now, Lookahead::Off),
        "bar closing at now waits"
    );
    let later = T0 + 10 * MIN;
    assert!(
        history.is_confirmed(T0 + 9 * MIN, later, Lookahead::Off),
        "closed bar passes later"
    );
    let gated: Vec<ClosedBar> = history
        .bars()
        .iter()
        .copied()
        .filter(|bar| history.is_confirmed(bar.ts_ms, later, Lookahead::Off))
        .collect();
    assert_eq!(gated.len(), 10, "whole history confirmed past its close");
    let ring = ring_of(&gated);
    assert_close(
        ring.total_volume(),
        100.0 * 55.0,
        "full volume sums exactly",
    );
    ring.validate().expect("ring stays coherent");
}

#[test]
fn csv_vs_dynamic_equality() {
    // CSV arm: fixture rows parsed from strings (file scales as shipped).
    let csv_bars = csv_arm("SPY_1M.csv", 8);
    assert_eq!(csv_bars.len(), 8, "fixture arm yields eight rows");
    // Dynamic arm: identical values rebuilt through `Decimal` constructors
    // at deliberately different scales — the adapter must not see the
    // difference between string-parsed and programmatically-built decimals.
    let dynamic_bars: Vec<Bar> = csv_bars
        .iter()
        .map(|bar| {
            let mut o = bar.o;
            let mut h = bar.h;
            let mut l = bar.l;
            let mut c = bar.c;
            o.rescale(8);
            h.rescale(8);
            l.rescale(8);
            c.rescale(8);
            Bar::new()
                .ts(bar.ts)
                .o(o)
                .h(h)
                .l(l)
                .c(c)
                .volume(bar.volume)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let from_csv = BarsHistory::from_bars(&csv_bars, Timeframe::M1).expect("csv arm loads");
    let from_dynamic =
        BarsHistory::from_bars(&dynamic_bars, Timeframe::M1).expect("dynamic arm loads");
    assert_eq!(from_csv.bars(), from_dynamic.bars(), "row-for-row equality");
    assert_eq!(from_csv.gaps(), from_dynamic.gaps(), "gap reports agree");
    // Stamp convention pinned: history stamps read as bucket opens, the
    // exchange-CSV convention the projection goldens prove.
    let first_secs = csv_bars[0].ts.timestamp();
    assert_eq!(
        from_csv.bars()[0].ts_ms,
        first_secs * 1000,
        "stamps map identically"
    );
    for bar in from_csv.bars() {
        assert_eq!(bar.ts_ms % MIN, 0, "fixture stamps sit on the grid");
    }
    assert_eq!(
        from_csv.tf_ms(),
        Timeframe::M1.duration_ms(),
        "span comes from the single table"
    );
}

#[test]
fn correction_equals_rebuild() {
    let original = ten_bars();
    let mut corrected = original.clone();
    // Vendor revision: bars 6..10 print new closes and volumes.
    for bar in corrected.iter_mut().skip(6) {
        let bumped = format!("{}", bar.c + Decimal::new(150, 2));
        *bar = Bar::new()
            .ts(bar.ts)
            .o(bar.o)
            .h(bar.h + Decimal::new(150, 2))
            .l(bar.l)
            .c(bumped.parse().expect("decimal"))
            .volume(bar.volume + 25)
            .maybe_adj(None)
            .build();
    }
    let scratch = BarsHistory::from_bars(&corrected, Timeframe::M1).expect("corrected loads");
    let mut incremental = BarsHistory::from_bars(&original, Timeframe::M1).expect("original loads");
    let retained = incremental
        .rebuild_suffix(T0 + 6 * MIN, &corrected[6..])
        .expect("suffix rebuilds");
    assert_eq!(retained, 6, "six-bar prefix untouched");
    assert_eq!(
        incremental.bars(),
        scratch.bars(),
        "rebuild equals from-scratch"
    );
    assert_eq!(
        incremental.gaps(),
        scratch.gaps(),
        "gap reports agree after rebuild"
    );
    // Misaligned, empty, and out-of-range corrections refuse fail-closed.
    assert!(
        incremental
            .rebuild_suffix(T0 + 6 * MIN, &corrected[7..])
            .is_err(),
        "start must match"
    );
    assert!(
        incremental.rebuild_suffix(T0 + 6 * MIN, &[]).is_err(),
        "empty suffix refuses"
    );
    assert!(
        incremental
            .rebuild_suffix(T0 + 40 * MIN, &corrected[6..])
            .is_err(),
        "past-the-end refuses"
    );
    assert!(
        incremental
            .rebuild_suffix(T0 + 5 * MIN, &corrected[6..])
            .is_err(),
        "overlap refuses"
    );
}

#[test]
fn lookahead_non_leakage_probe() {
    let history = BarsHistory::from_bars(&ten_bars(), Timeframe::M1).expect("aligned loads");
    let now = T0 + 9 * MIN + 1;
    // Lookahead-on poisons the whole gate: even long-closed bars refuse,
    // so no future-bar data can leak through an enabled projection path.
    let leaked: Vec<_> = history
        .bars()
        .iter()
        .filter(|bar| is_confirmed_bar(bar.ts_ms, MIN, now, Lookahead::On))
        .collect();
    assert!(leaked.is_empty(), "lookahead-on confirms nothing");
    // Lookahead-off admits the formed prefix and drops the forming tail.
    let admitted: Vec<ClosedBar> = history
        .bars()
        .iter()
        .copied()
        .filter(|bar| is_confirmed_bar(bar.ts_ms, MIN, now, Lookahead::Off))
        .collect();
    assert_eq!(admitted.len(), 9, "prefix admitted, tail held");
    let ring = ring_of(&admitted);
    assert_close(
        ring.total_volume(),
        100.0 * 45.0,
        "future volume never finalizes",
    );
    assert!(
        ring.iter().all(|candle| candle.ts_ms < now),
        "no future stamps aboard"
    );
}

#[test]
fn from_bars_refuses_bad_input() {
    assert!(
        BarsHistory::from_bars(&[], Timeframe::M1).is_err(),
        "empty refuses like the loader"
    );
    let mut unordered = ten_bars();
    unordered.swap(3, 4);
    assert!(
        BarsHistory::from_bars(&unordered, Timeframe::M1).is_err(),
        "reorder refuses"
    );
    let mut inverted = ten_bars();
    inverted[5] = Bar::new()
        .ts(inverted[5].ts)
        .o(Decimal::new(10_000, 2))
        .h(Decimal::new(9_000, 2))
        .l(Decimal::new(9_500, 2))
        .c(Decimal::new(9_600, 2))
        .volume(100)
        .maybe_adj(None)
        .build();
    assert!(
        BarsHistory::from_bars(&inverted, Timeframe::M1).is_err(),
        "inverted OHLC refuses"
    );
    // Share counts past 2^53 lose integer precision in `f64`: refused
    // rather than rounded.
    let mut coarse = ten_bars();
    coarse[2] = bar_at(T0 + 2 * MIN, 10_000, 10_002, (1 << 53) + 1);
    assert!(
        BarsHistory::from_bars(&coarse, Timeframe::M1).is_err(),
        "imprecise volume refuses"
    );
    let mut exact = ten_bars();
    exact[2] = bar_at(T0 + 2 * MIN, 10_000, 10_002, 1 << 53);
    assert!(
        BarsHistory::from_bars(&exact, Timeframe::M1).is_ok(),
        "2^53 itself converts exactly"
    );
}

#[test]
fn gaps_report_sparse_holes_without_confirming_them() {
    let mut holed = ten_bars();
    holed.drain(4..7);
    let history = BarsHistory::from_bars(&holed, Timeframe::M1).expect("gapped loads");
    let gaps = history.gaps();
    assert_eq!(gaps.len(), 1, "one discontinuity");
    assert_eq!(gaps[0].at_index, 4, "hole opens at index four");
    assert_eq!(gaps[0].missing_bars, 3, "three buckets missing");
    // Gaps change no verdict: both sides confirm on their own span once
    // formed, and the hole itself never materializes.
    let now = T0 + 10 * MIN;
    assert!(
        history.is_confirmed(T0 + 3 * MIN, now, Lookahead::Off),
        "left edge confirms"
    );
    assert!(
        history.is_confirmed(T0 + 7 * MIN, now, Lookahead::Off),
        "right edge confirms"
    );
    let aligned = BarsHistory::from_bars(&ten_bars(), Timeframe::M1).expect("aligned loads");
    assert!(aligned.gaps().is_empty(), "aligned history reports no gaps");
}

#[test]
fn gate_fence_arithmetic() {
    // The confirmation fence is `open + span <= now`: a close exactly at
    // `now` counts, one millisecond early does not.
    let open = T0;
    assert!(
        is_confirmed_bar(open, MIN, open + MIN, Lookahead::Off),
        "close at now confirms"
    );
    assert!(
        !is_confirmed_bar(open, MIN, open + MIN - 1, Lookahead::Off),
        "open span waits"
    );
    assert!(
        !is_confirmed_bar(open, 0, open + MIN, Lookahead::Off),
        "non-positive span refuses"
    );
    assert!(
        !is_confirmed_bar(open, -MIN, open + MIN, Lookahead::Off),
        "negative span refuses"
    );
    assert!(
        !is_confirmed_bar(open, MIN, open + MIN, Lookahead::On),
        "lookahead refuses"
    );
}
