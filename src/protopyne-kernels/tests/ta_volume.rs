use super::*;
use crate::compat::compat_for;
use crate::indicator::{builtin_registry, OwnedOhlcv};
use crate::parity::{assert_expr_parity, expr_fixture};
use crate::ta_volume::{AdIndicator, AdoscIndicator, CmfIndicator, ObvIndicator};
use polars::prelude::{col, lit, when, EWMOptions, Expr, IntoLazy};
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

/// Flat prefix plus trend: forces EMA seed coincidence for the oscillator
/// stage, the same device the momentum seeding suite uses.
fn seeded_bars(flat: usize, n: usize) -> Vec<Bar> {
    let mut bars: Vec<Bar> = (0..flat)
        .map(|i| bar_at(100.0, 100.5, 99.5, 100.0, 1_000, i))
        .collect();
    let mut prev_close = 100.0;
    for i in 0..n {
        let close = 100.0 + i as f64 * 0.9 + ((i * 7) % 4) as f64 * 0.6;
        let open = if i == 0 { 100.0 } else { prev_close + 0.1 };
        bars.push(bar_at(
            open,
            open.max(close) + 0.7,
            open.min(close) - 0.7,
            close,
            1_000,
            flat + i,
        ));
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

fn ewm_span(n: usize) -> EWMOptions {
    EWMOptions {
        alpha: 2.0 / (n as f64 + 1.0),
        adjust: false,
        bias: false,
        min_periods: n,
        ignore_nulls: true,
    }
}

/// Money-flow multiplier: signed close position inside the range, defined
/// as 0 when the range is flat — the reference's `otherwise(0)` branch.
fn mfm_expr() -> Expr {
    when(col("high").neq(col("low")))
        .then(
            ((col("close") - col("low")) - (col("high") - col("close")))
                / (col("high") - col("low")),
        )
        .otherwise(lit(0.0))
}

fn ad_expr() -> Expr {
    ((mfm_expr() * col("volume")).cum_sum(false)).alias("ad")
}

/// Staged oscillator reference: the cumulative AD leg is staged as a frame
/// column first (the single-select harness cannot smooth a derived column
/// otherwise), then the fast/slow EMA difference is selected. Staging the
/// per-bar flow instead of its cumulative sum is the classic trap here: the
/// oscillator smooths the running line, not the prints.
fn adosc_reference(bars: &[Bar], fast: usize, slow: usize) -> Vec<Option<f64>> {
    use crate::research::frame_of_bars;
    use polars::prelude::DataType;
    let staged = frame_of_bars(bars)
        .expect("frame")
        .lazy()
        .with_columns([(mfm_expr() * col("volume")).cum_sum(false).alias("__ad")])
        .collect()
        .expect("staged frame");
    assert_eq!(staged.height(), bars.len());
    let selected = staged
        .lazy()
        .select([
            (col("__ad").ewm_mean(ewm_span(fast)) - col("__ad").ewm_mean(ewm_span(slow)))
                .alias("adosc"),
        ])
        .collect()
        .expect("select");
    let cast = selected
        .column("adosc")
        .expect("column")
        .cast(&DataType::Float64)
        .expect("cast");
    let chunked = cast.f64().expect("f64 view");
    (0..chunked.len())
        .map(|i| chunked.get(i).and_then(crate::indicator::fin_or_none))
        .collect()
}

/// Signed close direction: the first bar counts +1 (the reference's
/// `fill_null(1)`), flat bars count 0, moves count their sign.
fn obv_expr() -> Expr {
    let d = col("close") - col("close").shift(lit(1_i64));
    let sign = when(d.clone().is_null()).then(lit(1.0)).otherwise(
        when(d.clone().gt(lit(0.0))).then(lit(1.0)).otherwise(
            when(d.clone().lt(lit(0.0)))
                .then(lit(-1.0))
                .otherwise(lit(0.0)),
        ),
    );
    ((sign * col("volume")).cum_sum(false)).alias("obv")
}

fn empty_params() -> ParamSet {
    ParamSet::new()
}

#[test]
fn ad_matches_expr_on_trend_and_spy() {
    let ad = AdIndicator::new();
    let ps = empty_params();
    assert_eq!(ad.warmup(&ps).expect("valid"), 1);
    for (bars, name) in [
        (trend_bars(60), "trend"),
        (spy_bars(60), "spy"),
        (seeded_bars(14, 40), "seeded"),
    ] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = ad.batch(&data.views(), &ps).expect("valid");
        // Cumulative from bar zero: no warming nulls on clean data.
        assert!(
            result.values.iter().all(Option::is_some),
            "ad defined from bar zero on {name}"
        );
        let want = expr_fixture(&bars, ad_expr()).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, &format!("ad-{name}")).expect("parity");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
            assert_eq!(
                &ad.update(&mut state, tick, &ps).expect("valid"),
                want,
                "ad agreement"
            );
        }
    }
}

