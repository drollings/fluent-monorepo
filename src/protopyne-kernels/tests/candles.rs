use super::*;
use crate::parity::{assert_expr_parity, expr_fixture};
use polars::prelude::{Expr, col, lit, when};
use protopyne_types::Bar;
use std::path::PathBuf;

fn dec(x: f64) -> rust_decimal::Decimal {
    rust_decimal::Decimal::from_f64_retain(x).expect("finite test price")
}

fn bar_at(open: f64, high: f64, low: f64, close: f64, volume: u64, index: usize) -> Bar {
    use chrono::TimeZone as _;
    Bar::new()
        .ts(chrono::Utc
            .with_ymd_and_hms(2026, 1, 2, 9 + (index / 3600) as u32, ((index / 60) % 60) as u32, (index % 60) as u32)
            .single()
            .expect("valid timestamp"))
        .o(dec(open))
        .h(dec(high))
        .l(dec(low))
        .c(dec(close))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

fn trend_bars(n: usize) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(n);
    let mut prev_close = 100.0;
    for i in 0..n {
        let drift = i as f64 * 0.7 + ((i * 13) % 5) as f64 * 0.9;
        let close = 100.0 + drift;
        let gap = if i > 0 && i % 10 == 0 { 2.5 } else { 0.0 };
        let open = if i == 0 { close } else { prev_close + gap + 0.1 };
        let high = open.max(close) + 0.8;
        let low = open.min(close) - 0.8;
        bars.push(bar_at(open, high, low, close, 1_000 + i as u64 * 37, i));
        prev_close = close;
    }
    bars
}

fn spy_bars(count: usize) -> Vec<Bar> {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .expect("vendored CSV present");
    let mut bars = Vec::new();
    for record in reader.records().take(count) {
        let row = record.expect("readable row");
        let num = |idx: usize| row.get(idx).expect("column").parse::<f64>().expect("number");
        let epoch: i64 = row.get(0).expect("time").parse().expect("epoch");
        let ts = chrono::DateTime::from_timestamp(epoch, 0).expect("valid epoch");
        bars.push(
            Bar::new()
                .ts(ts)
                .o(dec(num(1)))
                .h(dec(num(2)))
                .l(dec(num(3)))
                .c(dec(num(4)))
                .volume(row.get(5).expect("vol").parse().expect("volume"))
                .maybe_adj(None)
                .build(),
        );
    }
    bars
}

fn ticks_of(bars: &[Bar]) -> Vec<BarTick> {
    use rust_decimal::prelude::ToPrimitive as _;
    bars.iter()
        .map(|b| BarTick {
            open: b.o.to_f64().unwrap_or(f64::NAN),
            high: b.h.to_f64().unwrap_or(f64::NAN),
            low: b.l.to_f64().unwrap_or(f64::NAN),
            close: b.c.to_f64().unwrap_or(f64::NAN),
            volume: b.volume as f64,
        })
        .collect()
}
fn tick(open: f64, high: f64, low: f64, close: f64) -> BarTick {
    BarTick { open, high, low, close, volume: 1_000.0 }
}

/// Bit-exact float equality: these hand values are exactly representable,
/// so the assertion is identity, not tolerance (clippy pedantic forbids
/// `assert_eq!` on floats outright).
fn assert_bits(actual: f64, expected: f64) {
    assert_eq!(actual.to_bits(), expected.to_bits());
}

/// Null-safe two-way maximum: the `max_horizontal` the reference uses has no
/// unambiguous re-export on this side, so the ladder stands in for it.
fn max2(a: Expr, b: Expr) -> Expr {
    when(a.clone().gt(b.clone())).then(a).otherwise(b)
}

fn min2(a: Expr, b: Expr) -> Expr {
    when(a.clone().lt(b.clone())).then(a).otherwise(b)
}

/// One single-bar reference case: id, reference expression, kernel probe.
type MeasureCase<'a> = (&'a str, Expr, Box<dyn Fn(BarTick) -> Option<f64>>);
/// One two-bar reference case: id, shifted reference expression, kernel probe.
type GapCase<'a> = (&'a str, Expr, Box<dyn Fn(BarTick, BarTick) -> bool>);
/// One planted pattern: the bar plus the predicate that must fire on it.
type Planted = (BarTick, fn(BarTick) -> bool);

