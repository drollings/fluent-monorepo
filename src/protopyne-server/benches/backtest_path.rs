//! Backtest decide-path and sweep fan-out baselines: throughput numbers only,
//! never optimization.
//!
//! `decide_path` runs one full `run_backtest` (propose → risk check → grant
//! evaluation → paper submit per bar) over a 64-bar synthetic tape;
//! `sweep_fanout` runs a four-variant `run_sweep` over the same tape. Both
//! execute the real scoring path on hermetic inputs (no CSV, no network),
//! so the numbers measure the decision stack, not file or clock behavior.
//! The smoke test (`runner::tests::bench_workload_smoke`) proves the same
//! workload executes outside this harness; what fails there never reaches
//! `cargo bench`.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use criterion::{Criterion, criterion_group, criterion_main};
use rust_decimal::Decimal;

use protopyne_server::{
    BacktestConfig, RunnerConfig, run_backtest, run_sweep,
};
use protopyne_strategy::{
    BreakerLimits, Direction, FixedSignal, Grant, GrantBudgets, GrantPermission, GrantScope,
    PortfolioThresholds, RiskEngine, RiskLimits, SignalThresholds, Strategy, StructureKind,
    TradeThresholds,
    elements::ExplicitStructure,
};
use protopyne_types::{Bar, Money};

fn money(value: i64) -> Money {
    Money::from(value)
}

fn rising_bars(count: usize) -> Vec<Bar> {
    let start: Decimal = Decimal::from(600);
    (0..count)
        .map(|step| {
            let close = start + Decimal::from(step as u64);
            Bar::new()
                .ts(Utc.timestamp_opt(1_767_344_400 + step as i64 * 900, 0).unwrap())
                .o(close)
                .h(close)
                .l(close)
                .c(close)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn strategy() -> Arc<Strategy> {
    Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("bench signal valid"),
        ))
        .with_structure(Arc::new(ExplicitStructure {
            structure: StructureKind::Vertical,
        })),
    )
}

fn grant(id: &str) -> Grant {
    Grant::new()
        .id(id.to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission { entries: true, exits: true, rolls: false })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(1_000)
                .max_contracts(10_000)
                .max_loss(money(1_000_000))
                .max_notional(money(100_000_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.5)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(money(1_000))
                .max_spread_width(money(50))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

fn configs() -> (BacktestConfig, RunnerConfig) {
    let venue = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("bench-synth".to_owned())
        .build();
    let runner = RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money(400),
        money(2_000),
        0,
        0,
    );
    (venue, runner)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .build()
        .expect("bench runtime builds")
}

fn decide_path(criterion: &mut Criterion) {
    let bars = rising_bars(64);
    let (venue, runner) = configs();
    let strategy = strategy();
    let risk = RiskEngine::new(RiskLimits::new().build());
    let grant = grant("bench");
    let runtime = runtime();
    criterion.bench_function("decide_path", |bencher| {
        bencher.iter(|| {
            runtime
                .block_on(run_backtest(
                    &strategy, &risk, &grant, &venue, &runner, &bars, 3,
                ))
                .expect("bench run completes")
        });
    });
}

fn sweep_fanout(criterion: &mut Criterion) {
    let bars = rising_bars(64);
    let (venue, runner) = configs();
    let strategy = strategy();
    let limits = RiskLimits::new().build();
    let grants: Vec<Grant> = (0..4).map(|variant| grant(&format!("bench-{variant}"))).collect();
    let runtime = runtime();
    criterion.bench_function("sweep_fanout", |bencher| {
        bencher.iter(|| {
            runtime
                .block_on(run_sweep(
                    Arc::clone(&strategy),
                    limits.clone(),
                    &venue,
                    &runner,
                    &bars,
                    grants.clone(),
                    3,
                ))
                .expect("bench sweep completes")
        });
    });
}

criterion_group!(benches, decide_path, sweep_fanout);
criterion_main!(benches);
