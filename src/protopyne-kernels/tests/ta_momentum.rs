use super::*;
use crate::compat::compat_for;
use crate::indicator::{builtin_registry, OwnedOhlcv};
use crate::parity::{assert_expr_parity, expr_fixture};
use crate::ta_momentum::{
    AdxIndicator, ApoIndicator, AroonIndicator, MomIndicator, PpoIndicator, RocIndicator,
    RocpIndicator, Rocr100Indicator, RocrIndicator, RsvIndicator, StochfIndicator, TrixIndicator,
    WillrIndicator,
};
use polars::prelude::{col, lit, when, EWMOptions, Expr, IntoLazy, RollingOptionsFixedWindow};
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

/// Deterministic trend with periodic gap-ups: one source of truth feeding
/// both the kernel legs and the expression frame.
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

/// Flat prefix plus trend: forces Wilder/EMA seed coincidence between the
/// hand-rolled recurrences and the `ewm_mean` reference, the same device the
/// volatility Expr-parity suite uses.
fn seeded_bars(flat: usize, n: usize) -> Vec<Bar> {
    let mut bars: Vec<Bar> = (0..flat)
        .map(|i| bar_at(100.0, 100.0, 100.0, 100.0, 1_000, i))
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

fn strict(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow {
        window_size: n,
        min_periods: n,
        weights: None,
        center: false,
        fn_params: None,
    }
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

fn sma_expr(n: usize) -> Expr {
    col("close").rolling_mean(strict(n))
}

fn ema_expr(n: usize) -> Expr {
    col("close").ewm_mean(ewm_span(n))
}

fn apo_expr(fast: usize, slow: usize, use_ema: bool) -> Expr {
    let (f, s) = if use_ema {
        (ema_expr(fast), ema_expr(slow))
    } else {
        (sma_expr(fast), sma_expr(slow))
    };
    (f - s).alias("apo")
}

fn ppo_expr(fast: usize, slow: usize, use_ema: bool) -> Expr {
    let (f, s) = if use_ema {
        (ema_expr(fast), ema_expr(slow))
    } else {
        (sma_expr(fast), sma_expr(slow))
    };
    ((f.clone() / s) - lit(1.0)).alias("ppo")
}

fn mom_expr(n: usize) -> Expr {
    (col("close") - col("close").shift(lit(n as i64))).alias("mom")
}

fn rocp_expr(n: usize) -> Expr {
    let prev = col("close").shift(lit(n as i64));
    ((col("close") - prev.clone()) / prev).alias("rocp")
}

fn roc_expr(n: usize) -> Expr {
    (rocp_expr(n) * lit(100.0)).alias("roc")
}

fn rocr_expr(n: usize) -> Expr {
    (col("close") / col("close").shift(lit(n as i64))).alias("rocr")
}

fn rocr100_expr(n: usize) -> Expr {
    (rocr_expr(n) * lit(100.0)).alias("rocr100")
}

/// `RSV`: ratio of close position inside the rolling range, defined as 0
/// (not null) when the range is flat or still warming — the reference's
/// `otherwise(0)` branch.
fn rsv_expr(n: usize) -> Expr {
    let highest = col("high").rolling_max(strict(n));
    let lowest = col("low").rolling_min(strict(n));
    when(highest.clone().neq(lowest.clone()))
        .then((col("close") - lowest.clone()) / (highest - lowest))
        .otherwise(lit(0.0))
        .alias("rsv")
}

fn willr_expr(n: usize) -> Expr {
    let highest = col("high").rolling_max(strict(n));
    let lowest = col("low").rolling_min(strict(n));
    when(highest.clone().neq(lowest.clone()))
        .then((highest.clone() - col("close")) / (highest - lowest))
        .otherwise(lit(0.0))
        .alias("willr")
}

fn stochf_fastd_expr(k: usize, d: usize) -> Expr {
    rsv_expr(k).rolling_mean(strict(d)).alias("fastd")
}

/// Staged TRIX reference: the single-select harness cannot nest `ewm_mean`
/// through aliases, so the three smoothing passes are staged as frame
/// columns first and only the final rate-of-change is selected — the same
/// evaluation the harness performs, with identical null/NaN mapping.
fn trix_reference(bars: &[Bar], n: usize) -> Vec<Option<f64>> {
    use crate::research::frame_of_bars;
    use polars::prelude::DataType;
    let staged = frame_of_bars(bars)
        .expect("frame")
        .lazy()
        .with_columns([col("close").ewm_mean(ewm_span(n)).alias("__e1")])
        .with_columns([col("__e1").ewm_mean(ewm_span(n)).alias("__e2")])
        .with_columns([col("__e2").ewm_mean(ewm_span(n)).alias("__e3")])
        .collect()
        .expect("staged frame");
    assert_eq!(staged.height(), bars.len());
    let selected = staged
        .lazy()
        .select([{
            let prev = col("__e3").shift(lit(1_i64));
            ((col("__e3") - prev.clone()) / prev).alias("trix")
        }])
        .collect()
        .expect("select");
    let cast = selected
        .column("trix")
        .expect("column")
        .cast(&DataType::Float64)
        .expect("cast");
    let chunked = cast.f64().expect("f64 view");
    (0..chunked.len())
        .map(|i| chunked.get(i).and_then(crate::indicator::fin_or_none))
        .collect()
}

/// First-match ladder over `(condition, distance)` rungs: the earliest rung
/// whose lagged leg still equals the window extreme wins, reproducing the
/// reference's reverse-argmax (most-recent extreme first). No rung matches
/// inside warmup, where the extreme itself is null — the `NaN` default maps
/// back to `None` through the fixture's null mapping.
fn first_match(rungs: &[(Expr, f64)]) -> Expr {
    match rungs.split_first() {
        None => lit(f64::NAN),
        Some(((cond, k), rest)) => when(cond.clone())
            .then(lit(*k))
            .otherwise(first_match(rest)),
    }
}

/// Reversed-argmax ladder: `argrev` is the distance back to the most recent
/// window extreme, so `1 - argrev / n` reproduces the reference's
/// reverse-argmax shape with strict full-window warmup.
fn aroon_expr(n: usize, use_high: bool) -> Expr {
    let leg = if use_high { col("high") } else { col("low") };
    let extreme = if use_high {
        col("high").rolling_max(strict(n))
    } else {
        col("low").rolling_min(strict(n))
    };
    let nf = n as f64;
    let rungs: Vec<(Expr, f64)> = (0..n)
        .map(|k| {
            let lagged = if k == 0 {
                leg.clone()
            } else {
                leg.clone().shift(lit(k as i64))
            };
            (lagged.eq(extreme.clone()), k as f64)
        })
        .collect();
    (lit(1.0) - first_match(&rungs) / lit(nf)).alias(if use_high { "aroonup" } else { "aroondown" })
}

fn apo_params(fast: usize, slow: usize, matype: f64) -> ParamSet {
    ParamSet::new()
        .with_int("fast_length", fast as i64)
        .with_int("slow_length", slow as i64)
        .with_float("matype", matype)
}

#[test]
fn apo_sma_matches_expr_on_trend_and_spy() {
    let apo = ApoIndicator::new();
    let ps = apo_params(5, 12, 0.0);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = apo.batch(&data.views(), &ps).expect("valid");
        let want = expr_fixture(&bars, apo_expr(5, 12, false)).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, &format!("apo-sma-{name}"))
            .expect("parity");
    }
}

