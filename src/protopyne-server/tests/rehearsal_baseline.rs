//! Rehearsal baseline pins.
//!
//! Each artefact here is the byte output of one rehearsal shape before any
//! pipeline widening: a single-symbol backtest, the same run through the
//! multi-symbol harness, the preview response body, and the paper decision
//! stream. Later changes re-assert these files, so "unchanged" is checkable
//! rather than argued.
//!
//! Record deliberately with `PROTOPYNE_RECORD=1`; a normal run only compares.
//! The mtime tripwire below is a reminder, not a gate: a golden written after
//! its test source means the pin moved without the test being revisited.
//!
//! The pinned reports name the economics arm in `fill_assumptions`, so a
//! reader can tell the run's configured constants from a chain-priced
//! package. That line replaced the bare `max loss <n> per proposal` line
//! (same slot, same meaning for the config arm); no metric moved.

use crate::common_fixtures::money;
use crate::common_fixtures::spy_history;
use protopyne_strategy::fixtures::grant_with;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use protopyne_strategy::{
    elements::{EconomicsPolicy, ExplicitStructure},
    Direction, FixedSignal, Grant, RiskEngine, RiskLimits, Strategy, StructureKind,
};
use protopyne_types::{Bar, ChainQuote, Money};
use protopyne_venue::{PaperConfig, PaperVenue};
use rust_decimal::Decimal;

use crate::backtest::{BacktestConfig, HistorySource};
use crate::rehearsal::StrategyKind;
use crate::report::ABSENT_CAPABILITIES;
use crate::runner::{run_backtest, run_multi_backtest, run_paper_decisions, MultiSymbolInput};
use crate::service::{PreviewParams, PreviewResponse, ServerConfig};

const BASELINE_DIR: &str = "baseline";

fn data_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name)
}

fn strategy() -> Arc<Strategy> {
    Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(ExplicitStructure {
            structure: StructureKind::Vertical,
        })),
    )
}

fn risk() -> RiskEngine {
    RiskEngine::new(RiskLimits::new().build())
}

fn grant(underlyings: &[&str]) -> Grant {
    // The shared fixture's shape; only the id, the scope's symbols, and the
    // signal floor a rehearsal tape must clear differ.
    grant_with(|grant| {
        grant.id = "rehearsal-baseline".to_owned();
        grant.scope.underlyings = underlyings.iter().map(|name| (*name).to_owned()).collect();
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
    })
}

fn venue_config(underlying: &str, data_version: &str) -> BacktestConfig {
    BacktestConfig::new()
        .underlying(underlying.to_owned())
        .data_version(data_version.to_owned())
        .build()
}

fn runner_config(underlying: &str) -> crate::runner::RunnerConfig {
    crate::runner::RunnerConfig::new(
        underlying.to_owned(),
        "ribbon".to_owned(),
        money("400"),
        money("2_000"),
        30,
        0,
    )
}

fn multi_input(underlying: &str, data_version: &str, bars: Vec<Bar>) -> MultiSymbolInput {
    MultiSymbolInput::new()
        .venue_config(venue_config(underlying, data_version))
        .runner_config(runner_config(underlying))
        .history(HistorySource::Dynamic(bars))
        .build()
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(BASELINE_DIR)
        .join(name)
}

/// Compare against a checked-in pin; `PROTOPYNE_RECORD=1` writes it.
fn check_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::create_dir_all(path.parent().expect("golden has a parent"))
            .expect("baseline directory exists");
        std::fs::write(&path, actual).expect("golden records");
        return;
    }
    let recorded = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("golden {} records first", path.display()));
    assert_eq!(actual, recorded, "{name} must match its baseline");
}

fn multi_canonical(run: &crate::runner::MultiBacktestRun) -> String {
    use std::fmt::Write as _;
    let mut out = run.manifest.to_canonical_json();
    for (index, leg) in run.runs.iter().enumerate() {
        let _ = write!(out, "\n--leg {index}--\n{}", leg.to_canonical_json());
    }
    out
}

#[tokio::test]
async fn single_symbol_backtest_matches_its_baseline() {
    let bars = spy_history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant(&["SPY"]),
        &venue_config("SPY", "spy-15m-v1"),
        &runner_config("SPY"),
        &bars,
        11,
    )
    .await
    .expect("rehearsal run");
    assert!(run.report.fills > 0, "the baseline must exercise a fill");
    check_golden("single_backtest.txt", &run.to_canonical_json());
}