#[test]
fn ad_zero_flows_and_missing_legs() {
    let ad = AdIndicator::new();
    let ps = empty_params();
    // Zero range and zero volume contribute nothing but stay defined: the
    // reference multiplies the defined-zero multiplier by the volume.
    let flat: Vec<Bar> = (0..20)
        .map(|i| bar_at(10.0, 10.0, 10.0, 10.0, 1_000, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&flat);
    let result = ad.batch(&data.views(), &ps).expect("valid");
    assert!(result.values.iter().all(|v| *v == Some(0.0)));
    let zero_vol: Vec<Bar> = (0..20)
        .map(|i| bar_at(9.0, 11.0, 8.0, 10.0, 0, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&zero_vol);
    let result = ad.batch(&data.views(), &ps).expect("valid");
    assert!(result.values.iter().all(|v| *v == Some(0.0)));
    // Missing or short legs refuse instead of miscomputing.
    let closes = vec![1.0, 2.0, 3.0];
    let legs = OwnedOhlcv {
        open: vec![],
        high: vec![1.0, 2.0, 3.0],
        low: vec![1.0, 2.0, 3.0],
        close: closes.clone(),
        volume: vec![],
    };
    assert!(ad.batch(&legs.views(), &ps).is_err());
    let short = OwnedOhlcv {
        open: vec![],
        high: vec![1.0, 2.0, 3.0],
        low: vec![1.0, 2.0, 3.0],
        close: closes,
        volume: vec![1.0, 2.0],
    };
    assert!(ad.batch(&short.views(), &ps).is_err());
}

#[test]
fn adosc_matches_staged_ema_difference() {
    let adosc = AdoscIndicator::new();
    let ps = ParamSet::new()
        .with_int("fast_length", 3)
        .with_int("slow_length", 10);
    assert_eq!(adosc.warmup(&ps).expect("valid"), 10);
    let bars = seeded_bars(14, 40);
    let data = OwnedOhlcv::from_bars(&bars);
    let result = adosc.batch(&data.views(), &ps).expect("valid");
    assert!(result.values[..9].iter().all(Option::is_none));
    let want = adosc_reference(&bars, 3, 10);
    assert_expr_parity(&result.values, &want, 1e-9, "adosc").expect("parity");
    let mut state = IndicatorState::new();
    for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
        assert_eq!(
            &adosc.update(&mut state, tick, &ps).expect("valid"),
            want,
            "adosc agreement"
        );
    }
    // Flat flow: the oscillator of a constant line reads zero.
    let flat: Vec<Bar> = (0..20)
        .map(|i| bar_at(10.0, 10.0, 10.0, 10.0, 1_000, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&flat);
    let result = adosc.batch(&data.views(), &ps).expect("valid");
    assert!(result.values[..9].iter().all(Option::is_none));
    assert!(result.values[9..]
        .iter()
        .all(|v| matches!(v, Some(x) if x.abs() <= 1e-9)));
}

#[test]
fn obv_matches_expr_with_first_bar_long() {
    let obv = ObvIndicator::new();
    let ps = empty_params();
    assert_eq!(obv.warmup(&ps).expect("valid"), 1);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = obv.batch(&data.views(), &ps).expect("valid");
        assert!(
            result.values.iter().all(Option::is_some),
            "obv defined from bar zero on {name}"
        );
        let want = expr_fixture(&bars, obv_expr()).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, &format!("obv-{name}")).expect("parity");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
            assert_eq!(
                &obv.update(&mut state, tick, &ps).expect("valid"),
                want,
                "obv agreement"
            );
        }
    }
    // Flat closes accumulate nothing after the opening print: increments are
    // zero, so the line pins at the first bar's volume.
    let flat: Vec<Bar> = (0..20)
        .map(|i| bar_at(10.0, 10.5, 9.5, 10.0, 1_000, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&flat);
    let result = obv.batch(&data.views(), &ps).expect("valid");
    assert!(result.values.iter().all(|v| *v == Some(1_000.0)));
    // Missing or short volume refuses.
    let legs = OwnedOhlcv {
        open: vec![],
        high: vec![],
        low: vec![],
        close: vec![1.0, 2.0, 3.0],
        volume: vec![],
    };
    assert!(obv.batch(&legs.views(), &ps).is_err());
}