#[test]
fn apo_ema_matches_expr_once_seeded() {
    let apo = ApoIndicator::new();
    let ps = apo_params(5, 12, 1.0);
    let bars = seeded_bars(14, 40);
    let data = OwnedOhlcv::from_bars(&bars);
    let result = apo.batch(&data.views(), &ps).expect("valid");
    let want = expr_fixture(&bars, apo_expr(5, 12, true)).expect("fixture");
    assert_expr_parity(&result.values, &want, 1e-9, "apo-ema").expect("parity");
}

#[test]
fn apo_fast_eq_slow_is_confident_zero() {
    // Identical legs cancel exactly: a confident zero, not a warmup `None`.
    let bars = trend_bars(30);
    let data = OwnedOhlcv::from_bars(&bars);
    for matype in [0.0, 1.0] {
        let ps = apo_params(8, 8, matype);
        let apo = ApoIndicator::new();
        assert_eq!(apo.warmup(&ps).expect("valid"), 8);
        let result = apo.batch(&data.views(), &ps).expect("valid");
        assert!(result.values[..7].iter().all(Option::is_none));
        assert!(
            result.values[7..]
                .iter()
                .all(|v| matches!(v, Some(x) if x.abs() <= 1e-9)),
            "apo fast==slow must read zero, matype {matype}"
        );
    }
}