fn open() -> Expr {
    col("open")
}
fn high() -> Expr {
    col("high")
}
fn low() -> Expr {
    col("low")
}
fn close() -> Expr {
    col("close")
}

#[test]
fn measures_match_hand_values() {
    let bar = tick(100.0, 103.0, 98.0, 102.0);
    assert_bits(real_body(bar), 2.0);
    assert_bits(upper_shadow(bar), 1.0);
    assert_bits(lower_shadow(bar), 2.0);
    assert_bits(high_low_range(bar), 5.0);
    assert_bits(upper_body(bar), 102.0);
    assert_bits(lower_body(bar), 100.0);
    assert_bits(shadows(bar), 3.0);
    // body / (2 * range - body + eps): 2 / (10 - 2 + 1e-8).
    assert_bits(efficiency_ratio(bar), 2.0 / (8.0 + TA_EPSILON));
    // Bearish bar mirrors the legs.
    let bear = tick(102.0, 103.0, 98.0, 100.0);
    assert_bits(real_body(bear), 2.0);
    assert_bits(upper_shadow(bear), 1.0);
    assert_bits(lower_shadow(bear), 2.0);
    assert_bits(upper_body(bear), 102.0);
    assert_bits(lower_body(bear), 100.0);
    // Degenerate flat bar: zero displacement over zero distance reads zero,
    // not a division artifact.
    let flat = tick(100.0, 100.0, 100.0, 100.0);
    assert_bits(efficiency_ratio(flat), 0.0);
    assert_bits(shadows(flat), 0.0);
}

#[test]
fn predicates_fire_on_boundaries() {
    // Color is inclusive: equal opens read bullish.
    assert!(candle_color(tick(100.0, 103.0, 98.0, 102.0)));
    assert!(!candle_color(tick(102.0, 103.0, 98.0, 100.0)));
    assert!(candle_color(tick(100.0, 101.0, 99.0, 100.0)));
    // Four-price: the whole range fits inside epsilon.
    assert!(four_price_doji(tick(50.0, 50.0, 50.0, 50.0)));
    assert!(four_price_doji(tick(0.0, TA_EPSILON, 0.0, 0.0)));
    assert!(!four_price_doji(tick(0.0, 2.0 * TA_EPSILON, 0.0, 0.0)));
    assert!(!four_price_doji(tick(100.0, 103.0, 98.0, 102.0)));
    // Doji boundary is inclusive at exactly epsilon.
    assert!(doji(tick(0.0, 1.0, -1.0, TA_EPSILON)));
    assert!(doji(tick(100.0, 101.0, 99.0, 100.0)));
    assert!(!doji(tick(0.0, 1.0, -1.0, 2.0 * TA_EPSILON)));
    // Dragonfly needs the lower tail; gravestone needs the upper tail.
    let fly = tick(100.0, 100.0, 99.0, 100.0);
    assert!(doji(fly) && dragonfly(fly) && !gravestone(fly));
    let stone = tick(100.0, 101.0, 100.0, 100.0);
    assert!(doji(stone) && gravestone(stone) && !dragonfly(stone));
    // A tailless doji is neither.
    let plain = tick(100.0, 100.0, 100.0, 100.0);
    assert!(doji(plain) && !dragonfly(plain) && !gravestone(plain));
    // A tailed non-doji is neither.
    let bodied = tick(100.0, 103.0, 97.0, 102.0);
    assert!(!doji(bodied) && !dragonfly(bodied) && !gravestone(bodied));
}

