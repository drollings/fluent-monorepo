//! Native bar-path goldens: hand-computed expectations with tight
//! tolerances, never measured quantities.
//! `#[allow(clippy::float_cmp)]` covers the exact decimal literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::indicator::BarTick;
use protopyne_types::Validate;
use std::path::PathBuf;

/// Frozen agreement tolerance shared with the Pine goldens.
const EPS: f64 = 1e-9;

/// Minute-aligned base: `T0 / 60000 = 28333333` exactly.
const T0: i64 = 1_699_999_980_000;

fn assert_close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() <= EPS * want.abs().max(1.0),
        "{what}: got {got}, want {want}"
    );
}

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn test_config() -> BarsConfig {
    BarsConfig::new(60_000, 60_000, 300_000, 86_400_000)
}

/// The ten-tick clean tape: per-tick volumes with cumulative legs. Ticks 1-2
/// seed the tracker; every later tick accumulates.
fn clean_tape() -> Vec<(i64, f64, f64)> {
    let per_tick: Vec<(i64, f64, f64)> = vec![
        (5_000, 100.0, 10.0),
        (10_000, 100.2, 10.0),
        (20_000, 101.0, 200.0),
        (30_000, 102.0, 200.0),
        (70_000, 101.5, 150.0),
        (80_000, 101.0, 150.0),
        (190_000, 99.0, 100.0),
        (200_000, 99.5, 100.0),
        (250_000, 100.0, 50.0),
        (290_000, 100.5, 30.0),
    ];
    let mut cum = 0.0;
    per_tick
        .into_iter()
        .map(|(offset, price, vol)| {
            cum += vol;
            (T0 + offset, price, cum)
        })
        .collect()
}

/// Replay the clean tape through a fresh ring: open on the first tick (the
/// `isnew` branch seeds tracking without accumulating), fold every later
/// tick, and evaluate the LTF boundary at each tick stamp (the Pine `fetch`
/// order). Legs track the bar-so-far extremes like the engine harness does.
fn replay_clean(ring: &mut BarRing) -> Vec<LtfOutcome> {
    ring.on_bar_open(100.0, 100.0, 100.0, 100.0, T0);
    let mut outcomes = Vec::new();
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let mut first = true;
    for (stamp, price, cumvol) in clean_tape() {
        high = high.max(price);
        low = low.min(price);
        if first {
            // isnew update: opens the bar, accumulates nothing.
            first = false;
        } else {
            let legs = BarTick { open: 100.0, high, low, close: price, volume: cumvol };
            ring.ingest_tick(legs, stamp);
        }
        outcomes.push(ring.confirm_ltf(stamp, price, None));
    }
    outcomes
}

#[test]
fn helper_edge_table() {
    assert_eq!(drift_pct(110.0, 100.0), Some(10.0));
    assert_eq!(drift_pct(0.0, 100.0), Some(100.0));
    assert_eq!(drift_pct(100.0, 0.0), Some(0.0));
    assert_eq!(drift_pct(100.0, -5.0), Some(0.0));
    assert_eq!(drift_pct(f64::NAN, 100.0), None);
    assert_eq!(drift_pct(100.0, f64::NAN), None);
    assert_eq!(estimate_buy_volume(100.0, 5.0), 100.0);
    assert_eq!(estimate_buy_volume(100.0, -5.0), 0.0);
    assert_eq!(estimate_buy_volume(100.0, 0.0), 50.0);
    assert_eq!(estimate_buy_volume(100.0, f64::NAN), 50.0);
    assert!(is_valid_volume(0.5));
    assert!(!is_valid_volume(0.0001));
    assert!(is_valid_volume(0.00011));
    assert!(!is_valid_volume(0.0));
    assert!(!is_valid_volume(-1.0));
    assert!(!is_valid_volume(f64::NAN));
    assert!(validate_ohlcv(100.0, 101.0, 99.0, 100.5, 200.0));
    assert!(!validate_ohlcv(100.0, 101.0, 99.0, 100.5, 0.0));
    assert!(!validate_ohlcv(f64::NAN, 101.0, 99.0, 100.5, 200.0));
    // Degenerate range falls back to the neutral split.
    assert_eq!(estimate_delta_volume(100.0, 100.0, 100.0, 100.0, 200.0), 100.0);
    assert_eq!(fast_tanh(0.0), 0.0);
    assert_close(fast_tanh(1.0), 0.5, "fast tanh at one");
}