#[test]
fn apo_update_agrees_with_batch_and_refuses_matype() {
    let bars = trend_bars(40);
    let data = OwnedOhlcv::from_bars(&bars);
    for matype in [0.0, 1.0] {
        let ps = apo_params(5, 12, matype);
        let apo = ApoIndicator::new();
        let batch = apo.batch(&data.views(), &ps).expect("valid");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(batch.values.iter()) {
            let got = apo.update(&mut state, tick, &ps).expect("valid");
            assert_eq!(&got, want, "apo update/batch agreement, matype {matype}");
        }
    }
    let bad = apo_params(5, 12, 2.0);
    assert!(ApoIndicator::new().batch(&data.views(), &bad).is_err());
}

#[test]
fn ppo_matches_expr_and_zero_convention() {
    let ppo = PpoIndicator::new();
    let ps = apo_params(5, 12, 0.0);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = ppo.batch(&data.views(), &ps).expect("valid");
        let want = expr_fixture(&bars, ppo_expr(5, 12, false)).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, &format!("ppo-sma-{name}"))
            .expect("parity");
    }
    let seeded = seeded_bars(14, 40);
    let data = OwnedOhlcv::from_bars(&seeded);
    let ema_ps = apo_params(5, 12, 1.0);
    let result = ppo.batch(&data.views(), &ema_ps).expect("valid");
    let want = expr_fixture(&seeded, ppo_expr(5, 12, true)).expect("fixture");
    assert_expr_parity(&result.values, &want, 1e-9, "ppo-ema").expect("parity");

    // fast==slow divides a leg by itself: confident zero on nonzero data.
    let bars30 = trend_bars(30);
    let data30 = OwnedOhlcv::from_bars(&bars30);
    let eq = apo_params(8, 8, 0.0);
    let result = ppo.batch(&data30.views(), &eq).expect("valid");
    assert!(result.values[..7].iter().all(Option::is_none));
    assert!(result.values[7..]
        .iter()
        .all(|v| matches!(v, Some(x) if x.abs() <= 1e-9)));

    // Update path tracks batch on both smoothing modes.
    let bars = trend_bars(40);
    let data = OwnedOhlcv::from_bars(&bars);
    for matype in [0.0, 1.0] {
        let ps = apo_params(5, 12, matype);
        let batch = ppo.batch(&data.views(), &ps).expect("valid");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(batch.values.iter()) {
            let got = ppo.update(&mut state, tick, &ps).expect("valid");
            assert_eq!(&got, want, "ppo update/batch agreement, matype {matype}");
        }
    }
    assert!(ppo.batch(&data.views(), &apo_params(5, 12, 3.0)).is_err());
}

#[test]
fn mom_matches_expr_with_shift_warmup() {
    let mom = MomIndicator::new();
    let ps = ParamSet::new().with_int("length", 10);
    // Bar 1 reads the close `length` bars back, so the first answer is bar
    // `length + 1` (the shift is part of the warmup, not hidden by it).
    assert_eq!(mom.warmup(&ps).expect("valid"), 11);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = mom.batch(&data.views(), &ps).expect("valid");
        assert!(result.values[..10].iter().all(Option::is_none));
        let want = expr_fixture(&bars, mom_expr(10)).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, &format!("mom-{name}")).expect("parity");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
            assert_eq!(&mom.update(&mut state, tick, &ps).expect("valid"), want);
        }
    }
}

#[test]
fn roc_family_matches_expr() {
    let bars = trend_bars(60);
    let data = OwnedOhlcv::from_bars(&bars);
    let ps = ParamSet::new().with_int("length", 10);
    let cases: [(&dyn Indicator, Expr, &str); 4] = [
        (&RocpIndicator::new(), rocp_expr(10), "rocp"),
        (&RocIndicator::new(), roc_expr(10), "roc"),
        (&RocrIndicator::new(), rocr_expr(10), "rocr"),
        (&Rocr100Indicator::new(), rocr100_expr(10), "rocr100"),
    ];
    for (kernel, expr, name) in cases {
        assert_eq!(kernel.warmup(&ps).expect("valid"), 11, "{name} warmup");
        let result = kernel.batch(&data.views(), &ps).expect("valid");
        assert!(
            result.values[..10].iter().all(Option::is_none),
            "{name} shift warmup"
        );
        let want = expr_fixture(&bars, expr).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, name).expect("parity");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
            assert_eq!(
                &kernel.update(&mut state, tick, &ps).expect("valid"),
                want,
                "{name} agreement"
            );
        }
    }
    // ×100 scaling is exact: roc == rocp * 100 positionally.
    let rocp = RocpIndicator::new()
        .batch(&data.views(), &ps)
        .expect("valid");
    let roc = RocIndicator::new()
        .batch(&data.views(), &ps)
        .expect("valid");
    for (p, r) in rocp.values.iter().zip(roc.values.iter()) {
        match (p, r) {
            (Some(p), Some(r)) => assert!((r - p * 100.0).abs() <= 1e-9),
            (None, None) => {}
            _ => panic!("roc/rocp null shape must agree"),
        }
    }
}