#[test]
fn gaps_compare_against_the_prior_bar() {
    let prev = tick(100.0, 101.0, 99.0, 100.5);
    // Full-range gaps are strict: touching is contiguous, not gapped.
    assert!(gap_up(prev, tick(101.5, 103.0, 101.5, 102.0)));
    assert!(!gap_up(prev, tick(100.5, 103.0, 101.0, 102.0)));
    assert!(gap_down(prev, tick(97.0, 98.5, 97.0, 98.0)));
    assert!(!gap_down(prev, tick(97.0, 99.0, 97.0, 98.0)));
    // Body gaps compare edges: current lower body clears the prior upper.
    assert!(real_body_gap_up(prev, tick(101.5, 103.0, 101.5, 102.0)));
    assert!(!real_body_gap_up(prev, tick(99.5, 103.0, 99.5, 102.0)));
    assert!(real_body_gap_down(prev, tick(97.0, 98.5, 97.0, 98.0)));
    assert!(!real_body_gap_down(prev, tick(97.0, 102.0, 97.0, 101.0)));
}

#[test]
fn missing_legs_never_affirm_a_pattern() {
    let nan = BarTick {
        open: f64::NAN,
        high: 101.0,
        low: 99.0,
        close: 100.0,
        volume: 1_000.0,
    };
    assert!(real_body(nan).is_nan());
    // Horizontal max/min ignore the missing leg (mirroring the reference's
    // null-ignoring horizontals), so the surviving shadow legs still read.
    assert_bits(upper_shadow(nan), 1.0);
    assert_bits(lower_shadow(nan), 1.0);
    assert!(!candle_color(nan));
    assert!(!four_price_doji(nan));
    assert!(!doji(nan));
    assert!(!dragonfly(nan));
    assert!(!gravestone(nan));
    assert!(!gap_up(nan, tick(100.0, 101.0, 99.0, 100.0)));
    assert!(!gap_up(tick(100.0, 101.0, 99.0, 100.0), nan));
}

#[test]
fn single_bar_fns_match_reference_exprs() {
    let cases: Vec<MeasureCase<'_>> = vec![
        ("real_body", (close() - open()).abs(), Box::new(|t| Some(real_body(t)))),
        ("upper_shadow", high() - max2(open(), close()), Box::new(|t| Some(upper_shadow(t)))),
        ("lower_shadow", min2(open(), close()) - low(), Box::new(|t| Some(lower_shadow(t)))),
        ("high_low_range", high() - low(), Box::new(|t| Some(high_low_range(t)))),
        ("upper_body", max2(open(), close()), Box::new(|t| Some(upper_body(t)))),
        ("lower_body", min2(open(), close()), Box::new(|t| Some(lower_body(t)))),
        (
            "shadows",
            (high() - low()) - (close() - open()).abs(),
            Box::new(|t| Some(shadows(t))),
        ),
        (
            "efficiency_ratio",
            {
                let body = (close() - open()).abs();
                let distance = lit(2.0) * (high() - low()) - body.clone();
                body / (distance + lit(TA_EPSILON))
            },
            Box::new(|t| Some(efficiency_ratio(t))),
        ),
        (
            "candle_color",
            close().gt_eq(open()),
            Box::new(|t| Some(f64::from(candle_color(t)))),
        ),
        (
            "four_price_doji",
            low().gt_eq(high() - lit(TA_EPSILON)),
            Box::new(|t| Some(f64::from(four_price_doji(t)))),
        ),
        (
            "doji",
            (open() - close()).abs().lt_eq(lit(TA_EPSILON)),
            Box::new(|t| Some(f64::from(doji(t)))),
        ),
        (
            "dragonfly",
            (open() - close()).abs().lt_eq(lit(TA_EPSILON)).and(low().lt(close() - lit(TA_EPSILON))),
            Box::new(|t| Some(f64::from(dragonfly(t)))),
        ),
        (
            "gravestone",
            (open() - close()).abs().lt_eq(lit(TA_EPSILON)).and(high().gt(close() + lit(TA_EPSILON))),
            Box::new(|t| Some(f64::from(gravestone(t)))),
        ),
    ];
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let ticks = ticks_of(&bars);
        for (id, expr, apply) in &cases {
            let want = expr_fixture(&bars, expr.clone()).expect("fixture");
            let got: Vec<Option<f64>> = ticks.iter().map(|t| apply(*t)).collect();
            assert_eq!(got.len(), want.len(), "{id} length on {name}");
            assert_expr_parity(&got, &want, 1e-9, &format!("candle-{id}-{name}")).expect("parity");
        }
    }
}