fn approx(got: &[Option<f64>], want: &[Option<f64>], eps: f64) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want.iter()) {
        match (g, w) {
            (None, None) => {}
            (Some(g), Some(w)) => assert!((g - w).abs() <= eps, "{g} vs {w}"),
            _ => panic!("na mismatch: {g:?} vs {w:?}"),
        }
    }
}

#[test]
fn mfi_single_sided_flow_pins_100_and_0() {
    use crate::ta_volume::MfiIndicator;
    let mfi = MfiIndicator::new();
    let ps = ParamSet::new().with_int("length", 1);
    // bar1 rises on 200 shares -> 100; bar2 falls on 300 -> 0.
    let legs = OwnedOhlcv {
        open: vec![10.0, 11.0, 10.5],
        high: vec![10.0, 11.0, 10.5],
        low: vec![10.0, 11.0, 10.5],
        close: vec![10.0, 11.0, 10.5],
        volume: vec![100.0, 200.0, 300.0],
    };
    let result = mfi.batch(&legs.views(), &ps).expect("valid");
    approx(&result.values, &[None, Some(100.0), Some(0.0)], 1e-9);
    let ticks: Vec<BarTick> = legs
        .close
        .iter()
        .zip(legs.volume.iter())
        .map(|(&c, &v)| BarTick {
            volume: v,
            ..BarTick::close_only(c)
        })
        .collect();
    let mut state = IndicatorState::new();
    let folded: Vec<Option<f64>> = ticks
        .iter()
        .map(|&t| mfi.update(&mut state, t, &ps).expect("valid update"))
        .collect();
    approx(&folded, &result.values, 1e-12);
}

#[test]
fn vwap_accumulates_price_volume() {
    use crate::ta_volume::VwapIndicator;
    let vwap = VwapIndicator::new();
    let ps = ParamSet::new();
    // (10*100 + 20*300)/400 = 17.5.
    let legs = OwnedOhlcv {
        open: vec![10.0, 20.0],
        high: vec![10.0, 20.0],
        low: vec![10.0, 20.0],
        close: vec![10.0, 20.0],
        volume: vec![100.0, 300.0],
    };
    let result = vwap.batch(&legs.views(), &ps).expect("valid");
    approx(&result.values, &[Some(10.0), Some(17.5)], 1e-9);
    let mut state = IndicatorState::new();
    let folded: Vec<Option<f64>> = legs
        .close
        .iter()
        .zip(legs.volume.iter())
        .map(|(&c, &v)| {
            vwap.update(
                &mut state,
                BarTick {
                    volume: v,
                    ..BarTick::close_only(c)
                },
                &ps,
            )
            .expect("valid update")
        })
        .collect();
    approx(&folded, &result.values, 1e-12);
}

#[test]
fn volume_registry_and_matrix_cover_all_six() {
    let registry = builtin_registry().expect("builtins");
    for id in ["ad", "adosc", "obv", "mfi", "vwap", "cmf"] {
        assert!(registry.get(id).is_some(), "{id} registered");
        let row = compat_for(id).unwrap_or_else(|| panic!("{id} matrix row"));
        assert!((row.eps - 1e-9).abs() == 0.0, "{id} frozen eps");
    }
}

