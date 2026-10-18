//! Backtest decide-path and sweep fan-out baselines: throughput numbers only,
//! never optimization.
//!
//! `decide_path` runs one full `run_backtest` (propose → risk check → grant
//! evaluation → paper submit per bar) over a 64-bar synthetic tape;
//! `sweep_fanout` runs a four-variant `run_sweep` over the same tape;
//! `multi_fanout` runs a two-symbol `run_multi_backtest` over two 64-bar
//! tapes through the per-symbol pool. `native_vs_pine` times the same
//! 120-bar context workload twice: once through the linked Pine driver
//! (interpreter per-bar evaluation) and once through the native
//! `MtfStore` + `fetch_all` view. All execute the real paths on hermetic
//! inputs (no CSV, no network — the five Pine sources load once from the
//! versioned fixture tree, outside the measured loop), so the numbers
//! measure the decision stack, not file or clock behavior.
//! The smoke test (`runner::tests::bench_workload_smoke`) proves the same
//! workload executes outside this harness; what fails there never reaches
//! `cargo bench`.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use criterion::{criterion_group, criterion_main, Criterion};
use rust_decimal::Decimal;

use protopyne_server::{
    run_backtest, run_multi_backtest, run_sweep, BacktestConfig, HistorySource, MultiSymbolInput,
    RunnerConfig,
};
use protopyne_strategy::{
    elements::ExplicitStructure, BreakerLimits, Direction, FixedSignal, Grant, GrantBudgets,
    GrantPermission, GrantScope, PortfolioThresholds, RiskEngine, RiskLimits, SignalThresholds,
    Strategy, StructureKind, TradeThresholds,
};
use protopyne_types::{Bar, Money};
use protopyne_venue::PaperConfig;

fn money(value: i64) -> Money {
    Money::from(value)
}