#[tokio::test]
async fn multi_symbol_backtest_matches_its_baseline() {
    let spy = spy_history();
    let qqq = crate::csv::load_bars_csv(&data_path("SPY_5M.csv")).expect("second history loads");
    let run = run_multi_backtest(
        &strategy(),
        &risk(),
        &grant(&["SPY", "QQQ"]),
        PaperConfig::new().build(),
        vec![
            multi_input("SPY", "spy-15m-v1", spy),
            multi_input("QQQ", "qqq-5m-v1", qqq),
        ],
        11,
    )
    .await
    .expect("multi-symbol rehearsal run");
    check_golden("multi_backtest.txt", &multi_canonical(&run));
}

#[tokio::test]
async fn preview_response_matches_its_baseline() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = crate::service::serve(config, spy_history())
        .await
        .expect("test server boots");
    let client = reqwest::Client::new();
    let body = client
        .post(format!("http://{}/proposals/preview", handle.addr))
        .json(&PreviewParams {
            seed: 7,
            grant: None,
            strategy: StrategyKind::Rehearsal,
        })
        .send()
        .await
        .expect("preview reachable")
        .text()
        .await
        .expect("preview body");
    // Prove the body is the typed response, not an error envelope.
    let response: PreviewResponse =
        serde_json::from_str(&body).expect("preview body is a typed response");
    assert!(response.run.report.fills > 0);
    check_golden("preview_response.json", &body);
    handle.shutdown().await;
}

#[tokio::test]
async fn paper_decisions_match_their_baseline() {
    let bars = spy_history();
    let paper = PaperVenue::new(PaperConfig::new().build());
    let decisions = run_paper_decisions(
        &strategy(),
        &risk(),
        &grant(&["SPY"]),
        &paper,
        &runner_config("SPY"),
        &bars,
        11,
    )
    .await
    .expect("the paper baseline decides");
    assert!(
        decisions.iter().any(|decision| decision.submitted),
        "the paper baseline must exercise a fill"
    );
    // The merit and authorization verdicts overlap on a clean, budgeted run:
    // the baseline pins that equality, so a divergence is a declared change.
    assert!(
        decisions
            .iter()
            .all(|decision| decision.grant_merit == decision.grant_authorized),
        "a clean baseline run must authorize exactly what merit allows"
    );
    let body = serde_json::to_string(&decisions).expect("decisions serialize");
    check_golden("paper_decisions.json", &body);
}

/// A chainless run's report names its absent option data verbatim: the
/// chain-absent line is part of the before bytes, so a later milestone that
/// adds chain facts shows up as a declared delta.
#[tokio::test]
async fn chainless_report_names_its_absence() {
    let bars = spy_history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant(&["SPY"]),
        &venue_config("SPY", "spy-15m-v1"),
        &runner_config("SPY"),
        &bars,
        11,
    )
    .await
    .expect("rehearsal run");
    assert!(
        run.report
            .data_limits
            .iter()
            .any(|line| line == ABSENT_CAPABILITIES),
        "a chainless report keeps the verbatim absence line, got {:?}",
        run.report.data_limits
    );
}

/// The batch chain-derived before-state: with no chain source threaded, the
/// chain arm cannot price a package, so every bar skips and nothing reaches
/// the venue. Today the refusal lands at strategy compose time (the
/// selection carries no economics), before the runner's leg gate; a later
/// milestone that threads a source turns this skip into a covered run — a
/// declared delta against this pin, not a silent behaviour swap.
#[tokio::test]
async fn chain_derived_batch_without_source_skips_every_bar() {
    let bars = spy_history();
    let config = runner_config("SPY").with_economics(EconomicsPolicy::ChainDerived);
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant(&["SPY"]),
        &venue_config("SPY", "spy-15m-v1"),
        &config,
        &bars,
        11,
    )
    .await
    .expect("chain-derived run records its skips");
    assert!(!run.decisions.is_empty(), "the fixture must decide");
    assert!(
        run.decisions.iter().all(|decision| !decision.submitted),
        "nothing may reach the venue without a chain source"
    );
    for decision in &run.decisions {
        let reason = decision.skipped.as_deref().unwrap_or("<reached the gates>");
        assert!(
            reason.contains("chain-derived"),
            "bar {} must name the chain-derived gap, got {reason:?}",
            decision.bar_index
        );
    }
}