fn cmf_params() -> ParamSet {
    ParamSet::new().with_int("length", 3)
}

#[test]
fn cmf_matches_hand_ratio() {
    // mfm: +0.5, -1.0, +1.0; mfv: 500, -1000, 2000; vol 1000/1000/2000.
    let bars = [
        (10.0, 12.0, 8.0, 11.0, 1_000_u64),
        (11.0, 13.0, 9.0, 9.0, 1_000),
        (9.0, 10.0, 9.0, 10.0, 2_000),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (o, h, l, c, v))| bar_at(o, h, l, c, v, i))
    .collect::<Vec<_>>();
    let data = OwnedOhlcv::from_bars(&bars);
    let cmf = CmfIndicator::new();
    assert_eq!(cmf.warmup(&cmf_params()).expect("valid"), 3);
    let result = cmf.batch(&data.views(), &cmf_params()).expect("valid");
    assert_eq!(result.values.len(), 3);
    assert!(
        result.values[..2].iter().all(Option::is_none),
        "window warms"
    );
    match result.values[2] {
        Some(v) => assert!((v - 0.375).abs() < 1e-12, "hand ratio {v}"),
        None => panic!("hand window defined"),
    }
    let mut state = IndicatorState::new();
    for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
        assert_eq!(
            &cmf.update(&mut state, tick, &cmf_params()).expect("valid"),
            want,
            "cmf agreement"
        );
    }
}

#[test]
fn cmf_zero_volume_window_is_na_and_flat_range_is_zero() {
    let cmf = CmfIndicator::new();
    let ps = ParamSet::new().with_int("length", 5);
    // Zero volume everywhere: 0/0 windows refuse instead of printing zero.
    let dry: Vec<Bar> = (0..20)
        .map(|i| bar_at(9.0, 11.0, 8.0, 10.0, 0, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&dry);
    let result = cmf.batch(&data.views(), &ps).expect("valid");
    assert!(
        result.values.iter().all(Option::is_none),
        "dry windows are na"
    );
    // Flat ranges with real volume: defined-zero flow over real volume.
    let flat: Vec<Bar> = (0..20)
        .map(|i| bar_at(10.0, 10.0, 10.0, 10.0, 1_000, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&flat);
    let result = cmf.batch(&data.views(), &ps).expect("valid");
    assert!(
        result.values[..4].iter().all(Option::is_none),
        "window warms"
    );
    assert!(
        result.values[4..].iter().all(|v| *v == Some(0.0)),
        "flat flow reads zero, not na"
    );
}

#[test]
fn cmf_holds_bounds_and_update_agreement_on_spy() {
    let cmf = CmfIndicator::new();
    let ps = ParamSet::new().with_int("length", 20);
    let bars = spy_bars(120);
    let data = OwnedOhlcv::from_bars(&bars);
    let result = cmf.batch(&data.views(), &ps).expect("valid");
    assert_eq!(result.values.len(), bars.len());
    for v in result.values.iter().flatten().skip(19) {
        assert!((-1.0..=1.0).contains(v), "money flow bounded");
    }
    let mut state = IndicatorState::new();
    for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
        assert_eq!(
            &cmf.update(&mut state, tick, &ps).expect("valid"),
            want,
            "cmf spy agreement"
        );
    }
}

#[test]
fn cmf_refuses_bad_periods_and_short_legs() {
    let cmf = CmfIndicator::new();
    assert!(
        cmf.warmup(&ParamSet::new().with_int("length", 0)).is_err(),
        "zero refuses"
    );
    let legs = OwnedOhlcv {
        open: vec![],
        high: vec![1.0, 2.0, 3.0],
        low: vec![1.0, 2.0, 3.0],
        close: vec![1.0, 2.0, 3.0],
        volume: vec![1.0, 2.0],
    };
    assert!(
        cmf.batch(&legs.views(), &cmf_params()).is_err(),
        "short volume refuses"
    );
}