#[test]
fn rsv_matches_expr_with_zero_floor() {
    // The reference defines flat-range AND warmup as 0 (otherwise-branch),
    // so the kernel is confident from bar zero: warmup is 1 and the floor
    // value is Some(0.0), never a null.
    let rsv = RsvIndicator::new();
    let ps = ParamSet::new().with_int("length", 5);
    assert_eq!(rsv.warmup(&ps).expect("valid"), 1);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = rsv.batch(&data.views(), &ps).expect("valid");
        let want = expr_fixture(&bars, rsv_expr(5)).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, &format!("rsv-{name}")).expect("parity");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
            assert_eq!(
                &rsv.update(&mut state, tick, &ps).expect("valid"),
                want,
                "rsv agreement"
            );
        }
    }
    // Flat market pins the defined zero, on both paths.
    let flat: Vec<Bar> = (0..20)
        .map(|i| bar_at(10.0, 10.0, 10.0, 10.0, 1_000, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&flat);
    let result = rsv.batch(&data.views(), &ps).expect("valid");
    assert!(result.values.iter().all(|v| *v == Some(0.0)));
    let mut state = IndicatorState::new();
    for tick in ticks_of(&flat) {
        assert_eq!(rsv.update(&mut state, tick, &ps).expect("valid"), Some(0.0));
    }
}

#[test]
fn willr_matches_expr_with_zero_floor() {
    let willr = WillrIndicator::new();
    let ps = ParamSet::new().with_int("length", 14);
    assert_eq!(willr.warmup(&ps).expect("valid"), 1);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = willr.batch(&data.views(), &ps).expect("valid");
        let want = expr_fixture(&bars, willr_expr(14)).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, &format!("willr-{name}")).expect("parity");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
            assert_eq!(
                &willr.update(&mut state, tick, &ps).expect("valid"),
                want,
                "willr agreement"
            );
        }
    }
    // willr + rsv partition the range: flat markets define both as zero.
    let flat: Vec<Bar> = (0..20)
        .map(|i| bar_at(10.0, 10.0, 10.0, 10.0, 1_000, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&flat);
    let result = willr.batch(&data.views(), &ps).expect("valid");
    assert!(result.values.iter().all(|v| *v == Some(0.0)));
}

#[test]
fn stochf_matches_expr_both_legs() {
    let stochf = StochfIndicator::new();
    let ps = ParamSet::new()
        .with_int("fastk_length", 5)
        .with_int("fastd_length", 3);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = stochf.batch(&data.views(), &ps).expect("valid");
        let want_k = expr_fixture(&bars, rsv_expr(5)).expect("fixture");
        assert_expr_parity(&result.values, &want_k, 1e-9, &format!("stochf-k-{name}"))
            .expect("parity");
        let fastd = result.secondary("fastd").expect("fastd leg");
        let want_d = expr_fixture(&bars, stochf_fastd_expr(5, 3)).expect("fixture");
        assert_expr_parity(fastd, &want_d, 1e-9, &format!("stochf-d-{name}")).expect("parity");
        // fastd smooths the zero-floored fastk: valid from bar m-1.
        assert!(fastd[..2].iter().all(Option::is_none));
        assert!(fastd[2].is_some());
        let mut state = IndicatorState::new();
        for (tick, (want_k, want_d)) in ticks_of(&bars)
            .into_iter()
            .zip(result.values.iter().zip(fastd.iter()))
        {
            let tick_out = stochf.update_full(&mut state, tick, &ps).expect("valid");
            assert_eq!(&tick_out.primary, want_k, "stochf-k agreement");
            let got_d = tick_out
                .secondary
                .iter()
                .find(|v| v.name == "fastd")
                .and_then(|v| v.value);
            assert_eq!(got_d, *want_d, "stochf-d agreement");
        }
    }
}

