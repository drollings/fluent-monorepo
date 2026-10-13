//! Stored opens: the ring persists each candle's open at write time instead
//! of re-chaining from the prior close at read time.
//!
//! Representation change, not a numeric one: every stored open equals what
//! the chained read returned, proven by the existing suites staying
//! byte-identical. One latent bug heals along the way: past eviction the
//! chained read kept serving the stale `candle_open` for the new first
//! candle, while storage keeps each candle's own open — pinned by
//! `evicted_first_candle_keeps_own_open` (red before the change).

use super::*;

const MIN: i64 = 60_000;

fn gappy_rows(count: usize) -> Vec<LtfRow> {
    // Opens deliberately far from closes so chained values are
    // distinguishable from row opens past the first candle.
    let base = 1_699_999_980_000;
    (0..count)
        .map(|i| LtfRow {
            ts_ms: base + i as i64 * MIN,
            o: 200.0 + i as f64,
            h: 102.0,
            l: 98.0,
            c: 100.0 + i as f64,
            volume: 10.0,
        })
        .collect()
}

fn assert_close(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() <= 1e-9, "{what}: got {got}, want {want}");
}

#[test]
fn stored_opens_match_chained_reads() {
    let config = BarsConfig::new(MIN, MIN, 5 * MIN, 86_400_000);
    let mut ring = BarRing::new(&config).expect("valid config");
    let rows = gappy_rows(3);
    ring.reconcile(&rows, 30.0, 200.0);
    // First open chains from the first row; later opens chain from the
    // previous close — exactly what the read-time chain returned.
    assert_close(ring.get(0).expect("readable").o, 200.0, "first open");
    assert_close(ring.get(1).expect("readable").o, 100.0, "second chains prev close");
    assert_close(ring.get(2).expect("readable").o, 101.0, "third chains prev close");
}

#[test]
fn evicted_first_candle_keeps_own_open() {
    // RED before stored opens: the chained read served the stale
    // `candle_open` (rows[0].o == 200.0) for the post-eviction first candle.
    let config = BarsConfig::new(MIN, MIN, 5 * MIN, 86_400_000);
    assert_eq!(config.max_candles(), 10, "10-slot ring for the pin");
    let mut ring = BarRing::new(&config).expect("valid config");
    let rows = gappy_rows(12);
    ring.reconcile(&rows, 120.0, 200.0);
    assert_eq!(ring.len(), 10, "overflow caps at capacity");
    let first = ring.get(0).expect("readable");
    assert_eq!(first.ts_ms, rows[2].ts_ms, "oldest two evicted");
    assert_close(first.o, 101.0, "evicted first keeps its chained open, not stale candle_open");
}

#[test]
fn remainder_adjustments_keep_chained_opens() {
    let config = BarsConfig::new(MIN, MIN, 5 * MIN, 86_400_000);
    let mut ring = BarRing::new(&config).expect("valid config");
    let rows = gappy_rows(2);
    // Shortfall appends: the top-up chains from the previous close.
    let short = ring.reconcile(&rows, 30.0, 200.0);
    assert!(short.remainder.is_some(), "shortfall tops up");
    assert_close(ring.get(2).expect("readable").o, 101.0, "top-up chains prev close");
    // Overshoot trims: surviving candles keep their stored opens.
    let over = ring.reconcile(&rows, 15.0, 200.0);
    assert!(over.remainder.is_some(), "overshoot trims");
    assert_close(ring.get(0).expect("readable").o, 200.0, "trim keeps first open");
    assert_close(ring.get(1).expect("readable").o, 100.0, "trim keeps chained open");
}