/// A golden newer than this test source means the pin moved without the test
/// being revisited. It reports as a failure so the reviewer sees it; recording
/// writes the golden, so the following run needs the source touched again.
#[test]
fn baseline_pins_are_regenerated_only_deliberately() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rehearsal_baseline.rs");
    let source_time = std::fs::metadata(&source)
        .expect("test source exists")
        .modified()
        .expect("source mtime");
    for name in [
        "single_backtest.txt",
        "multi_backtest.txt",
        "preview_response.json",
        "paper_decisions.json",
    ] {
        let path = golden_path(name);
        let golden_time = std::fs::metadata(&path)
            .unwrap_or_else(|_| panic!("golden {} records first", path.display()))
            .modified()
            .expect("golden mtime");
        assert!(
            golden_time <= source_time,
            "{} is newer than its test source: revisit the pin deliberately",
            path.display()
        );
    }
}

/// A flat synthetic ladder around a ~600 touch, one bar a minute from
/// 2026-06-01 — no fixture file, no calendar surprises.
fn ladder_bars(count: usize) -> Vec<Bar> {
    (0..count)
        .map(|index| {
            let price = Decimal::from(600 + index as i64);
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 6, 1, 12, index as u32, 0)
                    .unwrap())
                .o(price)
                .h(price)
                .l(price)
                .c(price)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// A 590–610 call ladder stamped at every bar. One-tick books around the
/// ladder's mid: mids equal touches, so a package clears the economics
/// tolerance at the venue's touch fill.
fn tick_ladder(bars: &[Bar], expiry: chrono::NaiveDate) -> Vec<ChainQuote> {
    use protopyne_types::{OptionContract, OptionKind};

    let mut quotes = Vec::new();
    for bar in bars {
        for (strike, bid, ask) in [
            (595, "5.499", "5.501"),
            (600, "2.499", "2.501"),
            (605, "1.499", "1.501"),
            (610, "0.499", "0.501"),
        ] {
            quotes.push(
                ChainQuote::new()
                    .contract(
                        OptionContract::new()
                            .underlying("SPY".to_owned())
                            .expiry(expiry)
                            .strike(Money::new(Decimal::from(strike)))
                            .kind(OptionKind::Call)
                            .build(),
                    )
                    .bid(bid.parse().expect("bid"))
                    .ask(ask.parse().expect("ask"))
                    .bid_size(50)
                    .ask_size(50)
                    .timestamp(bar.ts)
                    .build(),
            );
        }
    }
    quotes
}

/// A batch run with a threaded chain source under `RequireChains` books the
/// chain-derived economics: covered bars flow into two-leg fills through the
/// same shared body interactive replay steps, and the report names the
/// chain coverage instead of the no-chains absence. This is the covered
/// form of the skip-everything pin above — a declared delta, not a silent
/// swap.
#[tokio::test]
async fn chain_backed_batch_books_chain_derived_economics() {
    use protopyne_strategy::LegSelector;

    let bars = ladder_bars(8);
    let expiry = chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid expiry");
    let venue = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("chain-batch-v1".to_owned())
        .chain_history(tick_ladder(&bars, expiry))
        .require_chains(true)
        .build();
    let strategy = Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(LegSelector::new().build())),
    );
    let config = runner_config("SPY").with_economics(EconomicsPolicy::ChainDerived);
    let run = run_backtest(
        &strategy,
        &risk(),
        &grant(&["SPY"]),
        &venue,
        &config,
        &bars,
        11,
    )
    .await
    .expect("chain-backed run records its fills");
    let submitted: Vec<_> = run.decisions.iter().filter(|d| d.submitted).collect();
    assert!(
        !submitted.is_empty(),
        "covered bars flow: {:?}",
        run.decisions
            .iter()
            .map(|d| d.skipped.clone())
            .collect::<Vec<_>>()
    );
    for decision in &submitted {
        assert_eq!(
            decision.legs.len(),
            2,
            "a covered bar books the package, never a single leg"
        );
    }
    assert!(
        run.report
            .data_limits
            .iter()
            .any(|line| line.starts_with("option-chain data:")),
        "the report names the chain coverage, got {:?}",
        run.report.data_limits
    );
    assert!(
        !run.report
            .data_limits
            .iter()
            .any(|line| line == ABSENT_CAPABILITIES),
        "the no-chains clause drops once chains price the run, got {:?}",
        run.report.data_limits
    );
    assert!(
        run.report
            .data_limits
            .iter()
            .any(|line| line == crate::report::ABSENT_CAPABILITIES_WITH_CHAINS),
        "every other absence stands, got {:?}",
        run.report.data_limits
    );
}

