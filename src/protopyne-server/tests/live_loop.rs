//! Live-loop authority and the loop's durable outbox: an armed grant plus an
//! explicit journaled start is the only path to a bar, and a mid-loop kill
//! leaves journaled intents resolvable by query. Nothing here dials beyond
//! loopback; the credentialed soak stays `#[ignore]`.

use super::*;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::{
    elements::{ExplicitStructure, FixedSignal},
    BreakerLimits, Direction, Grant, GrantBudgets, GrantPermission, GrantScope,
    PortfolioThresholds, SignalThresholds, StructureKind, TradeThresholds,
};
use protopyne_types::Money;
use protopyne_venue::{
    Journal, OrderIntent, OrderRecord, OrderStatus, PaperConfig, PaperVenue, SessionConfig,
    TastytradeConfig, TastytradeTransport, TastytradeVenue, Venue, VenueError, VenueRegistry,
    VenueStorage, COMMAND_ARM_GRANT_KIND, COMMAND_START_LOOP_KIND, OPERATOR_CLIENT_ID,
};

use crate::backtest::BacktestConfig;
use crate::runner::RunnerConfig;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn bars() -> Vec<protopyne_types::Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

fn grant() -> Grant {
    Grant::new()
        .id("live-loop".to_owned())
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
                .min_conviction(0.0)
                .min_activity_score(0.0)
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

fn strategy() -> Arc<protopyne_strategy::Strategy> {
    Arc::new(
        protopyne_strategy::Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("signal"),
        ))
        .with_structure(Arc::new(ExplicitStructure {
            structure: StructureKind::Vertical,
        })),
    )
}

fn quote_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("live-loop".to_owned())
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

fn journal_authority(journal: &Journal, permit: &VenueStorage, armed: bool, started: bool) {
    if armed {
        journal
            .record_transition(
                permit,
                OPERATOR_CLIENT_ID,
                COMMAND_ARM_GRANT_KIND,
                &serde_json::to_string(&grant()).expect("grant json"),
                now(),
            )
            .expect("arm row");
    }
    if started {
        journal
            .record_transition(
                permit,
                OPERATOR_CLIENT_ID,
                COMMAND_START_LOOP_KIND,
                "{}",
                now(),
            )
            .expect("start row");
    }
}

/// A loop session over `venue`: the durable journal triple is real, the
/// authenticated venue is construction-only (the loop trades the registry's
/// venue, never `LiveSession.venue`). Starts with no supervised producer:
/// callers attach the set the case needs.
fn live_session(venue: Arc<dyn Venue>) -> LiveSession {
    let journal = Arc::new(Journal::ephemeral());
    let storage = VenueStorage::new(PathBuf::from("live-loop-test.db"));
    let session = protopyne_venue::TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        storage.clone(),
        None,
    )
    .expect("session boots");
    let transport = Arc::new(TastytradeTransport::new(
        TastytradeConfig::new()
            .base_url("http://127.0.0.1:9".to_owned())
            .build(),
    ));
    let broker = Arc::new(
        TastytradeVenue::new(
            transport,
            Arc::new(fluent_concurrency::io::net::NetCapability::new()),
            "ACC-LOOP".to_owned(),
        )
        .expect("venue constructs"),
    );
    LiveSession {
        session,
        journal,
        storage,
        venue: broker,
    }
}

fn loop_config<'a>(
    bars: Vec<protopyne_types::Bar>,
    strategy: &'a protopyne_strategy::Strategy,
    risk: &'a protopyne_strategy::RiskEngine,
    quote_config: &'a BacktestConfig,
    runner: &'a RunnerConfig,
    timeframe: protopyne_types::Timeframe,
) -> LiveLoop<'a> {
    LiveLoop {
        bars,
        sources: Vec::new(),
        strategy,
        risk,
        quote_config,
        runner_config: runner,
        timeframe,
        chains: ChainPolicy::Unscreened,
        cadence: std::time::Duration::ZERO,
        seed: 11,
    }
}