fn rising_bars(count: usize) -> Vec<Bar> {
    let start: Decimal = Decimal::from(600);
    (0..count)
        .map(|step| {
            let close = start + Decimal::from(step as u64);
            Bar::new()
                .ts(Utc
                    .timestamp_opt(1_767_344_400 + step as i64 * 900, 0)
                    .unwrap())
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
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
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
    let grants: Vec<Grant> = (0..4)
        .map(|variant| grant(&format!("bench-{variant}")))
        .collect();
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

fn multi_leg(underlying: &str, bars: Vec<Bar>) -> MultiSymbolInput {
    MultiSymbolInput::new()
        .venue_config(
            BacktestConfig::new()
                .underlying(underlying.to_owned())
                .data_version(format!("{underlying}-bench"))
                .build(),
        )
        .runner_config(RunnerConfig::new(
            underlying.to_owned(),
            "ribbon".to_owned(),
            money(400),
            money(2_000),
            0,
            0,
        ))
        .history(HistorySource::Dynamic(bars))
        .build()
}

fn multi_fanout(criterion: &mut Criterion) {
    // Two 64-bar legs through the per-symbol pool: same decide path as
    // `decide_path`, fanned out and re-sorted by the harness.
    let spy = rising_bars(64);
    let qqq: Vec<Bar> = rising_bars(64)
        .into_iter()
        .map(|bar| {
            Bar::new()
                .ts(bar.ts)
                .o(bar.o + Decimal::from(100))
                .h(bar.h + Decimal::from(100))
                .l(bar.l + Decimal::from(100))
                .c(bar.c + Decimal::from(100))
                .volume(bar.volume)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let strategy = strategy();
    let risk = RiskEngine::new(RiskLimits::new().build());
    let grant = grant("bench-multi");
    let paper = PaperConfig::new().build();
    let runtime = runtime();
    criterion.bench_function("multi_fanout", |bencher| {
        bencher.iter(|| {
            runtime
                .block_on(run_multi_backtest(
                    &strategy,
                    &risk,
                    &grant,
                    paper.clone(),
                    vec![multi_leg("SPY", spy.clone()), multi_leg("QQQ", qqq.clone())],
                    3,
                ))
                .expect("bench multi completes")
        });
    });
}

/// Native-vs-Pine comparison over one shared workload. Needs the server
/// `pine` feature (the Pine leg drives the language runtime); the
/// throughput benches above run on lean kernels.
#[cfg(feature = "pine")]
fn native_vs_pine(criterion: &mut Criterion) {
    use protopyne_kernels::{
        fetch_all, parse_pine_source, parse_program, resolve_imports, InputSet, LibraryId,
        LinkedModule, Lookahead, MarketContextKey, ModuleInput, MtfStore, PineEngine,
        PineRuntimeConfig,
    };
    use protopyne_types::Timeframe;

    // Same 120-bar 1M workload both legs run: every context kernel is warm
    // by the last bar, so the timing covers steady-state evaluation, not
    // warmup. Linkage (file reads, parse, resolve) happens once outside
    // the loop — the loop measures per-run evaluation only.
    let bars: Vec<Bar> = (0..120)
        .map(|step| {
            let close = Decimal::from(600) + Decimal::from(step as u64);
            Bar::new()
                .ts(Utc
                    .timestamp_opt(1_767_344_400 + i64::from(step) * 60, 0)
                    .unwrap())
                .o(close)
                .h(close)
                .l(close)
                .c(close)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let data_root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs");
    let mut inputs: Vec<ModuleInput> = [
        ("libraries/_libUtil.pine", "libUtil", 12u64),
        ("libraries/_libBuffers.pine", "libBuffers", 6),
        ("libraries/_libBars.pine", "libBars", 12),
        ("libraries/_libDrawing.pine", "libDrawing", 61),
        ("libraries/_libApex.pine", "libApex", 88),
    ]
    .into_iter()
    .map(|(relpath, name, version)| {
        let text = std::fs::read_to_string(data_root.join(relpath)).expect("shipped library loads");
        ModuleInput::new(
            LibraryId {
                publisher: "showmethegrail".to_owned(),
                name: name.to_owned(),
                version,
            },
            parse_pine_source(&format!("showmethegrail/{name}"), &text),
            parse_program(&text).expect("shipped library parses"),
        )
    })
    .collect();
    let driver = "import showmethegrail/libUtil/12 as UTIL\n\
         import showmethegrail/libBuffers/6 as libBuffers\n\
         import showmethegrail/libBars/12 as BARS\n\
         import showmethegrail/libDrawing/61 as libDrawing\n\
         import showmethegrail/libApex/88 as libApex\n\
         var ctx = libApex.MarketContext.new()\n\
         if barstate.isfirst\n    ctx.init()\nctx.fetch()\n";
    inputs.push(ModuleInput::new(
        LibraryId {
            publisher: "local".to_owned(),
            name: "Driver".to_owned(),
            version: 1,
        },
        parse_pine_source("local/Driver", driver),
        parse_program(driver).expect("driver parses"),
    ));
    let graph = resolve_imports(inputs).expect("apex libraries resolve");
    let modules: Vec<LinkedModule> = graph
        .order()
        .iter()
        .map(|id| {
            let entry = graph.get(id).expect("ordered module resolves");
            let aliases = graph
                .alias_table(id)
                .unwrap_or_default()
                .into_iter()
                .map(|(alias, target)| (alias, target.to_string()))
                .collect::<std::collections::HashMap<String, String>>();
            LinkedModule {
                key: id.to_string(),
                program: entry.program.clone(),
                aliases,
            }
        })
        .collect();
    let now_ms = bars.last().expect("nonempty").ts.timestamp_millis() + 60_000;

    criterion.bench_function("native_vs_pine/pine_driver", |bencher| {
        bencher.iter(|| {
            let mut engine = PineEngine::new(
                PineRuntimeConfig::new()
                    .chart_symbol("SPY".to_owned())
                    .chart_period("1".to_owned())
                    .build(),
            );
            engine
                .run_linked(&modules, &bars, InputSet::new())
                .expect("bench Pine run evaluates")
        });
    });
    criterion.bench_function("native_vs_pine/native_fetch", |bencher| {
        bencher.iter(|| {
            let registry = protopyne_kernels::indicator::builtin_registry().expect("builtins");
            let mut store = MtfStore::new();
            store.set_base(&bars).expect("base loads");
            let key = MarketContextKey::new(Timeframe::M1);
            fetch_all(
                &mut store,
                &registry,
                &[key],
                &std::collections::HashMap::new(),
                now_ms,
                Lookahead::Off,
            )
            .expect("bench native fetch fills")
        });
    });
}

#[cfg(feature = "pine")]
criterion_group!(
    benches,
    decide_path,
    sweep_fanout,
    multi_fanout,
    native_vs_pine
);
#[cfg(not(feature = "pine"))]
criterion_group!(benches, decide_path, sweep_fanout, multi_fanout);
criterion_main!(benches);