/// `RequireChains` with no chain history refuses before the first bar: a
/// screened run that cannot screen is a misconfiguration, never an
/// all-skip run that reads as quiet.
#[tokio::test]
async fn require_chains_without_history_refuses_before_the_first_bar() {
    let bars = spy_history();
    let venue = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("chainless-screened-v1".to_owned())
        .require_chains(true)
        .build();
    let config = runner_config("SPY").with_economics(EconomicsPolicy::ChainDerived);
    let refused = run_backtest(
        &strategy(),
        &risk(),
        &grant(&["SPY"]),
        &venue,
        &config,
        &bars,
        11,
    )
    .await;
    let reason = refused
        .expect_err("a sourceless screen refuses")
        .to_string();
    assert!(
        reason.contains("chain"),
        "the refusal names the missing chain, got: {reason}"
    );
}

/// The DTE gate judges the instrument, not the request: 7 requested days
/// against a ladder whose nearest listed expiry is 46 days out, under a grant
/// capped at 30, selects nothing. Selection refuses naming the window rather
/// than proposing an expiry the authority will refuse anyway.
#[tokio::test]
async fn an_expiry_past_the_grants_window_skips_every_bar() {
    use protopyne_strategy::LegSelector;

    let bars = ladder_bars(6);
    let expiry = chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid expiry");
    let venue = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("chain-dte-window-v1".to_owned())
        .chain_history(tick_ladder(&bars, expiry))
        .require_chains(true)
        .build();
    let strategy = Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(LegSelector::new().build())),
    );
    let mut grant = grant(&["SPY"]);
    grant.trade.max_dte_days = 30;
    let config = crate::runner::RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money("400"),
        money("2_000"),
        7,
        0,
    )
    .with_economics(EconomicsPolicy::ChainDerived);
    let run = run_backtest(&strategy, &risk(), &grant, &venue, &config, &bars, 11)
        .await
        .expect("the run records its skips");
    let skipped: Vec<_> = run
        .decisions
        .iter()
        .map(|decision| decision.skipped.clone().unwrap_or_default())
        .collect();
    assert!(
        skipped
            .iter()
            .all(|reason| reason.contains("past the 30-day maximum")),
        "every bar names the expiry window, got {skipped:?}"
    );
    assert!(
        run.decisions.iter().all(|decision| !decision.submitted),
        "nothing outside the window reaches the venue"
    );
}

/// The report names the economics arm that ran: a reader can tell whether the
/// checked numbers were the structure's own or the run's configured
/// constants.
#[tokio::test]
async fn the_report_names_the_economics_arm() {
    use protopyne_strategy::LegSelector;

    let bars = ladder_bars(6);
    let venue = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("chain-arm-v1".to_owned())
        .chain_history(tick_ladder(
            &bars,
            chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid expiry"),
        ))
        .require_chains(true)
        .build();
    let chained_strategy = Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(LegSelector::new().build())),
    );
    let chained = run_backtest(
        &chained_strategy,
        &risk(),
        &grant(&["SPY"]),
        &venue,
        &runner_config("SPY").with_economics(EconomicsPolicy::ChainDerived),
        &bars,
        11,
    )
    .await
    .expect("the chain-derived run records its fills");
    assert!(
        chained
            .report
            .fill_assumptions
            .iter()
            .any(|line| line.contains("chain-derived")),
        "the chained run names its arm, got {:?}",
        chained.report.fill_assumptions
    );
    let configured = run_backtest(
        &strategy(),
        &risk(),
        &grant(&["SPY"]),
        &BacktestConfig::new()
            .underlying("SPY".to_owned())
            .data_version("config-arm-v1".to_owned())
            .build(),
        &runner_config("SPY"),
        &bars,
        11,
    )
    .await
    .expect("the config-supplied run records its fills");
    assert!(
        configured
            .report
            .fill_assumptions
            .iter()
            .any(|line| line.contains("config-supplied")),
        "the configured run names its arm, got {:?}",
        configured.report.fill_assumptions
    );
    assert!(
        !configured
            .report
            .fill_assumptions
            .iter()
            .any(|line| line.contains("chain-derived")),
        "one arm names itself, never both"
    );
}

