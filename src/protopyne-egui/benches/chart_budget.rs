//! Trading-domain budget baselines for the terminal crate: throughput
//! numbers only, never optimization targets and never gates.
//!
//! Each bench runs a pure headless op over synthetic rows/series (no CSV,
//! no network, no display): oscillator normalization, quiet-label folding,
//! CSV export, and one full IntegrationLite pane build. What fails here as
//! a timing regression never reaches a gate — gates assert API shape (see
//! `tests/budget.rs`), wall-time is recorded in the bench note only.
//! Mirrors the `backtest_path` bench harness idiom (`harness = false`).

use criterion::{criterion_group, criterion_main, Criterion};
use protopyne_chart_model::convert::ChartRow;
use protopyne_egui::{
    export_rows_csv, integration_lite_view, label_series, normalize_series, OscillatorKind,
};
use protopyne_kernels::market_context::MarketRegime;

fn bench_rows(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1;
            ChartRow {
                time: 1_700_000_000 + i as i64,
                open: close - 0.1,
                high: close + 0.2,
                low: close - 0.2,
                close,
                volume: 1_000 + i as u64,
            }
        })
        .collect()
}

fn bench_series(count: usize, base: f64) -> Vec<Option<f64>> {
    (0..count)
        .map(|i| Some(base + (i as f64 * 0.05).sin() * 40.0))
        .collect()
}

fn normalize_band(c: &mut Criterion) {
    let raw = bench_series(4_000, 60.0);
    c.bench_function("normalize_series_4k", |b| {
        b.iter(|| normalize_series(&raw, OscillatorKind::Rsi));
    });
}

fn quiet_labels(c: &mut Criterion) {
    let health = bench_series(4_000, 10.0);
    let volatility = bench_series(4_000, 1.0);
    let volume = bench_series(4_000, 1.5);
    c.bench_function("label_series_4k", |b| {
        b.iter(|| {
            label_series(MarketRegime::Ranging, &health, &volatility, &volume)
                .expect("bench slices align")
        });
    });
}

fn csv_export(c: &mut Criterion) {
    let rows = bench_rows(4_000);
    c.bench_function("export_rows_csv_4k", |b| {
        b.iter(|| export_rows_csv(&rows).expect("bench rows finite"));
    });
}

fn lite_pane(c: &mut Criterion) {
    let rows = bench_rows(500);
    c.bench_function("integration_lite_view_500", |b| {
        b.iter(|| integration_lite_view(&rows, 0.14, 0.5, 100.0).expect("bench pane builds"));
    });
}

criterion_group!(benches, normalize_band, quiet_labels, csv_export, lite_pane);
criterion_main!(benches);