#[test]
fn an_arm_alone_is_not_a_start_and_a_start_alone_is_not_an_arm() {
    let gate = |armed: bool, started: bool| {
        let journal = Journal::ephemeral();
        let permit = VenueStorage::new(PathBuf::from("gate.db"));
        journal_authority(&journal, &permit, armed, started);
        gate_of(&journal.replay(&permit).unwrap().events)
    };
    assert_eq!(gate(false, false), LoopGate::Unarmed);
    assert_eq!(
        gate(true, false),
        LoopGate::NotStarted,
        "an armed grant without an explicit start does not run"
    );
    assert_eq!(
        gate(false, true),
        LoopGate::Unarmed,
        "a start with no arm still has no authority"
    );
    assert!(matches!(gate(true, true), LoopGate::Ready(_)));

    // A revoke ends the authority even though the start row remains.
    let journal = Journal::ephemeral();
    let permit = VenueStorage::new(PathBuf::from("gate.db"));
    journal_authority(&journal, &permit, true, true);
    journal
        .record_transition(
            &permit,
            OPERATOR_CLIENT_ID,
            protopyne_venue::COMMAND_REVOKE_GRANT_KIND,
            r#"{"grant_id":"live-loop"}"#,
            now(),
        )
        .expect("revoke row");
    assert_eq!(
        gate_of(&journal.replay(&permit).unwrap().events),
        LoopGate::Unarmed
    );
}

#[tokio::test]
async fn an_armed_and_started_loop_steps_each_bar_once() {
    use protopyne_types::{GapPolicy, SeriesContext};
    use protopyne_venue::{
        BackfillSource, JournalBook, ReconcileSource, StreamBreaker, StreamConfig, VenueError,
    };

    /// A matched book: an empty journal against an empty venue agrees, so
    /// the sensor observes clean and the loop may step.
    struct Matched;
    impl ReconcileSource for Matched {
        fn journal(&self) -> JournalBook {
            JournalBook::default()
        }
        fn venue_positions(&self) -> Vec<protopyne_venue::Position> {
            Vec::new()
        }
        fn venue_legs(&self) -> std::collections::HashMap<String, i64> {
            std::collections::HashMap::new()
        }
    }
    struct EmptyHistory;
    impl BackfillSource for EmptyHistory {
        fn fetch(
            &self,
            _start_ms: i64,
            _end_ms: i64,
        ) -> Result<Vec<protopyne_types::Bar>, VenueError> {
            Ok(Vec::new())
        }
    }

    let bars = bars();
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let mut live = live_session(venue);
    journal_authority(&live.journal, &live.storage, true, true);
    let feed_timeframe =
        protopyne_types::Timeframe::infer(&bars).unwrap_or(protopyne_types::Timeframe::D1);
    // The supervised producer set the live loop requires: a stream breaker,
    // a matched reconcile sensor, and a backfill sensor over the feed.
    live.session
        .attach_stream_breaker(Arc::new(StreamBreaker::new(StreamConfig::new().build())));
    live.session.attach_reconcile(Arc::new(Matched));
    live.session.attach_backfill(
        SeriesContext::new("SPY", feed_timeframe, GapPolicy::Sparse),
        vec![Arc::new(EmptyHistory) as Arc<dyn BackfillSource>],
        0,
    );

    let strategy = strategy();
    let risk = protopyne_strategy::RiskEngine::new(protopyne_strategy::RiskLimits::new().build());
    let quote_config = quote_config();
    let runner = runner_config();
    let timeframe =
        protopyne_types::Timeframe::infer(&bars).unwrap_or(protopyne_types::Timeframe::D1);
    let outcome = run_live_loop(
        &mut live,
        loop_config(
            bars.clone(),
            strategy.as_ref(),
            &risk,
            &quote_config,
            &runner,
            timeframe,
        ),
    )
    .await
    .expect("loop runs");
    let LiveLoopOutcome::Ran(decisions) = outcome else {
        panic!("armed + started must run, got {outcome:?}");
    };
    assert_eq!(decisions.len(), bars.len(), "one decision per fed bar");
    assert!(
        decisions.iter().any(|decision| decision.submitted),
        "a live quote source must fill the liquid fixture"
    );
    // Durable rows: the loop journals intents and transitions to its own
    // journal, never a discarded ephemeral one.
    let events = live.journal.replay(&live.storage).expect("replay").events;
    assert!(
        events
            .iter()
            .any(|event| event.kind == protopyne_venue::INTENT_KIND),
        "the loop's intents are journaled"
    );
    live.session.shutdown().await;
}