/// A chain-backed run marks its option legs from the run's own chain: the
/// report names the `ChainMid` basis, every booked bucket carries it, and the
/// open package's unrealized is a package-sized number (fees and slippage
/// against a near-1.00 entry) rather than the underlying's ~600 price against
/// that entry, which the tape proxy would have reported as thousands.
#[tokio::test]
async fn a_chain_backed_run_marks_its_legs_from_the_chain() {
    use crate::marks::MarkBasis;
    use protopyne_strategy::LegSelector;

    // Five bars so every fill selects the same strike pair (a single book's
    // entry average and its mark then speak about the same package).
    let bars = ladder_bars(5);
    let venue = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("chain-marks-v1".to_owned())
        .chain_history(tick_ladder(
            &bars,
            chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid expiry"),
        ))
        .require_chains(true)
        .build();
    let chained = Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(LegSelector::new().build())),
    );
    let run = run_backtest(
        &chained,
        &risk(),
        &grant(&["SPY"]),
        &venue,
        &runner_config("SPY").with_economics(EconomicsPolicy::ChainDerived),
        &bars,
        11,
    )
    .await
    .expect("the chain-backed run records its fills");
    assert_eq!(
        run.report.mark_basis,
        MarkBasis::ChainMid,
        "the report names the basis its marks came from"
    );
    assert!(
        run.report
            .fill_assumptions
            .iter()
            .any(|line| line.contains("chain mid")),
        "the assumption line names the chain mid, got {:?}",
        run.report.fill_assumptions
    );
    assert!(
        !run.report.greeks_note.contains("no option-chain feed")
            && run.report.greeks_note.contains("chain data"),
        "a chained run never claims it held no chain: {}",
        run.report.greeks_note
    );
    let portfolio = run.report.portfolio.as_ref().expect("a portfolio fold");
    let buckets: Vec<_> = portfolio
        .portfolios
        .iter()
        .flat_map(|book| &book.buckets)
        .collect();
    assert!(!buckets.is_empty(), "the run booked fills");
    for bucket in buckets {
        assert_eq!(
            bucket.mark_basis,
            MarkBasis::ChainMid,
            "a booked package carries the basis it marks from"
        );
        let unrealized = bucket.pnl_unrealised.expect("a marked book");
        assert!(
            unrealized.as_decimal().abs() < Decimal::from(10),
            "a package marks against package prices, not the ~600 underlying \
             close the tape proxy would have used: {unrealized}"
        );
    }

    // The same run without a chain keeps the fill-price basis and names the
    // absence instead.
    let chainless = run_backtest(
        &strategy(),
        &risk(),
        &grant(&["SPY"]),
        &BacktestConfig::new()
            .underlying("SPY".to_owned())
            .data_version("no-marks-v1".to_owned())
            .build(),
        &runner_config("SPY"),
        &bars,
        11,
    )
    .await
    .expect("the chainless run records its fills");
    assert_eq!(chainless.report.mark_basis, MarkBasis::FillPrice);
    assert_eq!(
        chainless.report.greeks_note,
        "greeks-over-time unavailable: prototype has no option-chain feed",
        "the chainless wording stays verbatim"
    );
    assert!(
        chainless
            .report
            .data_limits
            .iter()
            .any(|line| line == ABSENT_CAPABILITIES),
        "the chainless run keeps the verbatim absence line, got {:?}",
        chainless.report.data_limits
    );
}

/// The momentum composition over a fixture: deterministic decisions that
/// book defined-risk packages once a facts producer is observed.
///
/// The batch loops thread no facts (fail-closed sizing holds them at zero),
/// so the run drives the shared bar body directly with an observed buying
/// power — exactly what the live loop reads from its producer every bar.
mod momentum {
    use protopyne_strategy::{
        Grant, GrantLedger, PortfolioFacts, RiskEngine, RiskLimits, SignalConfig,
    };
    use protopyne_types::{Bar, ChainQuote, Money};
    use protopyne_venue::{Journal, PaperConfig, Venue};
    use rust_decimal::Decimal;

    use crate::backtest::{BacktestConfig, BacktestVenue};
    use crate::rehearsal::{select_strategy, StrategyKind};
    use crate::replay_session::{advance_bar, BarAdvance, BarQuote, ChainPolicy};
    use crate::runner::{BarDecision, BarHealth, RunnerConfig};

