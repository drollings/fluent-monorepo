use super::*;
use crate::indicator::{Ohlcv, OwnedOhlcv};
use crate::parity::{assert_expr_parity, expr_fixture};
use crate::ta_price::{AvgPriceIndicator, MedPriceIndicator, TypPriceIndicator, WclPriceIndicator};
use polars::prelude::{col, lit, Expr};
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
        let close = 100.0 + i as f64 * 0.7 + ((i * 13) % 5) as f64 * 0.9;
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

fn check_price_kernel(kernel: &dyn Indicator, bars: &[Bar], expr: Expr, context: &str) {
    let data = OwnedOhlcv::from_bars(bars);
    let ps = ParamSet::new();
    let result = kernel.batch(&data.views(), &ps).expect("valid params");
    let want = expr_fixture(bars, expr).expect("expr evaluates");
    assert_expr_parity(&result.values, &want, 1e-9, context).expect("parity");
    assert_eq!(kernel.warmup(&ps).expect("valid"), 1);
    assert!(result.values[0].is_some(), "{context} valid from first bar");
    let mut state = IndicatorState::new();
    let folded: Vec<Option<f64>> = ticks_of(bars)
        .into_iter()
        .map(|t| kernel.update(&mut state, t, &ps).expect("valid"))
        .collect();
    assert_eq!(result.values, folded, "{context} update/batch agreement");
}

#[test]
fn price_kernels_match_exprs_on_spy_slice() {
    let bars = spy_bars(60);
    check_price_kernel(
        &AvgPriceIndicator::new(),
        &bars,
        ((col("open") + col("high") + col("low") + col("close")) / lit(4.0)).alias("avgprice"),
        "avgprice",
    );
    check_price_kernel(
        &MedPriceIndicator::new(),
        &bars,
        ((col("high") + col("low")) / lit(2.0)).alias("medprice"),
        "medprice",
    );
    check_price_kernel(
        &TypPriceIndicator::new(),
        &bars,
        ((col("high") + col("low") + col("close")) / lit(3.0)).alias("typprice"),
        "typprice",
    );
    check_price_kernel(
        &WclPriceIndicator::new(),
        &bars,
        ((col("high") + col("low") + col("close") * lit(2.0)) / lit(4.0)).alias("wclprice"),
        "wclprice",
    );
}

#[test]
fn price_kernels_match_exprs_on_gapped_trend() {
    let bars = trend_bars(30);
    check_price_kernel(
        &AvgPriceIndicator::new(),
        &bars,
        ((col("open") + col("high") + col("low") + col("close")) / lit(4.0)).alias("avgprice"),
        "avgprice gapped",
    );
    check_price_kernel(
        &WclPriceIndicator::new(),
        &bars,
        ((col("high") + col("low") + col("close") * lit(2.0)) / lit(4.0)).alias("wclprice"),
        "wclprice gapped",
    );
}

#[test]
fn price_kernels_propagate_missing_legs_as_none() {
    // A `NaN` leg poisons only its own row; neighbors stay computable.
    let close = [10.0, f64::NAN, 12.0];
    let legs = Ohlcv {
        open: &[9.0, 9.5, 11.0],
        high: &[11.0, 12.0, 13.0],
        low: &[8.0, 9.0, 10.0],
        close: &close,
        volume: &[100.0, 100.0, 100.0],
    };
    let ps = ParamSet::new();
    let avg = AvgPriceIndicator::new().batch(&legs, &ps).expect("valid");
    assert!(avg.values[0].is_some());
    assert_eq!(avg.values[1], None);
    assert!(avg.values[2].is_some());
    // Close-only input refuses: the high/low legs are required, never assumed.
    let close_only = Ohlcv::close_only(&[10.0, 11.0]);
    assert!(AvgPriceIndicator::new().batch(&close_only, &ps).is_err());
    assert!(MedPriceIndicator::new().batch(&close_only, &ps).is_err());
    assert!(TypPriceIndicator::new().batch(&close_only, &ps).is_err());
    assert!(WclPriceIndicator::new().batch(&close_only, &ps).is_err());
}