#[test]
fn config_validates_and_reports() {
    let good = test_config();
    assert_eq!(good.max_candles(), 10, "ratio 5 plus padding");
    good.validate().expect("sane config validates");
    for bad in [
        BarsConfig::new(0, 60_000, 300_000, 86_400_000),
        BarsConfig::new(60_000, -1, 300_000, 86_400_000),
        BarsConfig::new(60_000, 60_000, 0, 86_400_000),
        BarsConfig::new(60_000, 60_000, 300_000, 0),
    ] {
        assert!(bad.validate().is_err(), "non-positive span must refuse");
        assert!(
            BarRing::new(&bad).is_err(),
            "ring construction refuses bad config"
        );
    }
    let mut drift = test_config();
    drift.drift_threshold = f64::NAN;
    assert!(drift.validate().is_err(), "absent threshold must refuse");
}

#[test]
fn ring_identity_get_chains_and_bounds() {
    let config = test_config();
    let mut ring = BarRing::new(&config).expect("ring builds");
    assert!(ring.is_empty());
    assert_eq!(ring.len(), 0);
    assert_eq!(ring.get(0), None, "empty ring reads absent");
    ring.on_bar_open(100.0, 101.0, 99.0, 100.5, T0);
    let rows = [
        LtfRow { ts_ms: T0, o: 100.0, h: 101.0, l: 99.0, c: 100.5, volume: 400.0 },
        LtfRow { ts_ms: T0 + 60_000, o: 100.5, h: 102.0, l: 100.0, c: 101.5, volume: 300.0 },
    ];
    ring.reconcile(&rows, 700.0, 100.0);
    assert_eq!(ring.len(), 2);
    assert!(ring.has_volume_waiting());
    let first = ring.get(0).expect("oldest reads");
    assert_eq!(first.o, 100.0, "first open chains from the bar open");
    assert_eq!(first.c, 100.5);
    assert_close(first.buy_volume, 400.0 * 0.7, "row one buys at 0.70");
    let second = ring.get(1).expect("newest reads");
    assert_eq!(second.o, 100.5, "later opens chain from the prior close");
    assert_eq!(ring.get(2), None, "past-the-end reads absent");
    assert_close(ring.total_volume(), 700.0, "totals track");
    ring.validate().expect("populated ring validates");
}

#[test]
fn ring_evicts_oldest_beyond_capacity() {
    // Degenerate grid: one candle per bar plus padding (capacity 6).
    let config = BarsConfig::new(60_000, 60_000, 60_000, 86_400_000);
    assert_eq!(config.max_candles(), 6);
    let mut ring = BarRing::new(&config).expect("ring builds");
    let rows: Vec<LtfRow> = (0..8)
        .map(|i| LtfRow {
            ts_ms: T0 + i * 60_000,
            o: 100.0,
            h: 101.0,
            l: 99.0,
            c: 100.0,
            volume: 100.0,
        })
        .collect();
    ring.reconcile(&rows, 800.0, 100.0);
    assert_eq!(ring.len(), 6, "capacity bounds memory");
    assert_close(ring.total_volume(), 600.0, "evicted volumes leave the totals");
    assert_eq!(
        ring.get(0).expect("oldest").ts_ms,
        T0 + 2 * 60_000,
        "oldest retained follows eviction"
    );
}