    use super::{grant, ladder_bars, money, runner_config, tick_ladder};

    fn buying_power_facts() -> PortfolioFacts {
        PortfolioFacts::observed(money("0"), None, Some(money("100_000")), None)
    }

    fn roomy_grant() -> Grant {
        let mut grant = grant(&["SPY"]);
        grant.trade.max_loss_per_trade = money("1_000_000");
        grant
    }

    fn roomy_risk() -> RiskEngine {
        RiskEngine::new(
            RiskLimits::new()
                .max_loss_per_trade(1_000_000.0)
                .max_notional(100_000_000.0)
                .build(),
        )
    }

    fn momentum_config() -> RunnerConfig {
        use protopyne_strategy::elements::EconomicsPolicy;
        runner_config("SPY").with_economics(EconomicsPolicy::ChainDerived)
    }

    async fn drive(
        bars: &[Bar],
        chain_history: Vec<ChainQuote>,
        facts: PortfolioFacts,
    ) -> Vec<BarDecision> {
        let venue_config = BacktestConfig::new()
            .underlying("SPY".to_owned())
            .data_version("momentum-v1".to_owned())
            .chain_history(chain_history)
            .require_chains(true)
            .build();
        let venue = BacktestVenue::new(
            venue_config.clone(),
            bars.to_vec(),
            PaperConfig::new().build(),
        )
        .expect("venue builds");
        let strategy = select_strategy(StrategyKind::Momentum).expect("momentum selects");
        let risk = roomy_risk();
        let grant = roomy_grant();
        let runner = momentum_config();
        let timeframe =
            protopyne_types::Timeframe::infer(bars).unwrap_or(protopyne_types::Timeframe::D1);
        let mut signal = strategy
            .bind(&SignalConfig::new(timeframe))
            .expect("the momentum signal binds");
        let ledger = GrantLedger::new(grant);
        let journal = Journal::ephemeral();
        let chain = venue.chain_source();
        let mut closes = Vec::with_capacity(bars.len());
        let mut skipped_warmup = 0u64;
        let mut book = crate::replay_session::PositionBook::new();
        let mut corporate_cursor = 0usize;
        let mut decisions = Vec::with_capacity(bars.len());
        for (index, bar) in bars.iter().enumerate() {
            decisions.push(
                advance_bar(
                    BarAdvance {
                        strategy: &strategy,
                        signal: signal.as_mut(),
                        risk: &risk,
                        ledger: &ledger,
                        venue: &venue,
                        journal: &journal,
                        permit: None,
                        quote_config: &venue_config,
                        chains: ChainPolicy::RequireChains,
                        chain: chain.as_deref(),
                        bars,
                        timeframe,
                        closes: &mut closes,
                        skipped_warmup: &mut skipped_warmup,
                        book: &mut book,
                        runner_config: &runner,
                        seed: 7,
                        corporate: &venue_config.corporate_actions,
                        corporate_cursor: &mut corporate_cursor,
                        quote: BarQuote::Synthesized,
                        health: BarHealth::rehearsed(),
                        facts: Some(facts),
                        gate: None,
                    },
                    index,
                    bar,
                )
                .await,
            );
        }
        decisions
    }

    fn trend_bars() -> Vec<Bar> {
        ladder_bars(40)
    }

