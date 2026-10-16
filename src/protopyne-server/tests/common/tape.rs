//! Shared helper for the tape-backed integration suites: seed an ephemeral
//! store with a bar series and hand back the key plus a whole-range read.
//!
//! One home for the seeding shape, so the history-source, multi-symbol, and
//! gateway-parity suites do not keep three copies of the same registration,
//! append, and request construction. The store's own gate runs inside the
//! permit's scope here; callers that later read the tape (through the runner
//! or the gateway) install the same scope around their async run.

use std::sync::Arc;

use protopyne_store::{
    AssetClass, CandleStore as _, FoldPolicy, MarketTape, PriceScale, ReadRequest, StoreStorage,
    TapeKey, TimeRange, WritePolicy,
};
use protopyne_strategy::{
    BreakerLimits, Direction, FixedSignal, Grant, GrantBudgets, GrantPermission, GrantScope,
    PortfolioThresholds, RiskEngine, RiskLimits, SignalThresholds, Strategy, StructureKind,
    TradeThresholds,
    elements::ExplicitStructure,
};
use protopyne_types::{Bar, Money, Timeframe};
use protopyne_venue::PaperConfig;

use crate::{BacktestConfig, RunnerConfig};

/// The venue spelling every seeded tape prints under.
pub const VENUE: &str = "test-venue";

/// The capability set a seeded in-memory tape is gated on.
#[must_use]
pub fn caps() -> fluent_wvr::CapabilitySet {
    fluent_wvr::CapabilitySet::new().with(StoreStorage::new(":memory:".into()))
}

/// Seed an ephemeral tape with `bars` and return the store, its key, and a
/// request that reads the whole series. Runs the store's gated calls inside
/// the permit's scope; callers must already be inside a tokio task.
#[must_use]
pub fn seed(
    bars: &[Bar],
    symbol: &str,
    timeframe: Timeframe,
    caps: &fluent_wvr::CapabilitySet,
) -> (MarketTape, TapeKey, ReadRequest) {    let tape = MarketTape::open_in_memory().expect("ephemeral tape opens");
    let key = fluent_wvr::CURRENT_CAPS.sync_scope(caps.clone(), || {
        let symbol_id = tape
            .register(
                symbol,
                VENUE,
                AssetClass::Stock,
                PriceScale::of(2).expect("valid scale"),
                "UTC",
                timeframe,
            )
            .expect("registration succeeds");
        let key = TapeKey { symbol_id, timeframe };
        tape.append(key, bars, WritePolicy::InsertIfAbsent)
            .expect("seed append succeeds");
        key
    });
    let first = bars.first().expect("seed needs at least one bar").ts.timestamp_millis();
    let last = bars.last().expect("seed needs at least one bar").ts.timestamp_millis();
    let end = last.saturating_add(timeframe.duration_ms());
    let request = ReadRequest {
        symbol: symbol.to_owned(),
        venue: VENUE.to_owned(),
        timeframe,
        range: TimeRange::bounded(first, end).expect("ordered range bounds"),
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    };
    (tape, key, request)
}

/// Seed a file-backed store at `root` with `bars` under `symbol`/`timeframe`
/// and return the open tape. Registration and appends run inside a
/// root-bound permit scope the caller supplies; the returned tape carries
/// the same binding a boot-time open would mint. Callers that reopen from
/// disk (like the boot path) ignore the return.
pub fn seed_file(
    root: &std::path::Path,
    bars: &[Bar],
    symbol: &str,
    timeframe: Timeframe,
    venue: &str,
) -> MarketTape {
    let tape = MarketTape::open(root).expect("file store opens");
    let caps =
        fluent_wvr::CapabilitySet::new().with(StoreStorage::new(root.to_path_buf()));
    fluent_wvr::CURRENT_CAPS.sync_scope(caps, || {
        let symbol_id = tape
            .register(
                symbol,
                venue,
                AssetClass::Stock,
                PriceScale::of(2).expect("valid scale"),
                "UTC",
                timeframe,
            )
            .expect("registration succeeds");
        tape.append(
            TapeKey { symbol_id, timeframe },
            bars,
            WritePolicy::InsertIfAbsent,
        )
        .expect("seed append succeeds");
    });
    tape
}

/// A flat rehearsal strategy: fixed long exposure into an explicit vertical,
/// the same shape the runner suites rehearse with.
#[must_use]
pub fn strategy() -> Strategy {
    Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
    ))
    .with_structure(Arc::new(ExplicitStructure { structure: StructureKind::Vertical }))
}

/// A permissive risk engine for a rehearsal run.
#[must_use]
pub fn risk() -> RiskEngine {
    RiskEngine::new(RiskLimits::new().build())
}

/// A rehearsal grant whose window covers the vendored history by construction.
#[must_use]
pub fn grant(symbol: &str) -> Grant {
    Grant::new()
        .id("tape-rehearsal".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec![symbol.to_owned()])
                .structures(vec![StructureKind::Vertical])
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission { entries: true, exits: true, rolls: false })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(1_000)
                .max_contracts(10_000)
                .max_loss(Money::from(1_000_000))
                .max_notional(Money::from(100_000_000))
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
                .max_loss_per_trade(Money::from(1_000))
                .max_spread_width(Money::from(50))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(Money::from(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid stamp"))
        .valid_until(chrono::DateTime::from_timestamp(1_900_000_000, 0).expect("valid stamp"))
        .build()
}

/// A venue pin for one symbol.
#[must_use]
pub fn venue_config(symbol: &str, data_version: &str) -> BacktestConfig {
    BacktestConfig::new()
        .underlying(symbol.to_owned())
        .data_version(data_version.to_owned())
        .build()
}

/// A runner config for one symbol.
#[must_use]
pub fn runner_config(symbol: &str) -> RunnerConfig {
    RunnerConfig::new(
        symbol.to_owned(),
        "ribbon".to_owned(),
        Money::from(400),
        Money::from(2_000),
        30,
        0,
    )
}

/// Explicit paper tuning (fees, slippage, liquidity) stated, not hidden.
#[must_use]
pub fn paper() -> PaperConfig {
    PaperConfig::new().build()
}