#[test]
fn ring_windows_iterators_and_barssince() {
    let mut ring = BarRing::new(&test_config()).expect("ring builds");
    ring.on_bar_open(100.0, 100.0, 100.0, 100.0, T0);
    let rows: Vec<LtfRow> = (0..5)
        .map(|i| LtfRow {
            ts_ms: T0 + i * 60_000,
            o: 100.0,
            h: 101.0,
            l: 99.0,
            c: 100.0 + i as f64,
            volume: 100.0 * (i as f64 + 1.0),
        })
        .collect();
    ring.reconcile(&rows, 1500.0, 100.0);
    let forward: Vec<i64> = ring.iter().map(|c| c.ts_ms).collect();
    assert_eq!(forward.len(), 5);
    assert!(forward.windows(2).all(|w| w[0] < w[1]), "oldest to newest");
    let backward: Vec<i64> = ring.iter_rev().map(|c| c.ts_ms).collect();
    assert_eq!(backward, {
        let mut rev = forward.clone();
        rev.reverse();
        rev
    });
    assert_eq!(ring.window(2).len(), 2, "tail window");
    assert_eq!(ring.window(0).len(), 0);
    assert_eq!(ring.window(99).len(), 5, "oversized window saturates");
    assert_eq!(
        ring.bars_since(|c| c.volume > 350.0 && c.volume < 450.0),
        Some(1),
        "one bar since the 400-unit candle"
    );
    assert_eq!(
        ring.bars_since(|c| c.volume > 10_000.0),
        None,
        "never-true reads absent"
    );
    assert_eq!(
        ring.bars_since(|_| true),
        Some(0),
        "true on the newest bar reads zero"
    );
}

#[test]
fn clean_tape_replays_pine_goldens() {
    let mut ring = BarRing::new(&test_config()).expect("ring builds");
    let outcomes = replay_clean(&mut ring);
    assert_eq!(outcomes.len(), 10);
    for (at, crossed) in [(4, 1), (6, 2), (8, 1)] {
        assert_eq!(
            outcomes[at].boundaries_crossed, crossed,
            "crossing at tick {at}"
        );
        assert!(outcomes[at].ready_boundary, "pulse on tick {at}");
    }
    assert!(!outcomes[9].ready_boundary, "quiet on the confirming tick");
    assert_eq!(ring.boundary_count(), 4);
    assert_eq!(ring.gap_count(), 1, "bucket 2 had no trades");
    assert_eq!(ring.tick_count(), 8, "ticks 3-10 accumulate");
    assert_close(ring.tick_volume_total(), 980.0, "first two ticks seed");
    let confirmed = ConfirmedBar {
        bar_open_ms: T0,
        o: 100.0,
        h: 102.0,
        l: 99.0,
        c: 100.5,
        volume: 1000.0,
    };
    let report = ring.confirm(&confirmed);
    assert!(report.confirmed);
    assert_eq!(ring.len(), 5, "four distributed candles plus remainder");
    assert_close(ring.total_volume(), 1000.0, "remainder tops up");
    assert_close(ring.total_buy_volume(), 580.0 + 35.0 / 3.0, "buy tape");
    assert_close(report.tick_accuracy, 0.98, "accuracy");
    assert_close(report.volume_drift_pct, 2.0, "drift");
    assert_close(ring.last_bar_drift(), 2.0, "recorded drift");
    assert!(report.healthy, "2% drift is under the 4% threshold");
    assert_eq!(ring.pull_count(), 0, "reconciled bars trigger no pull");
    assert!(!report.pull_needed);
    let first = ring.get(0).expect("first candle");
    assert_eq!(first.o, 100.0, "chains from the bar open");
    assert_eq!((first.h, first.l, first.c), (102.0, 100.0, 101.5));
    assert_eq!(first.buy_volume, 400.0);
    assert_eq!(first.sell_volume(), 150.0);
    assert_close(ring.volume_imbalance(), 275.0 / 3.0, "buy pressure positive");
}

#[test]
fn rollback_resets_accumulator() {
    let mut ring = BarRing::new(&test_config()).expect("ring builds");
    // isnew update opens the bar and seeds nothing; ticks fold from t2.
    ring.on_bar_open(100.0, 101.0, 100.0, 100.0, T0);
    ring.confirm_ltf(T0 + 5_000, 100.0, None);
    let tape = [(10_000, 101.0, 2.0), (20_000, 100.5, 1.0), (30_000, 100.5, 100.0)];
    let mut kinds = Vec::new();
    for (offset, price, cumvol) in tape {
        let legs = BarTick { open: 100.0, high: 101.0, low: 100.0, close: price, volume: cumvol };
        kinds.push(ring.ingest_tick(legs, T0 + offset));
        ring.confirm_ltf(T0 + offset, price, None);
    }
    assert!(matches!(kinds[0], TickIngest::Seeded));
    assert!(matches!(kinds[1], TickIngest::RolledBack));
    assert_eq!(ring.rollback_count(), 1);
    assert_close(ring.tick_volume_total(), 99.0, "post-reset delta only");
    let report = ring.confirm(&ConfirmedBar {
        bar_open_ms: T0,
        o: 100.0,
        h: 101.0,
        l: 100.0,
        c: 100.5,
        volume: 100.0,
    });
    assert!(report.confirmed);
    assert_eq!(ring.len(), 2, "finalized candle plus remainder");
    assert_close(ring.total_volume(), 100.0, "bar total");
    assert_close(report.tick_accuracy, 0.99, "accuracy");
    assert!(report.healthy);
    let first = ring.get(0).expect("first candle");
    assert_eq!((first.h, first.c), (101.0, 100.5));
    assert_eq!(first.buy_volume, 49.5, "flat delta splits neutral");
}

