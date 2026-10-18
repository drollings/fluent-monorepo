//! Frame-budget baselines for the generic paint scaffolding: throughput
//! numbers only, never optimization targets and never gates.
//!
//! Each bench runs a pure headless op over synthetic rows/series (no CSV,
//! no network, no display): ribbon banding, pivot marking, viewport
//! windowing, and tick capping. Trading-domain ops (oscillator
//! normalization, quiet labels, export, parity panes) ride the terminal
//! crate's harness instead. What fails here as a timing regression never
//! reaches a gate — gates assert API shape (see `tests/budget.rs`),
//! wall-time is recorded in the bench note only. Mirrors the
//! `backtest_path` bench harness idiom (`harness = false`).

use criterion::{criterion_group, criterion_main, Criterion};
use fluent_egui::{axis_ticks, pivot_marks, ribbon_view, visible_rows};
use protopyne_chart_model::convert::ChartRow;

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

fn ribbon_band(c: &mut Criterion) {
    let fast = bench_series(4_000, 100.0);
    let slow = bench_series(4_000, 99.0);
    c.bench_function("ribbon_view_4k", |b| {
        b.iter(|| ribbon_view(&fast, &slow, 0.5).expect("bench alpha valid"));
    });
}

fn pivot_scan(c: &mut Criterion) {
    let fast = bench_series(4_000, 60.0);
    c.bench_function("pivot_marks_4k", |b| {
        b.iter(|| pivot_marks(&fast, 100.0).expect("bench threshold valid"));
    });
}

fn viewport_window(c: &mut Criterion) {
    let rows = bench_rows(4_000);
    c.bench_function("visible_rows_4k", |b| {
        b.iter(|| visible_rows(&rows, 0, 4_000));
    });
    c.bench_function("axis_ticks_capped", |b| {
        b.iter(|| axis_ticks(90.0, 120.0, 40, 8));
    });
}

criterion_group!(benches, ribbon_band, pivot_scan, viewport_window);
criterion_main!(benches);