    fn expiry() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid expiry")
    }

    /// A dense call ladder covering the whole trend tape: one-tick books at
    /// every five-point strike, so the selector always finds strikes beyond
    /// the touch however far the tape runs.
    fn wide_ladder(bars: &[Bar], expiry: chrono::NaiveDate) -> Vec<ChainQuote> {
        use protopyne_types::{OptionContract, OptionKind};

        let mut quotes = Vec::new();
        for bar in bars {
            for strike in (590..=650).step_by(5) {
                let mid = f64::from(650 - strike).mul_add(0.5, 0.5);
                let bid: Money = format!("{:.3}", mid - 0.001).parse().expect("bid parses");
                let ask: Money = format!("{:.3}", mid + 0.001).parse().expect("ask parses");
                quotes.push(
                    ChainQuote::new()
                        .contract(
                            OptionContract::new()
                                .underlying("SPY".to_owned())
                                .expiry(expiry)
                                .strike(Money::new(Decimal::from(strike)))
                                .kind(OptionKind::Call)
                                .build(),
                        )
                        .bid(bid)
                        .ask(ask)
                        .bid_size(50)
                        .ask_size(50)
                        .timestamp(bar.ts)
                        .build(),
                );
            }
        }
        quotes
    }

    /// A rising tape books chain-priced verticals: every submitted decision
    /// fills a two-leg package, and driving the same bars twice decides
    /// identically.
    #[tokio::test]
    async fn the_momentum_composition_books_defined_risk_packages() {
        let bars = trend_bars();
        let history = wide_ladder(&bars, expiry());
        let facts = buying_power_facts();
        let first = drive(&bars, history.clone(), facts).await;
        let submitted: Vec<_> = first.iter().filter(|d| d.submitted).collect();
        assert!(
            !submitted.is_empty(),
            "an observed buying power funds entries on a rising tape"
        );
        for decision in &submitted {
            assert_eq!(
                decision.legs.len(),
                2,
                "every booked fill is a two-leg package, got {:?}",
                decision.legs
            );
            assert!(
                decision.fill_price.is_some(),
                "a booked package names its fill price"
            );
        }
        let second = drive(&bars, history, facts).await;
        assert_eq!(
            first, second,
            "the same bars decide identically across drives"
        );
    }

    /// The zero-conviction control at run level: a flat tape emits no conviction, so
    /// the grant's conviction floor denies every bar and nothing books —
    /// even with buying power observed.
    #[tokio::test]
    async fn a_flat_tape_books_nothing() {
        let bars: Vec<Bar> = (0..40)
            .map(|_| {
                let price = Decimal::from(600);
                Bar::new()
                    .ts(
                        chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 6, 1, 12, 0, 0)
                            .unwrap(),
                    )
                    .o(price)
                    .h(price)
                    .l(price)
                    .c(price)
                    .volume(1_000)
                    .maybe_adj(None)
                    .build()
            })
            .collect();
        let decisions = drive(&bars, tick_ladder(&bars, expiry()), buying_power_facts()).await;
        assert!(
            decisions.iter().all(|d| !d.submitted),
            "a conviction-free tape submits nothing"
        );
    }

    /// Selection is configuration: the rehearsal default resolves to the
    /// fixed-signal composition, momentum to the kernel one, and the
    /// default call path is untouched.
    #[test]
    fn selection_names_the_installed_elements() {
        let rehearsal = select_strategy(StrategyKind::Rehearsal).expect("rehearsal selects");
        assert_eq!(rehearsal.signal_name(), "fixed");
        assert_eq!(rehearsal.sizing_name(), "fixed");
        assert_eq!(rehearsal.loss_name(), "hold");
        assert!(
            !rehearsal.has_exits(),
            "the rehearsal composition holds: no exit pass may touch its bars"
        );
        let momentum = select_strategy(StrategyKind::Momentum).expect("momentum selects");
        assert_eq!(momentum.signal_name(), "kernel");
        assert_eq!(momentum.sizing_name(), "buying-power");
        assert_eq!(momentum.loss_name(), "set");
        assert!(momentum.has_exits(), "momentum installs a real loss set");
    }
}

#[test]
fn every_strategy_kind_resolves_through_the_composition_catalog() {
    use crate::rehearsal::{rehearsal_strategy, select_strategy};
    use protopyne_strategy::slots::composition::{strategies, strategy_ids};

    // Every kind resolves; the shim adds error mapping, not behavior.
    assert!(select_strategy(StrategyKind::Rehearsal).is_ok());
    assert!(select_strategy(StrategyKind::Momentum).is_ok());
    // Every catalog row builds, so no row can orphan outside the kinds.
    for id in strategy_ids() {
        assert!(
            strategies().build(id, &serde_json::json!({})).is_ok(),
            "{id} builds through the catalog"
        );
    }
    // The rehearsal entry point covers every structure arm.
    for structure in [
        None,
        Some(StructureKind::Single),
        Some(StructureKind::Vertical),
        Some(StructureKind::Calendar),
    ] {
        assert!(
            rehearsal_strategy(structure).is_ok(),
            "rehearsal builds for {structure:?}"
        );
    }
    // Unknown structures refuse at the boundary, not downstream.
    assert!(strategies()
        .build("rehearsal", &serde_json::json!({"structure": "condor-ish"}))
        .is_err());
}