#[tokio::test]
async fn an_unarmed_loop_decides_nothing() {
    let bars = bars();
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let mut live = live_session(venue);
    journal_authority(&live.journal, &live.storage, true, false);

    let strategy = strategy();
    let risk = protopyne_strategy::RiskEngine::new(protopyne_strategy::RiskLimits::new().build());
    let quote_config = quote_config();
    let runner = runner_config();
    let outcome = run_live_loop(
        &mut live,
        loop_config(
            bars,
            strategy.as_ref(),
            &risk,
            &quote_config,
            &runner,
            protopyne_types::Timeframe::D1,
        ),
    )
    .await
    .expect("refusal is an outcome, not an error");
    assert!(
        matches!(outcome, LiveLoopOutcome::Refused(LoopGate::NotStarted)),
        "a grant without a start refuses: {outcome:?}"
    );
    assert!(
        live.journal
            .replay(&live.storage)
            .expect("replay")
            .events
            .iter()
            .all(|event| event.kind != protopyne_venue::INTENT_KIND),
        "nothing was journaled or placed"
    );
    live.session.shutdown().await;
}

/// A venue that accepts an order into `Pending` and never works it: the
/// mid-loop kill state (journaled, no venue verdict).
struct PendingVenue {
    inner: PaperVenue,
    orders: Mutex<HashMap<String, OrderRecord>>,
}

impl PendingVenue {
    fn new() -> Self {
        Self {
            inner: PaperVenue::new(PaperConfig::new().build()),
            orders: Mutex::new(HashMap::new()),
        }
    }
}

impl Venue for PendingVenue {
    fn dry_run(
        &self,
        intent: &OrderIntent,
        quote: &protopyne_venue::Quote,
    ) -> Result<protopyne_venue::DryRunOutcome, VenueError> {
        self.inner.dry_run(intent, quote)
    }

    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        _quote: &'a protopyne_venue::Quote,
        now: chrono::DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            let record = OrderRecord::pending(intent.clone(), now);
            self.orders
                .lock()
                .expect("orders")
                .insert(intent.client_id.clone(), record.clone());
            Ok(record)
        })
    }

    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<OrderRecord>> + Send + 'a>> {
        Box::pin(async move { self.orders.lock().expect("orders").get(client_id).cloned() })
    }

    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        _now: chrono::DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            Err(VenueError::Invalid(format!(
                "cancel of {client_id} not modelled"
            )))
        })
    }

    fn positions(&self) -> Vec<protopyne_venue::Position> {
        Vec::new()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        self.orders
            .lock()
            .expect("orders")
            .values()
            .cloned()
            .collect()
    }

    fn apply_corporate_event(
        &self,
        _action: &protopyne_venue::CorporateEventArgs,
    ) -> Result<protopyne_venue::CorporateOutcome, VenueError> {
        Err(VenueError::Invalid("not modelled".to_owned()))
    }
}