#[test]
fn trix_matches_nested_ema_expr() {
    // Nested ewm seeding coincides only from matched seed states, so the
    // frame opens flat (same device as the volatility seeding test).
    let trix = TrixIndicator::new();
    let ps = ParamSet::new().with_int("length", 5);
    assert_eq!(trix.warmup(&ps).expect("valid"), 3 * 5 - 1);
    let bars = seeded_bars(14, 40);
    let data = OwnedOhlcv::from_bars(&bars);
    let result = trix.batch(&data.views(), &ps).expect("valid");
    let want = trix_reference(&bars, 5);
    assert_expr_parity(&result.values, &want, 1e-9, "trix").expect("parity");
    let mut state = IndicatorState::new();
    for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
        assert_eq!(
            &trix.update(&mut state, tick, &ps).expect("valid"),
            want,
            "trix agreement"
        );
    }
}

#[test]
fn aroon_matches_reverse_argmax_expr() {
    let aroon = AroonIndicator::new();
    let ps = ParamSet::new().with_int("length", 5);
    assert_eq!(aroon.warmup(&ps).expect("valid"), 5);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = aroon.batch(&data.views(), &ps).expect("valid");
        let want_up = expr_fixture(&bars, aroon_expr(5, true)).expect("fixture");
        assert_expr_parity(&result.values, &want_up, 1e-9, &format!("aroon-up-{name}"))
            .expect("parity");
        let down = result.secondary("down").expect("down leg");
        let want_down = expr_fixture(&bars, aroon_expr(5, false)).expect("fixture");
        assert_expr_parity(down, &want_down, 1e-9, &format!("aroon-down-{name}")).expect("parity");
        // Aroon lives on the (0, 1] grid: strict warmup, then 1/n steps.
        assert!(result.values[..4].iter().all(Option::is_none));
        assert!(
            result.values[4..]
                .iter()
                .all(|v| matches!(v, Some(x) if *x > 0.0 && *x <= 1.0)),
            "aroon range on {name}"
        );
        let mut state = IndicatorState::new();
        for (tick, (want_up, want_down)) in ticks_of(&bars)
            .into_iter()
            .zip(result.values.iter().zip(down.iter()))
        {
            let tick_out = aroon.update_full(&mut state, tick, &ps).expect("valid");
            assert_eq!(&tick_out.primary, want_up, "aroon-up agreement");
            let got_down = tick_out
                .secondary
                .iter()
                .find(|v| v.name == "down")
                .and_then(|v| v.value);
            assert_eq!(got_down, *want_down, "aroon-down agreement");
        }
    }
}

#[test]
fn aroon_tie_break_prefers_most_recent_extreme() {
    // Two equal highs: the most recent one owns the extreme, so aroon-up
    // reads 1.0 — the reference reverses the window before argmax.
    let bars: Vec<Bar> = (0..10)
        .map(|i| {
            let high = if i == 4 || i == 9 {
                120.0
            } else {
                100.0 + i as f64
            };
            bar_at(100.0, high, 99.0, 100.0 + i as f64 * 0.1, 1_000, i)
        })
        .collect();
    let data = OwnedOhlcv::from_bars(&bars);
    let ps = ParamSet::new().with_int("length", 5);
    let aroon = AroonIndicator::new();
    let result = aroon.batch(&data.views(), &ps).expect("valid");
    assert_eq!(result.values[9], Some(1.0));
    let want = expr_fixture(&bars, aroon_expr(5, true)).expect("fixture");
    assert_expr_parity(&result.values, &want, 1e-9, "aroon-tie").expect("parity");
}

#[test]
fn momentum_registry_and_matrix_cover_all_thirteen() {
    let registry = builtin_registry().expect("builtins");
    for id in [
        "apo", "mom", "ppo", "roc", "rocp", "rocr", "rocr100", "rsv", "stochf", "trix", "willr",
        "aroon", "adx",
    ] {
        assert!(registry.get(id).is_some(), "{id} registered");
        let row = compat_for(id).unwrap_or_else(|| panic!("{id} matrix row"));
        assert!((row.eps - 1e-9).abs() == 0.0, "{id} frozen eps");
    }
}

fn adx_params() -> ParamSet {
    ParamSet::new().with_int("length", 14)
}

fn adx2_params() -> ParamSet {
    ParamSet::new().with_int("length", 2)
}

