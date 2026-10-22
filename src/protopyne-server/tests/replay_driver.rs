use std::sync::Arc;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::{
    elements::ExplicitStructure, Direction, FixedSignal, Grant, GrantBudgets, GrantPermission,
    GrantScope, PortfolioThresholds, RiskEngine, RiskLimits, SignalThresholds, Strategy,
    StructureKind, TradeThresholds,
};
use protopyne_types::{Bar, Money};
use protopyne_venue::PaperConfig;

use crate::backtest::{BacktestConfig, BacktestVenue};
use crate::replay_session::{ChainPolicy, ReplayDriver, ReplayRegistry, ReplaySession};
use crate::runner::RunnerConfig;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn synthetic_bars(count: usize) -> Vec<Bar> {
    (0..count)
        .map(|index| {
            let price = Decimal::from(100 + index as i64);
            Bar::new()
                .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
                    + chrono::Duration::minutes(index as i64))
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

fn single_strategy() -> Strategy {
    Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
    ))
    .with_structure(Arc::new(ExplicitStructure {
        structure: StructureKind::Single,
    }))
}

fn risk_engine() -> RiskEngine {
    RiskEngine::new(RiskLimits::new().build())
}

fn single_grant() -> Grant {
    Grant::new()
        .id("replay-driver-test".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Single])
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
                .min_conviction(0.0)
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
        .breakers(
            protopyne_strategy::BreakerLimits::new()
                .max_consecutive_errors(3)
                .build(),
        )
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

fn venue_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("replay-driver-test-v1".to_owned())
        .build()
}

fn runner_config() -> RunnerConfig {
    RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money(400),
        money(2_000),
        30,
        0,
    )
}