#[test]
fn pull_reconcile_rebuilds_from_closed_rows() {
    let mut ring = BarRing::new(&test_config()).expect("ring builds");
    ring.on_bar_open(100.0, 100.0, 100.0, 100.0, T0);
    let rows = [
        (T0 - 2_400_000, 100.0, 101.0, 99.0, 100.5),
        (T0 - 1_800_000, 100.5, 102.0, 100.0, 101.5),
        (T0 - 1_200_000, 101.5, 103.0, 101.0, 102.5),
        (T0 - 600_000, 102.5, 104.0, 102.0, 103.5),
        (T0, 103.5, 105.0, 103.0, 104.5),
    ]
    .map(|(ts_ms, o, h, l, c)| LtfRow { ts_ms, o, h, l, c, volume: 200.0 });
    let outcome = ring.reconcile(&rows, 1000.0, 100.0);
    assert_eq!(outcome.rebuilt, 5);
    assert!(outcome.remainder.is_none(), "exact reference adds nothing");
    assert_eq!(ring.len(), 5);
    assert_close(ring.total_volume(), 1000.0, "rows sum to the reference");
    assert_close(ring.total_buy_volume(), 800.0, "140 plus four 165 splits");
    assert_eq!(ring.pull_count(), 1, "rebuild counts");
    let first = ring.get(0).expect("first row");
    assert_eq!((first.o, first.h, first.c), (100.0, 101.0, 100.5));
    assert_eq!(first.buy_volume, 140.0);
    assert_eq!(first.sell_volume(), 60.0);
}

#[test]
fn confirm_fence_is_idempotent() {
    let mut ring = BarRing::new(&test_config()).expect("ring builds");
    replay_clean(&mut ring);
    let confirmed = ConfirmedBar {
        bar_open_ms: T0,
        o: 100.0,
        h: 102.0,
        l: 99.0,
        c: 100.5,
        volume: 1000.0,
    };
    let first = ring.confirm(&confirmed);
    assert!(first.confirmed);
    let total = ring.total_volume();
    let second = ring.confirm(&confirmed);
    assert!(!second.confirmed, "same bar never confirms twice");
    assert_eq!(ring.total_volume(), total, "second confirm changes nothing");
    assert_eq!(ring.len(), 5);
}