#[test]
fn gap_fns_match_reference_shift_exprs() {
    let prev_high = high().shift(lit(1));
    let prev_low = low().shift(lit(1));
    let prev_upper = max2(open(), close()).shift(lit(1));
    let prev_lower = min2(open(), close()).shift(lit(1));
    let cases: Vec<GapCase<'_>> = vec![
        ("gap_up", low().gt(prev_high), Box::new(gap_up)),
        ("gap_down", high().lt(prev_low), Box::new(gap_down)),
        (
            "real_body_gap_up",
            min2(open(), close()).gt(prev_upper),
            Box::new(real_body_gap_up),
        ),
        (
            "real_body_gap_down",
            max2(open(), close()).lt(prev_lower),
            Box::new(real_body_gap_down),
        ),
    ];
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let ticks = ticks_of(&bars);
        for (id, expr, apply) in &cases {
            let want = expr_fixture(&bars, expr.clone()).expect("fixture");
            // No predecessor on the opener: the reference yields null, which
            // is the absence the kernel reports too.
            let got: Vec<Option<f64>> = ticks
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    if i == 0 {
                        // No predecessor on the opener: the reference yields
                        // null, and the kernel reports the same absence.
                        None
                    } else {
                        Some(f64::from(apply(ticks[i - 1], *t)))
                    }
                })
                .collect();
            assert_expr_parity(&got, &want, 0.0, &format!("candle-{id}-{name}")).expect("parity");
        }
    }
}

/// Calibration: thresholds are task-value heuristics, so they earn trust
/// from a must-NOT-fire control group (false-positive rate 0) plus planted
/// patterns (recall 1) — never from widening a tolerance.
#[test]
fn calibration_controls_never_fire_and_planted_always_fire() {
    // Control group one: ordinary trending bars carry bodies far above
    // epsilon — no doji-family predicate may fire. The opener is skipped:
    // it is a degenerate single-point bar (open == close by construction).
    let trend = ticks_of(&trend_bars(60));
    let trend_fires = trend
        .iter()
        .skip(1)
        .filter(|t| doji(**t) || dragonfly(**t) || gravestone(**t) || four_price_doji(**t))
        .count();
    assert_eq!(trend_fires, 0, "control group one must not fire");
    // Control group two: high-volatility bars with large bodies — range and
    // displacement both orders of magnitude above the threshold.
    let volatile: Vec<BarTick> = (0..40)
        .map(|i| {
            let base = 100.0 + f64::from(i) * 3.0;
            tick(base, base + 9.0, base - 9.0, base + 7.5)
        })
        .collect();
    let volatile_fires = volatile
        .iter()
        .filter(|t| doji(**t) || dragonfly(**t) || gravestone(**t) || four_price_doji(**t))
        .count();
    assert_eq!(volatile_fires, 0, "control group two must not fire");
    // Planted patterns: five of each family; every planting must report.
    let mut planted: Vec<Planted> = Vec::new();
    for i in 0..5 {
        let base = 50.0 + f64::from(i);
        planted.push((tick(base, base + 2.0, base - 2.0, base), doji));
        planted.push((tick(base, base, base - 3.0, base), dragonfly));
        planted.push((tick(base, base + 3.0, base, base), gravestone));
        planted.push((tick(base, base, base, base), four_price_doji));
    }
    let missed = planted.iter().filter(|(t, pred)| !pred(*t)).count();
    assert_eq!(missed, 0, "planted patterns must all fire");
    assert_eq!(planted.len(), 20);
    // Precision/recall against the reference on real data: the SPY parity
    // suites above already assert exact agreement, so the frozen threshold
    // set (TA_EPSILON = 1e-8) holds 1.0 accept with 0.0 false-fire here.
    let spy = ticks_of(&spy_bars(60));
    let spy_fires = spy
        .iter()
        .filter(|t| doji(**t) || dragonfly(**t) || gravestone(**t) || four_price_doji(**t))
        .count();
    assert!(
        spy_fires < spy.len(),
        "selective firing on real data ({spy_fires} of {})",
        spy.len()
    );
}