fn adx_bars() -> Vec<Bar> {
    // Rising closes with one pullback bar: exercises +DM, -DM, and TR legs.
    let legs = [
        (9.0, 10.0, 8.0, 9.0),
        (10.0, 11.0, 9.0, 10.0),
        (11.0, 12.0, 9.0, 11.0),
        (10.0, 11.0, 8.0, 10.0),
        (11.5, 12.0, 10.0, 11.5),
        (12.5, 13.0, 11.0, 12.5),
    ];
    legs.into_iter()
        .enumerate()
        .map(|(i, (o, h, l, c))| bar_at(o, h, l, c, 1_000, i))
        .collect()
}

fn assert_close(got: Option<f64>, want: f64, what: &str) {
    match got {
        Some(g) => assert!((g - want).abs() < 1e-9, "{what}: {g} vs {want}"),
        None => panic!("{what}: None vs {want}"),
    }
}

#[test]
fn adx_matches_hand_stepped_rma_composition() {
    // RMA(2) over hand-derived legs: seeds are window means, steps are
    // Wilder recurrences — the shared smoother, not hand-rolled sums.
    let bars = adx_bars();
    let data = OwnedOhlcv::from_bars(&bars);
    let adx = AdxIndicator::new();
    assert_eq!(adx.warmup(&adx2_params()).expect("valid"), 3);
    let result = adx.batch(&data.views(), &adx2_params()).expect("valid");
    assert!(
        result.values[..2].iter().all(Option::is_none),
        "two smoothings warm"
    );
    let plus = result.secondary("plus_di").expect("plus leg");
    let minus = result.secondary("minus_di").expect("minus leg");
    assert_close(plus[2], 30.0, "plus_di[2]");
    assert_close(minus[2], 0.0, "minus_di[2]");
    assert_close(result.values[2], 100.0, "adx[2]");
    assert_close(result.values[3], 57.142857142857146, "adx[3]");
    assert_close(result.values[5], 63.04915514592934, "adx[5]");
    assert_close(plus[5], 38.57142857142857, "plus_di[5]");
    assert_close(minus[5], 5.714285714285714, "minus_di[5]");
}

#[test]
fn adx_reads_strong_on_a_steady_uptrend() {
    let bars = trend_bars(60);
    let data = OwnedOhlcv::from_bars(&bars);
    let adx = AdxIndicator::new();
    let result = adx.batch(&data.views(), &adx_params()).expect("valid");
    assert!(
        result.values[..26].iter().all(Option::is_none),
        "ADX warms 26 bars"
    );
    for leg in [&result.values, result.secondary("plus_di").expect("plus")] {
        for v in leg.iter().flatten().skip(26) {
            assert!((0.0..=100.0).contains(v), "DMI legs range");
        }
    }
    let plus = result.secondary("plus_di").expect("plus leg");
    let minus = result.secondary("minus_di").expect("minus leg");
    let tail = bars.len() - 5..bars.len();
    assert!(
        tail.clone().all(|i| plus[i] > minus[i]),
        "+DI dominates -DI into the trend"
    );
    assert!(
        matches!(result.values[bars.len() - 1], Some(v) if v > 25.0),
        "steady trend reads strong"
    );
}

#[test]
fn adx_holes_emit_none_without_failing() {
    // `Decimal` bars cannot carry NaN: the hole enters at the leg level,
    // exactly like the calibration null-injection control.
    let bars = adx_bars();
    let mut data = OwnedOhlcv::from_bars(&bars);
    data.close[3] = f64::NAN;
    let mut ticks = ticks_of(&bars);
    ticks[3].close = f64::NAN;
    let adx = AdxIndicator::new();
    let result = adx.batch(&data.views(), &adx2_params()).expect("valid");
    assert_eq!(result.values.len(), bars.len(), "length conserved");
    assert!(result.values[3].is_none(), "dirty bar emits None");
    assert!(
        result.secondary("plus_di").expect("plus")[3].is_none(),
        "dirty bar poisons both legs"
    );
    let mut state = IndicatorState::new();
    for (tick, want) in ticks.into_iter().zip(result.values.iter()) {
        assert_eq!(
            &adx.update(&mut state, tick, &adx2_params()).expect("valid"),
            want,
            "adx hole agreement"
        );
    }
}

#[test]
fn adx_refuses_bad_periods() {
    let adx = AdxIndicator::new();
    assert!(
        adx.warmup(&ParamSet::new().with_int("length", 0)).is_err(),
        "zero refuses"
    );
    assert!(adx.warmup(&ParamSet::new()).is_err(), "missing refuses");
}