#[tokio::test]
async fn a_mid_loop_kill_leaves_the_intent_in_the_outbox_and_query_resolves_it() {
    use protopyne_types::{GapPolicy, SeriesContext};
    use protopyne_venue::{
        BackfillSource, JournalBook, ReconcileSource, StreamBreaker, StreamConfig, VenueError,
    };

    struct Matched;
    impl ReconcileSource for Matched {
        fn journal(&self) -> JournalBook {
            JournalBook::default()
        }
        fn venue_positions(&self) -> Vec<protopyne_venue::Position> {
            Vec::new()
        }
        fn venue_legs(&self) -> std::collections::HashMap<String, i64> {
            std::collections::HashMap::new()
        }
    }
    struct EmptyHistory;
    impl BackfillSource for EmptyHistory {
        fn fetch(
            &self,
            _start_ms: i64,
            _end_ms: i64,
        ) -> Result<Vec<protopyne_types::Bar>, VenueError> {
            Ok(Vec::new())
        }
    }

    let bars = bars();
    let venue: Arc<dyn Venue> = Arc::new(PendingVenue::new());
    let mut live = live_session(venue);
    journal_authority(&live.journal, &live.storage, true, true);
    let feed_timeframe =
        protopyne_types::Timeframe::infer(&bars).unwrap_or(protopyne_types::Timeframe::D1);
    live.session
        .attach_stream_breaker(Arc::new(StreamBreaker::new(StreamConfig::new().build())));
    live.session.attach_reconcile(Arc::new(Matched));
    live.session.attach_backfill(
        SeriesContext::new("SPY", feed_timeframe, GapPolicy::Sparse),
        vec![Arc::new(EmptyHistory) as Arc<dyn BackfillSource>],
        0,
    );

    let strategy = strategy();
    let risk = protopyne_strategy::RiskEngine::new(protopyne_strategy::RiskLimits::new().build());
    let quote_config = quote_config();
    let runner = runner_config();
    let timeframe =
        protopyne_types::Timeframe::infer(&bars).unwrap_or(protopyne_types::Timeframe::D1);
    let outcome = run_live_loop(
        &mut live,
        loop_config(
            bars,
            strategy.as_ref(),
            &risk,
            &quote_config,
            &runner,
            timeframe,
        ),
    )
    .await
    .expect("loop runs");
    let LiveLoopOutcome::Ran(decisions) = outcome else {
        panic!("armed + started must run");
    };
    assert!(decisions.iter().any(|decision| decision.submitted));

    // The kill: the loop is gone, but every intent it journaled is still
    // non-terminal, so the outbox names it and query-by-id resolves it.
    let outbox = live
        .journal
        .outbox_pending(&live.storage)
        .expect("outbox reads");
    assert!(!outbox.is_empty(), "a pending order must not be stranded");
    for intent in outbox {
        let found = live
            .session
            .registry()
            .query(&intent.client_id)
            .await
            .expect("query resolves");
        assert_eq!(found.intent.client_id, intent.client_id);
        assert_eq!(found.status, OrderStatus::Pending);
    }
    live.session.shutdown().await;
}

/// An armed and started loop with no supervised producer refuses: authority
/// alone never steps a bar.
#[tokio::test]
async fn authority_without_producers_refuses_the_loop() {
    let bars = bars();
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let mut live = live_session(venue);
    journal_authority(&live.journal, &live.storage, true, true);

    let strategy = strategy();
    let risk = protopyne_strategy::RiskEngine::new(protopyne_strategy::RiskLimits::new().build());
    let quote_config = quote_config();
    let runner = runner_config();
    let outcome = run_live_loop(
        &mut live,
        loop_config(
            bars,
            strategy.as_ref(),
            &risk,
            &quote_config,
            &runner,
            protopyne_types::Timeframe::D1,
        ),
    )
    .await
    .expect("refusal is an outcome, not an error");
    match outcome {
        LiveLoopOutcome::MissingProducers(missing) => {
            assert_eq!(
                missing,
                vec![
                    "stream".to_owned(),
                    "reconcile".to_owned(),
                    "backfill".to_owned()
                ],
                "a bare session names every missing producer"
            );
        }
        other => panic!("authority without producers must refuse, got {other:?}"),
    }
    assert!(
        live.journal
            .replay(&live.storage)
            .expect("replay")
            .events
            .iter()
            .all(|event| event.kind != protopyne_venue::INTENT_KIND),
        "nothing was journaled or placed"
    );
    live.session.shutdown().await;
}
