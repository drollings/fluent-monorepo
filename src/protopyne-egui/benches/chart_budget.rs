//! Trading-domain budget baselines for the terminal crate: throughput
//! numbers only, never optimization targets and never gates.
//!
//! Each bench runs a pure headless op over synthetic rows/series (no CSV,
//! no network, no display): oscillator normalization, quiet-label folding,
//! CSV export, one full IntegrationLite pane build, and the two sides of the
//! grant-tier read. What fails here as a timing regression never reaches a
//! gate — gates assert API shape (see `tests/budget.rs`), wall-time is
//! recorded in the bench note only. Mirrors the `backtest_path` bench
//! harness idiom (`harness = false`).

use chrono::{TimeZone, Utc};
use criterion::{criterion_group, criterion_main, Criterion};
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::convert::ChartRow;
use protopyne_egui::{
    export_rows_csv, integration_lite_sub_pane, integration_lite_view, label_series,
    normalize_series, GrantFormState, IntegrationLiteConfig, OscillatorKind,
};
use protopyne_kernels::market_context::MarketRegime;
use protopyne_strategy::{
    BreakerLimits, Direction, Grant, GrantBudgets, GrantPatch, GrantPermission, GrantScope,
    PortfolioThresholds, SignalThresholds, StructureKind, TradeThresholds,
};
use protopyne_types::Money;
use protopyne_view::MockGateway;

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

fn lite_sub_pane(c: &mut Criterion) {
    let rows = bench_rows(500);
    let config = IntegrationLiteConfig::new()
        .eit_alpha(0.14)
        .fill_alpha(0.5)
        .build();
    c.bench_function("integration_lite_sub_pane_500", |b| {
        b.iter(|| integration_lite_sub_pane(&rows, &config).expect("bench sub-pane builds"));
    });
}

/// A complete grant, the shape a live tier read serializes.
fn bench_grant() -> Grant {
    Grant::new()
        .id("bench-grant".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
                .directions(vec![Direction::Long])
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(4)
                .max_contracts(8)
                .max_loss(Money::from(500))
                .max_notional(Money::from(5_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.0)
                .min_activity_score(0.0)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(Money::from(10))
                .max_spread_width(Money::from(5))
                .min_dte_days(1)
                .max_dte_days(30)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(Money::from(10_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

/// The two sides of the gated tier read, recorded as observed numbers.
///
/// `grant_tier_payload_one_read` is what one ungated frame used to pay on the
/// gateway side (a full grant serialization plus one boundary string per
/// tier); `grant_form_frame_gated` is what a painted frame pays now, because
/// the interval keeps the payload off all but one frame in N. The pair is the
/// budget note for the interval — never a gate.
fn grant_tier_read(c: &mut Criterion) {
    let grant = bench_grant();
    let tiers = GrantPatch::default().field_names().to_vec();
    c.bench_function("grant_tier_payload_one_read", |b| {
        b.iter(|| {
            let document = serde_json::to_value(&grant).expect("bench grant serializes");
            tiers
                .iter()
                .map(|tier| document.get(*tier).map(serde_json::Value::to_string))
                .collect::<Vec<Option<String>>>()
        });
    });

    let gateway = MockGateway::new();
    let mut form = GrantFormState::from_schema(&GrantPatch::default().describe());
    c.bench_function("grant_form_frame_gated", |b| {
        b.iter(|| form.refresh_preview(&gateway));
    });
}

criterion_group!(
    benches,
    normalize_band,
    quiet_labels,
    csv_export,
    lite_pane,
    lite_sub_pane,
    grant_tier_read
);
criterion_main!(benches);