/// The owned driver and the borrowed session step identically: one body.
#[tokio::test]
async fn driver_and_session_agree_bar_for_bar() {
    let bars = synthetic_bars(10);
    let venue = BacktestVenue::new(venue_config(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    let strategy = single_strategy();
    let risk = risk_engine();
    let grant = single_grant();
    let mut session = ReplaySession::start(
        &strategy,
        &risk,
        &grant,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        &bars,
        5,
    )
    .expect("session starts");
    let mut driver = ReplayDriver::start(
        single_strategy(),
        risk_engine(),
        single_grant(),
        venue_config(),
        runner_config(),
        bars.clone(),
        5,
        0,
    )
    .expect("driver starts");
    assert_eq!(driver.cursor(), 0, "a zero start_bar stands at the head");
    let mut session_run = Vec::new();
    let mut driver_run = Vec::new();
    while let Some(decision) = session.step().await {
        session_run.push(decision);
    }
    while let Some(decision) = driver.step().await {
        driver_run.push(decision);
    }
    assert_eq!(driver_run, session_run, "owned and borrowed runs agree");
    assert_eq!(driver.cursor(), driver.total_bars());
    assert!(driver.step().await.is_none(), "stepping past the end holds");
}

/// A mid-history start pre-fills signal context: verdicts from `start_bar`
/// on match the session's verdicts at the same indices.
#[tokio::test]
async fn start_bar_prefills_context_without_deciding_it() {
    let bars = synthetic_bars(10);
    let venue = BacktestVenue::new(venue_config(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    let strategy = single_strategy();
    let risk = risk_engine();
    let grant = single_grant();
    let mut session = ReplaySession::start(
        &strategy,
        &risk,
        &grant,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        &bars,
        5,
    )
    .expect("session starts");
    let mut full = Vec::new();
    while let Some(decision) = session.step().await {
        full.push(decision);
    }
    let mut driver = ReplayDriver::start(
        single_strategy(),
        risk_engine(),
        single_grant(),
        venue_config(),
        runner_config(),
        bars,
        5,
        4,
    )
    .expect("driver starts mid-history");
    assert_eq!(driver.cursor(), 4);
    assert!(driver.decisions().is_empty(), "context bars decide nothing");
    let mut partial = Vec::new();
    while let Some(decision) = driver.step().await {
        partial.push(decision);
    }
    assert_eq!(
        partial,
        full[4..],
        "verdicts from the start bar match the full run"
    );
}

/// `step_n` stops at the end of history and answers what it decided.
#[tokio::test]
async fn step_n_stops_at_the_end() {
    let bars = synthetic_bars(5);
    let mut driver = ReplayDriver::start(
        single_strategy(),
        risk_engine(),
        single_grant(),
        venue_config(),
        runner_config(),
        bars,
        5,
        0,
    )
    .expect("driver starts");
    let decided = driver.step_n(100).await;
    assert_eq!(decided.len(), 5, "an oversize step stops at the end");
    assert_eq!(driver.decisions().len(), 5);
    assert!(
        driver.step_n(3).await.is_empty(),
        "stepping past the end decides nothing"
    );
}

/// Construction fails closed: empty history and a start past the last bar.
#[test]
fn start_refuses_empty_history_and_past_start_bar() {
    let empty: Vec<Bar> = Vec::new();
    assert!(
        ReplayDriver::start(
            single_strategy(),
            risk_engine(),
            single_grant(),
            venue_config(),
            runner_config(),
            empty,
            1,
            0,
        )
        .is_err(),
        "empty history refuses"
    );
    let bars = synthetic_bars(4);
    assert!(
        ReplayDriver::start(
            single_strategy(),
            risk_engine(),
            single_grant(),
            venue_config(),
            runner_config(),
            bars,
            1,
            5,
        )
        .is_err(),
        "a start past the last bar refuses"
    );
}

/// The registry replaces a slot on restart and releases it on remove.
#[tokio::test]
async fn registry_replaces_on_restart_and_releases_on_remove() {
    let mut registry = ReplayRegistry::new();
    assert!(
        registry.session("terminal").is_none(),
        "a fresh slot is empty"
    );
    registry.start(
        "terminal".to_owned(),
        ReplayDriver::start(
            single_strategy(),
            risk_engine(),
            single_grant(),
            venue_config(),
            runner_config(),
            synthetic_bars(6),
            1,
            0,
        )
        .expect("first driver starts"),
    );
    registry
        .session_mut("terminal")
        .expect("slot is occupied")
        .step_n(2)
        .await;
    assert_eq!(
        registry.session("terminal").expect("slot reads").cursor(),
        2
    );
    // A restart replaces, never merges: the cursor returns to the head.
    registry.start(
        "terminal".to_owned(),
        ReplayDriver::start(
            single_strategy(),
            risk_engine(),
            single_grant(),
            venue_config(),
            runner_config(),
            synthetic_bars(6),
            1,
            0,
        )
        .expect("second driver starts"),
    );
    assert_eq!(
        registry.session("terminal").expect("slot reads").cursor(),
        0
    );
    // Slots are independent.
    registry.start(
        "second".to_owned(),
        ReplayDriver::start(
            single_strategy(),
            risk_engine(),
            single_grant(),
            venue_config(),
            runner_config(),
            synthetic_bars(6),
            1,
            0,
        )
        .expect("other driver starts"),
    );
    assert!(registry.remove("terminal"), "an occupied slot releases");
    assert!(
        registry.session("terminal").is_none(),
        "the slot is empty again"
    );
    assert!(
        !registry.remove("terminal"),
        "releasing an empty slot reports it"
    );
    assert!(
        registry.session("second").is_some(),
        "the other slot is untouched"
    );
}

/// The screen with no chains installed: a vertical driver refuses every
/// spread under `RequireChains` with the byte-identical note, submitting
/// nothing — the pre-lift pin, kept.
#[tokio::test]
async fn uncovered_bars_keep_the_unchanged_refusal() {
    let bars = synthetic_bars(8);
    let mut driver = ReplayDriver::start(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(ExplicitStructure {
            structure: StructureKind::Vertical,
        })),
        risk_engine(),
        single_grant(),
        venue_config(),
        RunnerConfig::new(
            "SPY".to_owned(),
            "ribbon".to_owned(),
            money(400),
            money(2_000),
            30,
            0,
        ),
        bars.clone(),
        5,
        0,
    )
    .expect("driver starts");
    let decided = driver.step_n(8).await;
    assert_eq!(decided.len(), 8, "every bar decides");
    assert!(
        decided.iter().all(|decision| !decision.submitted),
        "no spread reaches the venue without chains"
    );
    assert!(
        decided
            .iter()
            .all(|decision| decision.skipped.as_deref()
                == Some(crate::replay_session::CHAIN_GATE_NOTE)),
        "every refusal names the missing feed verbatim"
    );
}

fn vertical_grant() -> Grant {
    let mut grant = single_grant();
    grant.scope.structures = vec![StructureKind::Vertical];
    grant
}

/// Covered bars flow: an installed source selects the chain-derived arm, the
/// screen admits each bar, and the shared path books two-leg fills — never
/// single-leg stock, and never legs the config arm never priced.
#[tokio::test]
async fn covered_bars_flow_into_two_leg_fills() {
    use protopyne_strategy::LegSelector;
    use protopyne_types::{ChainQuote, OptionContract, OptionKind};

    let bars = synthetic_bars(8);
    let mut driver = ReplayDriver::start(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(LegSelector::new().build())),
        risk_engine(),
        vertical_grant(),
        venue_config(),
        RunnerConfig::new(
            "SPY".to_owned(),
            "ribbon".to_owned(),
            money(400),
            money(2_000),
            30,
            0,
        ),
        bars.clone(),
        5,
        0,
    )
    .expect("driver starts");
    // Ladder around the ~100 touch on the grant's DTE window, quoted one
    // tick wide at each bar's own timestamp so fills pass the age gate and
    // the package clears its tolerance.
    let expiry = chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid expiry");
    let mut quotes = Vec::new();
    for bar in &bars {
        for (strike, (bid, ask)) in [
            (95, ("4.499", "4.501")),
            (100, ("2.499", "2.501")),
            (105, ("1.499", "1.501")),
            (110, ("0.499", "0.501")),
        ] {
            quotes.push(
                ChainQuote::new()
                    .contract(
                        OptionContract::new()
                            .underlying("SPY".to_owned())
                            .expiry(expiry)
                            .strike(Money::from(strike))
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
    driver.set_leg_quotes(Arc::new(crate::chain_history::HistoricalChainSource::new(
        quotes,
    )));
    let decided = driver.step_n(8).await;
    assert_eq!(decided.len(), 8, "every bar decides");
    for decision in &decided {
        assert!(
            decision.submitted,
            "covered bar submits: {:?}",
            decision.skipped
        );
        assert_eq!(decision.filled_quantity, 1, "the package is one unit");
        assert_eq!(decision.legs.len(), 2, "both legs are recorded");
    }
}

/// A chain source that counts full-window clones: the coverage screen must
/// never cost one. `covers` delegates to the inner cheap predicate — the
/// production composition — so only `chain_snapshot` clones.
struct CountingChain {
    inner: crate::chain_history::HistoricalChainSource,
    snapshots: std::sync::atomic::AtomicUsize,
    screens: std::sync::atomic::AtomicUsize,
}

impl protopyne_venue::LegQuoteSource for CountingChain {
    fn leg_quote(
        &self,
        contract: &protopyne_types::OptionContract,
    ) -> Option<protopyne_venue::Quote> {
        self.inner.leg_quote(contract)
    }

    fn leg_quote_at(
        &self,
        contract: &protopyne_types::OptionContract,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Option<protopyne_venue::Quote> {
        self.inner.leg_quote_at(contract, at)
    }

    fn covers(&self, underlying: &str, at: chrono::DateTime<chrono::Utc>) -> bool {
        self.screens
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.covers(underlying, at)
    }

    fn chain_snapshot(
        &self,
        underlying: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Vec<protopyne_types::ChainQuote> {
        self.snapshots
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.chain_snapshot(underlying, at)
    }
}

/// The coverage screen costs no snapshot clone: one snapshot per bar feeds
/// the view, and the screen answers its own question cheaply. A bounded
/// allocation budget, not a wall-clock benchmark — counts, never timing.
#[tokio::test]
async fn the_screen_costs_no_snapshot_clone_per_bar() {
    use protopyne_strategy::LegSelector;
    use protopyne_types::{ChainQuote, OptionContract, OptionKind};

    let bars = synthetic_bars(8);
    let mut driver = ReplayDriver::start(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(LegSelector::new().build())),
        risk_engine(),
        vertical_grant(),
        venue_config(),
        RunnerConfig::new(
            "SPY".to_owned(),
            "ribbon".to_owned(),
            Money::from(400),
            Money::from(2_000),
            30,
            0,
        ),
        bars.clone(),
        5,
        0,
    )
    .expect("driver starts");
    let expiry = chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid expiry");
    let mut quotes = Vec::new();
    for bar in &bars {
        for (strike, (bid, ask)) in [
            (95, ("4.499", "4.501")),
            (100, ("2.499", "2.501")),
            (105, ("1.499", "1.501")),
            (110, ("0.499", "0.501")),
        ] {
            quotes.push(
                ChainQuote::new()
                    .contract(
                        OptionContract::new()
                            .underlying("SPY".to_owned())
                            .expiry(expiry)
                            .strike(Money::from(strike))
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
    let counting = Arc::new(CountingChain {
        inner: crate::chain_history::HistoricalChainSource::new(quotes),
        snapshots: std::sync::atomic::AtomicUsize::new(0),
        screens: std::sync::atomic::AtomicUsize::new(0),
    });
    let installed: Arc<dyn protopyne_venue::LegQuoteSource> = counting.clone();
    driver.set_leg_quotes(installed);
    let decided = driver.step_n(8).await;
    assert_eq!(decided.len(), 8, "every bar decides");
    assert!(
        decided.iter().all(|decision| decision.submitted),
        "covered bars submit"
    );
    assert_eq!(
        counting
            .snapshots
            .load(std::sync::atomic::Ordering::Relaxed),
        8,
        "one snapshot per bar feeds the view, never a second for the screen"
    );
    assert_eq!(
        counting.screens.load(std::sync::atomic::Ordering::Relaxed),
        8,
        "the screen still answers once per bar, cheaply"
    );
}

#[test]
fn registry_ids_sorted_len_tracks() {
    let mut registry = ReplayRegistry::new();
    assert!(registry.is_empty());
    assert_eq!(registry.len(), 0);
    for id in ["c", "a", "b"] {
        registry.start(
            id.to_owned(),
            ReplayDriver::start(
                single_strategy(),
                risk_engine(),
                single_grant(),
                venue_config(),
                runner_config(),
                synthetic_bars(6),
                1,
                0,
            )
            .expect("driver starts"),
        );
    }
    assert!(!registry.is_empty());
    assert_eq!(registry.len(), 3);
    assert_eq!(registry.ids(), vec!["a", "b", "c"]);
    registry.start(
        "a".to_owned(),
        ReplayDriver::start(
            single_strategy(),
            risk_engine(),
            single_grant(),
            venue_config(),
            runner_config(),
            synthetic_bars(6),
            1,
            0,
        )
        .expect("restart starts"),
    );
    assert_eq!(registry.len(), 3);
    assert_eq!(registry.ids(), vec!["a", "b", "c"]);
}
