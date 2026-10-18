use crate::parity::{assert_expr_parity, expr_fixture};
use crate::research::{
    efficiency_ratio, fractal_dimension, frame_of_bars, frame_of_values, price_density, rsrs,
};
use polars::prelude::{col, lit, Expr, RollingOptionsFixedWindow};
use protopyne_types::Bar;
use std::path::PathBuf;

fn dec(x: f64) -> rust_decimal::Decimal {
    rust_decimal::Decimal::from_f64_retain(x).expect("finite test price")
}

fn bar_at(open: f64, high: f64, low: f64, close: f64, volume: u64, index: usize) -> Bar {
    use chrono::TimeZone as _;
    Bar::new()
        .ts(chrono::Utc
            .with_ymd_and_hms(
                2026,
                1,
                2,
                9 + (index / 3600) as u32,
                ((index / 60) % 60) as u32,
                (index % 60) as u32,
            )
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
        let open = if i == 0 {
            close
        } else {
            prev_close + gap + 0.1
        };
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
        let num = |idx: usize| {
            row.get(idx)
                .expect("column")
                .parse::<f64>()
                .expect("number")
        };
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

fn legs_of(bars: &[Bar]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    use rust_decimal::prelude::ToPrimitive as _;
    let f = |get: fn(&Bar) -> rust_decimal::Decimal| {
        bars.iter()
            .map(|b| get(b).to_f64().unwrap_or(f64::NAN))
            .collect()
    };
    (f(|b| b.h), f(|b| b.l), f(|b| b.c))
}

fn strict(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow {
        window_size: n,
        min_periods: n,
        weights: None,
        center: false,
        fn_params: None,
    }
}

fn pop_std(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow {
        window_size: n,
        min_periods: n,
        weights: None,
        center: false,
        fn_params: Some(polars::prelude::RollingFnParams::Var(
            polars::prelude::RollingVarParams { ddof: 0 },
        )),
    }
}

fn shift1(column: &str) -> Expr {
    col(column) - col(column).shift(lit(1))
}

fn sample_bars() -> Vec<Bar> {
    use chrono::TimeZone as _;
    (0..4)
        .map(|i| {
            Bar::new()
                .ts(chrono::Utc
                    .with_ymd_and_hms(2026, 1, 2, 9, i * 15, 0)
                    .single()
                    .expect("valid timestamp"))
                .o("100.00".parse().expect("decimal"))
                .h("101.00".parse().expect("decimal"))
                .l("99.00".parse().expect("decimal"))
                .c("100.50".parse().expect("decimal"))
                .volume(1_000_u64)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

#[test]
fn bars_frame_carries_ohlcv_columns() {
    let frame = frame_of_bars(&sample_bars()).expect("frame builds");
    assert_eq!(frame.height(), 4);
    assert_eq!(
        frame.get_column_names(),
        vec!["ts", "open", "high", "low", "close", "volume"]
    );
    let close = frame.column("close").expect("close column");
    assert_eq!(close.f64().expect("float").get(0), Some(100.50));
}

#[test]
fn values_frame_preserves_nulls() {
    let frame = frame_of_values("sma", &[None, None, Some(2.0)]).expect("frame builds");
    assert_eq!(frame.height(), 3);
    assert_eq!(frame.column("sma").expect("column").null_count(), 2);
}

#[test]
fn noise_efficiency_ratio_matches_reference() {
    let period = 14;
    let expr = (col("close") - col("close").shift(lit(period as i64))).abs()
        / shift1("close").abs().rolling_sum(strict(period));
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let (_, _, closes) = legs_of(&bars);
        let got = efficiency_ratio(&closes, period);
        assert_eq!(got.len(), bars.len(), "length on {name}");
        let want = expr_fixture(&bars, expr.clone()).expect("fixture");
        assert_expr_parity(&got, &want, 1e-9, &format!("noise-eff_ratio-{name}")).expect("parity");
    }
}

#[test]
fn noise_price_density_matches_reference() {
    let period = 14;
    let expr = (col("high") - col("low")).rolling_sum(strict(period))
        / (col("high").rolling_max(strict(period)) - col("low").rolling_min(strict(period)));
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let (highs, lows, _) = legs_of(&bars);
        let got = price_density(&highs, &lows, period);
        assert_eq!(got.len(), bars.len(), "length on {name}");
        let want = expr_fixture(&bars, expr.clone()).expect("fixture");
        assert_expr_parity(&got, &want, 1e-9, &format!("noise-price_density-{name}"))
            .expect("parity");
    }
}

#[test]
fn noise_fractal_dimension_matches_reference() {
    let period = 14;
    let expr = {
        let n1 = lit(1.0 / period as f64 / period as f64);
        let extent =
            col("high").rolling_max(strict(period)) - col("low").rolling_min(strict(period));
        let leg = (n1 + shift1("close") / extent)
            .sqrt()
            .rolling_sum(strict(period));
        lit(1.0)
            + (leg.log(std::f64::consts::E) + lit(std::f64::consts::LN_2))
                / lit((2 * period) as f64).log(std::f64::consts::E)
    };
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let (highs, lows, closes) = legs_of(&bars);
        let got = fractal_dimension(&highs, &lows, &closes, period);
        assert_eq!(got.len(), bars.len(), "length on {name}");
        let want = expr_fixture(&bars, expr.clone()).expect("fixture");
        assert_expr_parity(&got, &want, 1e-9, &format!("noise-fractal-{name}")).expect("parity");
    }
}

#[test]
fn noise_degenerate_inputs_agree_on_none() {
    // A flat line zeroes every numerator and denominator: both sides read
    // `None` (the reference via NaN, the kernel via the finiteness guard).
    let flat = vec![100.0; 30];
    let period = 14;
    for got in [
        efficiency_ratio(&flat, period),
        price_density(&flat, &flat, period),
        fractal_dimension(&flat, &flat, &flat, period),
    ] {
        assert_eq!(got.len(), flat.len());
        assert!(got.iter().all(Option::is_none), "flat input stays None");
    }
    // Short inputs never fabricate a window.
    assert!(efficiency_ratio(&[1.0, 2.0], 14)
        .iter()
        .all(Option::is_none));
    assert!(price_density(&[1.0], &[1.0], 14)
        .iter()
        .all(Option::is_none));
    assert!(rsrs(&[1.0, 2.0], &[1.0, 2.0], 5, 10)
        .iter()
        .all(Option::is_none));
}

#[test]
fn rsrs_matches_moment_reference() {
    // Least-squares slope in moment form (exact least squares with
    // intercept), then the rolling z-score — the same two stages the
    // reference composes from its rolling helpers.
    let (n, m) = (5, 10);
    let slope = {
        let mx = col("low").rolling_mean(strict(n));
        let my = col("high").rolling_mean(strict(n));
        let mxy = (col("low") * col("high")).rolling_mean(strict(n));
        let mxx = (col("low") * col("low")).rolling_mean(strict(n));
        (mxy - mx.clone() * my.clone()) / (mxx - mx.clone() * mx)
    };
    let expr =
        (slope.clone() - slope.clone().rolling_mean(strict(m))) / slope.rolling_std(pop_std(m));
    for (bars, name, eps) in [(trend_bars(60), "trend", 1e-9), (spy_bars(60), "spy", 1e-6)] {
        let (highs, lows, _) = legs_of(&bars);
        let got = rsrs(&highs, &lows, n, m);
        assert_eq!(got.len(), bars.len(), "length on {name}");
        let want = expr_fixture(&bars, expr.clone()).expect("fixture");
        // SPY-scale prices cancel ~1e-8 through the reference's raw-moment
        // slope form (products near 1e6); the kernel demeans per window and
        // is the stable side, so the SPY tolerance absorbs reference-side
        // dust, not kernel error.
        assert_expr_parity(&got, &want, eps, &format!("rsrs-{name}")).expect("parity");
    }
}

/// Calibration: RSRS lookbacks earn trust from no-fire controls (constant or
/// too-short inputs are all `None`) plus Expr-parity on real data — never from
/// widening a tolerance. The upstream defaults (`n=18`, `m=600`) mean any
/// history under 600 bars honestly reports `None` everywhere.
#[test]
fn rsrs_calibration_controls() {
    // Constant legs: zero slope variance, nothing to score.
    let flat = vec![100.0; 700];
    assert!(rsrs(&flat, &flat, 18, 600).iter().all(Option::is_none));
    // Short history: fewer bars than the scoring window.
    let short = vec![100.0; 60];
    assert!(rsrs(&short, &short, 5, 30).iter().all(Option::is_none));
    // Perfect trend: the slope never varies, so its z-score is undefined.
    let line: Vec<f64> = (0..700).map(|i| 100.0 + f64::from(i) * 0.5).collect();
    assert!(rsrs(&line, &line, 18, 600).iter().all(Option::is_none));
    // Seeded noise detunes: finite scores whose mean stays near zero instead
    // of concentrating on a spurious ranking.
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (u64::MAX >> 11) as f64 - 0.5
    };
    let (highs, lows): (Vec<f64>, Vec<f64>) = (0..700)
        .map(|i| {
            let base = 100.0 + next() * 4.0 + (f64::from(i) * 0.01).sin();
            (base + next().abs(), base - next().abs())
        })
        .unzip();
    let scored: Vec<f64> = rsrs(&highs, &lows, 18, 600).into_iter().flatten().collect();
    assert!(!scored.is_empty(), "noise scores finite values");
    let mean = scored.iter().sum::<f64>() / scored.len() as f64;
    assert!(mean.abs() < 0.5, "noise detunes near zero, got {mean}");
}

#[test]
fn noise_divisions_refuse_non_finite_legs() {
    // A non-finite leg anywhere in the window refuses the whole window
    // (strict windows exclude it first); the division never sees it, so
    // the `div_or_none` migrations below cannot change these arms.
    let clean: Vec<f64> = (0..30).map(|i| 100.0 + f64::from(i) * 0.3).collect();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut highs = clean.clone();
        let mut lows = clean.clone();
        highs[15] = bad;
        lows[20] = bad;
        for got in [
            efficiency_ratio(&clean, 14),
            price_density(&clean, &clean, 14),
            fractal_dimension(&clean, &clean, &clean, 14),
        ] {
            assert!(got[29].is_some(), "clean window scores");
        }
        assert!(
            efficiency_ratio(&highs, 14)[15..]
                .iter()
                .all(Option::is_none),
            "non-finite close refuses every window covering it"
        );
        assert!(
            price_density(&highs, &lows, 14)[29].is_none(),
            "non-finite high/low refuses density"
        );
        assert!(
            fractal_dimension(&highs, &lows, &highs, 14)[29].is_none(),
            "non-finite leg refuses dimension"
        );
    }
}

#[test]
fn noise_divisions_stay_vacuous_on_degenerate_periods() {
    // Period 0 builds one-bar windows (zero displacement over zero path):
    // `0/0` reads `None` on both spellings, never `Some(inf)`.
    let closes: Vec<f64> = (0..10).map(|i| 100.0 + f64::from(i)).collect();
    assert!(
        efficiency_ratio(&closes, 0).iter().all(Option::is_none),
        "zero period stays vacuous"
    );
    assert!(
        price_density(&closes, &closes, 0)
            .iter()
            .all(Option::is_none),
        "zero period stays vacuous"
    );
    // Empty inputs never fabricate a window.
    assert!(efficiency_ratio(&[], 14).is_empty());
    assert!(price_density(&[], &[], 14).is_empty());
    assert!(fractal_dimension(&[], &[], &[], 14).is_empty());
}