#[test]
fn spike_and_alpha_follow_pine_guards() {
    let mut ring = BarRing::new(&test_config()).expect("ring builds");
    assert!(!ring.is_spike(100.0), "empty ring never spikes");
    replay_clean(&mut ring);
    ring.confirm(&ConfirmedBar {
        bar_open_ms: T0,
        o: 100.0,
        h: 102.0,
        l: 99.0,
        c: 100.5,
        volume: 1000.0,
    });
    assert!(!ring.is_spike(0.0), "absent reference never spikes");
    assert!(!ring.is_spike(-5.0), "negative reference never spikes");
    // Newest candle holds 20 units over a 60s/86400s slice of any reference
    // far above it: no spike. A tiny reference makes the same candle spike.
    assert!(!ring.is_spike(1_000_000.0));
    assert!(ring.is_spike(10.0), "20 units dwarf a 10-unit reference slice");
    let alpha = ring.alpha(1000.0, 900.0, 1.0, 1.0, 2.0, 0.5);
    assert!(alpha.is_finite(), "alpha stays finite");
    assert!((0.01..=0.90).contains(&alpha), "alpha stays in range");
    assert_eq!(
        BarRing::new(&test_config()).expect("ring").alpha(0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
        0.08,
        "empty guards read the default"
    );
}

#[test]
fn tick_resample_folds_and_refuses() {
    // Raw prints accumulate whole: the first print seeds direction-neutral,
    // every print's volume folds in, and only closed buckets emit.
    let ticks: Vec<RawTick> = [(0, 100.0, 10.0), (20_000, 101.0, 200.0), (70_000, 101.5, 150.0), (130_000, 102.0, 50.0)]
        .map(|(offset, price, volume)| RawTick { ts_ms: T0 + offset, price, volume })
        .to_vec();
    let parts = tick_resample(&ticks, 60_000, false, Lookahead::Off).expect("two buckets close");
    assert_eq!(parts.len(), 2, "trailing partial bucket stays unconfirmed");
    assert_eq!(parts[0].ts_ms, T0);
    assert_eq!((parts[0].o, parts[0].h, parts[0].l, parts[0].c), (100.0, 101.0, 100.0, 101.0));
    assert_eq!(parts[0].volume, 210.0);
    assert_eq!(parts[1].ts_ms, T0 + 60_000);
    assert_eq!(parts[1].volume, 150.0);
    // Single-bucket tape: nothing confirmed yet.
    let err = tick_resample(&ticks[..2], 60_000, false, Lookahead::Off)
        .expect_err("refused before first closed bar");
    assert!(err.to_string().contains("closed"), "refusal names the reason: {err}");
    // Lookahead stays refused even on valid input.
    let err = tick_resample(&ticks, 60_000, false, Lookahead::On)
        .expect_err("lookahead refuses");
    assert!(err.to_string().contains("lookahead"), "refusal names the reason: {err}");
    assert!(tick_resample(&[], 60_000, false, Lookahead::Off).is_err(), "empty tape refuses");
    // Deterministic: two folds agree bit for bit.
    let again = tick_resample(&ticks, 60_000, false, Lookahead::Off).expect("stable");
    assert_eq!(parts, again);
}

#[test]
fn tick_resample_sparse_skips_dense_fills() {
    // Buckets 0 and 2 populated, bucket 1 empty.
    let ticks = [RawTick { ts_ms: T0, price: 100.0, volume: 10.0 }, RawTick { ts_ms: T0 + 20_000, price: 101.0, volume: 5.0 }, RawTick { ts_ms: T0 + 130_000, price: 102.0, volume: 7.0 }, RawTick { ts_ms: T0 + 190_000, price: 103.0, volume: 9.0 }];
    let sparse = tick_resample(&ticks, 60_000, false, Lookahead::Off).expect("sparse folds");
    assert_eq!(sparse.len(), 2, "empty bucket leaves no candle");
    assert_eq!(sparse[1].ts_ms, T0 + 120_000);
    let dense = tick_resample(&ticks, 60_000, true, Lookahead::Off).expect("dense folds");
    assert_eq!(dense.len(), 3, "empty bucket fills a zero placeholder");
    assert_eq!(dense[1].ts_ms, T0 + 60_000);
    assert_eq!(dense[1].volume, 0.0);
    assert_eq!(
        (dense[1].o, dense[1].h, dense[1].l, dense[1].c),
        (101.0, 101.0, 101.0, 101.0),
        "placeholder chains the last close"
    );
}

fn load_history_csv(name: &str) -> Vec<ClosedBar> {
    let content =
        std::fs::read_to_string(data_root().join("historical").join(name)).expect("history loads");
    let mut lines = content.lines();
    assert_eq!(lines.next().expect("header"), "time,open,high,low,close,Volume");
    lines
        .map(|line| {
            let mut cells = line.split(',');
            let secs = cells.next().expect("time").parse::<i64>().expect("int time");
            let num = |cell: Option<&str>| cell.expect("cell").parse::<f64>().expect("number");
            ClosedBar {
                ts_ms: secs * 1000,
                o: num(cells.next()),
                h: num(cells.next()),
                l: num(cells.next()),
                c: num(cells.next()),
                volume: num(cells.next()),
            }
        })
        .collect()
}

fn load_history_volumes(name: &str) -> Vec<(i64, f64)> {
    let content =
        std::fs::read_to_string(data_root().join("historical").join(name)).expect("history loads");
    content
        .lines()
        .skip(1)
        .map(|line| {
            let mut cells = line.split(',');
            let secs = cells.next().expect("time").parse::<i64>().expect("int time");
            let vol = cells.nth(4).expect("volume").parse::<f64>().expect("number");
            (secs * 1000, vol)
        })
        .collect()
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn project_bars_matches_exchange_minutes() {
    let m1 = load_history_csv("SPY_1M.csv");
    assert_eq!(m1.len(), 436, "minute fixture row count pinned");
    for (target_ms, name) in [(300_000, "SPY_5M.csv"), (900_000, "SPY_15M.csv")] {
        let projected =
            project_bars(&m1, target_ms, Lookahead::Off, None).expect("projection folds");
        assert!(!projected.is_empty());
        for bar in &projected {
            assert_eq!(bar.ts_ms % target_ms, 0, "floors sit on the grid");
        }
        let expected = load_history_csv(name);
        let by_ts: std::collections::HashMap<i64, &ClosedBar> =
            expected.iter().map(|bar| (bar.ts_ms, bar)).collect();
        let mut overlap = 0;
        for bar in &projected {
            if let Some(want) = by_ts.get(&bar.ts_ms) {
                overlap += 1;
                assert_eq!((bar.o, bar.h, bar.l, bar.c), (want.o, want.h, want.l, want.c));
                assert_eq!(bar.volume, want.volume, "volumes sum exactly");
            }
        }
        assert!(overlap > 30, "real overlap, got {overlap}");
    }
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn project_bars_is_conservative_and_closed() {
    let m1 = load_history_csv("SPY_1M.csv");
    let direct = project_bars(&m1, 900_000, Lookahead::Off, None).expect("direct folds");
    let staged = project_bars(&m1, 300_000, Lookahead::Off, None).expect("first hop folds");
    let twice = project_bars(&staged, 900_000, Lookahead::Off, None).expect("second hop folds");
    assert_eq!(direct, twice, "projection composes across hops");
    let total_in: f64 = m1.iter().map(|bar| bar.volume).sum();
    let total_out: f64 = direct.iter().map(|bar| bar.volume).sum();
    assert_close(total_in, total_out, "projection conserves volume");
    assert_eq!(direct.first().expect("head").o, m1.first().expect("head").o);
    assert_eq!(direct.last().expect("tail").c, m1.last().expect("tail").c);
    // Unconfirmed trailing bucket drops out under a cutoff inside it.
    let now_inside_last = direct.last().expect("tail").ts_ms + 1;
    let cut = project_bars(&m1, 900_000, Lookahead::Off, Some(now_inside_last)).expect("cuts");
    assert_eq!(cut.len(), direct.len() - 1, "unconfirmed tail excluded");
    assert!(project_bars(&m1, 900_000, Lookahead::On, None).is_err(), "lookahead refuses");
    assert!(project_bars(&[], 900_000, Lookahead::Off, None).expect("empty").is_empty());
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn tick_tape_resample_conserves_and_aligns() {
    let content = std::fs::read_to_string(data_root().join("historical/SPY_ticks.csv"))
        .expect("tick tape loads");
    let mut lines = content.lines();
    assert_eq!(
        lines.next().expect("header"),
        "timestamp_ms,price,volume,is_out_of_order,original_timestamp_ms"
    );
    let ticks: Vec<RawTick> = lines
        .map(|line| {
            let mut cells = line.split(',');
            RawTick {
                ts_ms: cells.next().expect("stamp").parse().expect("int stamp"),
                price: cells.next().expect("price").parse().expect("price"),
                volume: cells.next().expect("volume").parse().expect("volume"),
            }
        })
        .collect();
    assert_eq!(ticks.len(), 10503, "tape row count pinned");
    let parts = tick_resample(&ticks, 60_000, false, Lookahead::Off).expect("tape folds");
    assert!(!parts.is_empty());
    for bar in &parts {
        assert_eq!(bar.ts_ms % 60_000, 0, "every bucket aligned");
    }
    assert!(parts.windows(2).all(|w| w[0].ts_ms < w[1].ts_ms), "oldest to newest");
    let tape_total: f64 = ticks.iter().map(|tick| tick.volume).sum();
    let parts_total: f64 = parts.iter().map(|bar| bar.volume).sum();
    let trailing_idx = ticks.iter().map(|tick| tick.ts_ms.div_euclid(60_000)).max().expect("tape");
    let trailing: f64 = ticks
        .iter()
        .filter(|tick| tick.ts_ms.div_euclid(60_000) == trailing_idx)
        .map(|tick| tick.volume)
        .sum();
    assert_close(
        tape_total,
        parts_total + trailing,
        "resample conserves tape volume past the unconfirmed tail",
    );
    let m1_total: f64 = load_history_volumes("SPY_1M.csv").iter().map(|(_, v)| v).sum();
    let drift = (tape_total - m1_total).abs() / m1_total * 100.0;
    assert!(drift < 5.0, "tape tracks minute history within 5%, got {drift:.2}%");
}

#[test]
fn timeframe_sweep_aligns_midnights_mondays_and_ms() {
    // Knife-edge stamps: exact midnight UTC, exact Monday open, sub-second ms.
    // Monday 2026-01-12T00:00:00Z; midnight grid floors must hold for every
    // timeframe including seconds.
    let monday_ms: i64 = 1_768_608_000_000;
    for target_ms in [5_000, 15_000, 30_000, 60_000, 300_000, 600_000, 900_000, 1_800_000] {
        assert_eq!(monday_ms % target_ms, 0, "fixture sits on every grid");
        let bars: Vec<ClosedBar> = [-2, -1, 0, 1]
            .map(|k| ClosedBar {
                ts_ms: monday_ms + k * target_ms,
                o: 100.0,
                h: 101.0,
                l: 99.0,
                c: 100.5,
                volume: 10.0,
            })
            .to_vec();
        let projected = project_bars(&bars, target_ms, Lookahead::Off, None).expect("sweeps");
        assert_eq!(projected.len(), 4, "exact-boundary stamps keep own buckets");
        assert_eq!(projected[2].ts_ms, monday_ms, "midnight/Monday bucket exact");
    }
}

#[test]
fn tick_bar_states_follow_bar_time() {
    let single = tick_bar_state(true, true);
    assert!(single.is_new && single.is_confirmed, "single-tick bar opens and confirms");
    assert!(!single.is_history && single.is_realtime, "tick runs emulate realtime");
    let first = tick_bar_state(true, false);
    assert!(first.is_new && !first.is_confirmed);
    let last = tick_bar_state(false, true);
    assert!(!last.is_new && last.is_confirmed);
    let middle = tick_bar_state(false, false);
    assert!(!middle.is_new && !middle.is_confirmed);
}

#[test]
fn source_choice_and_volume_deltas() {
    assert_eq!(choose_source(500, 500), ResampleSource::Ticks, "full cover folds ticks");
    assert_eq!(choose_source(100, 500), ResampleSource::Bars, "partial cover reuses bars");
    assert_eq!(choose_source(0, 500), ResampleSource::Bars, "uncovered reuses bars");
    assert_eq!(choose_source(0, 0), ResampleSource::Bars, "nothing to cover reuses bars");
    assert_eq!(volume_deltas(&[10.0, 20.0, 25.0]), vec![10.0, 5.0], "first seeds");
    assert!(volume_deltas(&[]).is_empty());
    assert!(volume_deltas(&[7.0]).is_empty());
}

#[test]
fn component_surface_validates_through_execute() {
    use fluent_wvr::prelude::{Component, WorkUnit};
    use protopyne_types::Validate;
    let mut config = test_config();
    assert_eq!(
        config.field_names(),
        &["ltf_ms", "fetch_ms", "ctf_ms", "htf_ms", "spike_threshold", "drift_threshold", "fill_gaps"]
    );
    assert_eq!(config.get_field("ltf_ms").expect("reads"), "60000");
    assert!(config.set_field("ltf_ms", "1").is_err(), "reconfigure by rebuilding");
    assert!(config.get_field("nope").is_err());
    let ctx = fluent_wvr::prelude::WorkContext::default();
    let output = config.execute(&ctx).expect("valid config executes");
    assert!(output.success);
    let mut bad = BarsConfig::new(0, 60_000, 300_000, 86_400_000);
    assert!(bad.validate().is_err());
    assert!(bad.execute(&ctx).is_err(), "invalid config fails closed");
    assert!(bad.set_field("anything", "v").is_err());
    let _ = bad.as_any();
}
