use super::*;

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::{
    elements::{ExplicitStructure, FixedSignal},
    BreakerLimits, Direction, Grant, GrantBudgets, GrantPermission, GrantScope, OrderShape,
    PortfolioFacts, PortfolioFactsSource, PortfolioThresholds, Proposal, ProposalSide,
    RiskApproved, RiskLimits, RiskState, SignalThresholds, StructureKind, TradeThresholds,
};
use protopyne_types::{GapPolicy, Money, Qty, SeriesContext};
use protopyne_venue::{
    GateReading, Journal, JournalBook, OrderIntent, OrderSide, OrderStatus, PaperConfig,
    PaperVenue, Position, Quote, ReconcileReport, ReconcileSource, SessionConfig, StreamBreaker,
    StreamConfig, TradingSession, TransportError, Venue, VenueRegistry, VenueStorage,
};

use crate::backtest::{BacktestConfig, BacktestVenue};
use crate::runner::{run_backtest, RunnerConfig};

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

fn strategy() -> Strategy {
    Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
    ))
    .with_structure(Arc::new(ExplicitStructure {
        structure: StructureKind::Vertical,
    }))
}

fn risk() -> RiskEngine {
    RiskEngine::new(RiskLimits::new().build())
}

fn grant_with(min_conviction: f64) -> Grant {
    Grant::new()
        .id(format!("loop-{min_conviction}"))
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
                .min_conviction(min_conviction)
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

fn venue_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("spy-15m-v1".to_owned())
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

/// A fixed-answer quote source: `Some` always answers, `None` is a dark feed.
struct FixedSource(Option<Quote>);

impl QuoteSource for FixedSource {
    fn latest(&self, _underlying: &str) -> Option<Quote> {
        self.0
    }

    fn name(&self) -> &str {
        "fixed"
    }
}

fn quote() -> Quote {
    Quote::new()
        .bid(money(600))
        .ask(money(601))
        .bid_size(50)
        .ask_size(50)
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap())
        .build()
}

fn feed(bars: &[Bar]) -> mpsc::Receiver<Bar> {
    // Capacity fits every bar, so the buffered send never blocks and the
    // channel closes (dropping the sender) once the bars are queued.
    let (tx, rx) = mpsc::channel(bars.len().max(1));
    for bar in bars {
        tx.try_send(bar.clone()).expect("capacity fits every bar");
    }
    drop(tx);
    rx
}

/// The shared per-bar body means the loop, fed the same bars, decides exactly
/// as the batch run — the "one code path" assertion, mechanized.
#[tokio::test]
async fn the_loop_reproduces_the_batch_run() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    let quote_config = venue_config();
    let runner = runner_config();
    let timeframe = Timeframe::infer(&bars).unwrap_or(Timeframe::D1);
    let batch = run_backtest(&strategy, &risk, &grant, &quote_config, &runner, &bars, 11)
        .await
        .expect("batch run");

    let venue: Arc<dyn Venue> = Arc::new(
        BacktestVenue::new(
            quote_config.clone(),
            bars.clone(),
            PaperConfig::new().build(),
        )
        .expect("venue"),
    );
    let session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("loop-equality.db".into()),
        None,
    )
    .expect("boot");
    let mut receiver = feed(&bars);
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            Vec::new(),
            &strategy,
            &risk,
            &grant,
            &quote_config,
            &runner,
            timeframe,
            ChainPolicy::Unscreened,
            Duration::ZERO,
            11,
            None,
        )
        .await
        .expect("loop runs");
    assert_eq!(decisions, batch.decisions, "one body, two callers");
    assert!(
        decisions.iter().any(|decision| decision.submitted),
        "liquid history must fill"
    );
    session.shutdown().await;
}

/// A dark quote ladder halts every entry: each bar advances the signal, then
/// fails closed with the missing quote named, never inventing a price.
#[tokio::test]
async fn a_dark_quote_source_halts_entries() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    let quote_config = venue_config();
    let runner = runner_config();
    let timeframe = Timeframe::infer(&bars).unwrap_or(Timeframe::D1);

    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("loop-dark.db".into()),
        None,
    )
    .expect("boot");
    let mut receiver = feed(&bars);
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            vec![Arc::new(FixedSource(None))],
            &strategy,
            &risk,
            &grant,
            &quote_config,
            &runner,
            timeframe,
            ChainPolicy::Unscreened,
            Duration::ZERO,
            11,
            None,
        )
        .await
        .expect("loop runs");
    assert_eq!(decisions.len(), bars.len());
    assert!(
        decisions.iter().all(|decision| !decision.submitted),
        "a dark ladder must halt every entry"
    );
    assert!(decisions.iter().all(|decision| {
        decision
            .skipped
            .as_deref()
            .is_some_and(|reason| reason.contains("no quote from any source"))
    }));
    session.shutdown().await;
}

/// Boot a session whose storage permit matches its journal, so the durable
/// submission protocol can book an operator exit.
fn session_with_journal() -> (TradingSession, tempfile::TempDir, Arc<Journal>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Arc::new(Journal::open(&path).expect("open"));
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new(path),
        None,
    )
    .expect("boot");
    (session, dir, journal)
}

/// Risk approval minted for one proposal under default limits.
fn approved_for(proposal_id: &str) -> RiskApproved {
    let proposal = Proposal::new()
        .id(proposal_id.to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Exit)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money(600))
        .max_loss(money(300))
        .notional(money(1_000))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap())
        .build();
    let state = RiskState {
        quote_age_secs: 0,
        clock_skew_secs: 0,
        reconciliation_healthy: true,
        facts: PortfolioFacts::unobserved(),
    };
    RiskEngine::new(RiskLimits::new().build())
        .check(&proposal, OrderShape::new(false, true), &state)
        .expect("test exit is healthy")
}

/// An operator exit through the remaining order path: the close protocol
/// demands the same risk token as every other submission, and exits route
/// even while entries halt. Scopes the session's storage permit the same
/// channel the loop journals through.
async fn operator_exit(
    session: &TradingSession,
    dir: &tempfile::TempDir,
    journal: &Arc<Journal>,
    id: &str,
) -> protopyne_venue::OrderRecord {
    let proposal_id = format!("{id}-prop");
    let intent = OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id(proposal_id.clone())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Sell)
        .quantity(2)
        .limit_price(money(600))
        .build();
    let at = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap();
    let permit = VenueStorage::new(dir.path().join("journal.db"));
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let mut records = fluent_wvr::CURRENT_CAPS
        .scope(
            caps,
            session.submit_close_intents(
                journal,
                &approved_for(&proposal_id),
                vec![intent],
                &quote(),
                at,
            ),
        )
        .await
        .expect("operator exits route");
    assert_eq!(records.len(), 1, "one intent closes to one record");
    records.pop().expect("one record")
}

/// An observed book the loop reads through the session's producer: the
/// grant's open-risk cap binds at-rest exposure, so a book already over
/// the cap denies every entry while the risk gate (no aggregate caps
/// here) stays quiet.
struct BookOverCap;
impl PortfolioFactsSource for BookOverCap {
    fn facts(&self) -> PortfolioFacts {
        PortfolioFacts::observed(money(1_000_000), None, None, None)
    }
}

/// An observed book over the grant's open-risk cap halts loop entries
/// with the cap named: the loop sources the reading from the session's
/// producer, not from the ledger's lifetime tally.
#[tokio::test]
async fn observed_book_over_the_open_risk_cap_halts_loop_entries() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let mut grant = grant_with(0.5);
    grant.portfolio.max_open_risk = money(10_000);
    let quote_config = venue_config();
    let runner = runner_config();
    let timeframe = Timeframe::infer(&bars).unwrap_or(Timeframe::D1);

    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("loop-open-risk.db".into()),
        None,
    )
    .expect("boot");
    session.attach_portfolio_facts(Arc::new(BookOverCap));
    let mut receiver = feed(&bars);
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            Vec::new(),
            &strategy,
            &risk,
            &grant,
            &quote_config,
            &runner,
            timeframe,
            ChainPolicy::Unscreened,
            Duration::ZERO,
            11,
            None,
        )
        .await
        .expect("loop runs");
    assert_eq!(decisions.len(), bars.len());
    assert!(
        decisions.iter().all(|decision| !decision.submitted),
        "a book over the cap submits nothing"
    );
    assert!(decisions.iter().all(|decision| {
        decision
            .grant_merit_reasons
            .iter()
            .any(|reason| reason.contains("portfolio open-risk cap exceeded"))
    }));
    session.shutdown().await;
}

/// While the loop halts entries on a dark ladder, an operator exit still
/// routes: exits bypass the producer-confidence axis.
#[tokio::test]
async fn a_paper_exit_still_routes_while_entries_halt() {
    let (session, dir, journal) = session_with_journal();
    let mut receiver = feed(&history());
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            vec![Arc::new(FixedSource(None))],
            &strategy(),
            &risk(),
            &grant_with(0.5),
            &venue_config(),
            &runner_config(),
            Timeframe::D1,
            ChainPolicy::Unscreened,
            Duration::ZERO,
            11,
            None,
        )
        .await
        .expect("loop runs");
    assert!(decisions.iter().all(|decision| !decision.submitted));

    let record = operator_exit(&session, &dir, &journal, "exit#0").await;
    assert_eq!(record.filled_quantity, 2);
    session.shutdown().await;
}

/// The loop is a foreground driver: when the feed closes it returns, and the
/// session then shuts down with no task left to drain.
#[tokio::test]
async fn shutdown_drains_after_the_feed_closes() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    let quote_config = venue_config();
    let runner = runner_config();
    let timeframe = Timeframe::infer(&bars).unwrap_or(Timeframe::D1);
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("loop-shutdown.db".into()),
        None,
    )
    .expect("boot");
    // A scope-owned feed pump feeds the loop: the session owns it, so shutdown
    // must drain it.
    let mut receiver = session.spawn_feed(bars.clone(), bars.len().max(1));
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            Vec::new(),
            &strategy,
            &risk,
            &grant,
            &quote_config,
            &runner,
            timeframe,
            ChainPolicy::Unscreened,
            Duration::ZERO,
            11,
            None,
        )
        .await
        .expect("loop runs");
    assert_eq!(decisions.len(), bars.len());
    // Shutdown completes once the feed is drained: no leaked task hangs it,
    // and the drain reports clean.
    let drained = tokio::time::timeout(Duration::from_secs(5), session.shutdown())
        .await
        .expect("shutdown drains cleanly");
    assert!(drained, "a clean loop leaves no task to outlive shutdown");
}

/// A venue book that always disagrees with the journal: attachment alone
/// must not halt anything — only the sensor's observed trip does. The
/// per-bar units observe this every bar, so the halt persists across the
/// loop instead of being cleared by a clean self-check.
struct DriftingBook;

impl ReconcileSource for DriftingBook {
    fn journal(&self) -> JournalBook {
        JournalBook::default()
    }

    fn venue_positions(&self) -> Vec<Position> {
        vec![Position::new()
            .underlying("SPY".to_owned())
            .quantity(100)
            .avg_price(money(600))
            .build()]
    }

    fn venue_legs(&self) -> std::collections::HashMap<String, i64> {
        std::collections::HashMap::new()
    }
}

fn mismatch_report() -> ReconcileReport {
    ReconcileReport {
        matched: false,
        mismatches: Vec::new(),
        alerts: vec!["book disagrees with venue".to_owned()],
    }
}

/// A tripped reconcile gate halts loop entries with a named reason, while a
/// paper exit submitted through the session still routes.
#[tokio::test]
async fn tripped_reconcile_halts_loop_entries_but_routes_a_paper_exit() {
    let bars = history();
    let (mut session, dir, journal) = session_with_journal();
    // The sensor observes this every bar through the loop's supervised
    // units, so the halt persists instead of being cleared.
    session.attach_reconcile(Arc::new(DriftingBook));
    assert!(!session.gate_reading().allows_entries());

    let timeframe = Timeframe::infer(&bars).unwrap_or(Timeframe::D1);
    let mut receiver = feed(&bars);
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            Vec::new(),
            &strategy(),
            &risk(),
            &grant_with(0.5),
            &venue_config(),
            &runner_config(),
            timeframe,
            ChainPolicy::Unscreened,
            Duration::ZERO,
            11,
            None,
        )
        .await
        .expect("the loop refuses entries, it does not fail");
    assert_eq!(decisions.len(), bars.len());
    assert!(decisions.iter().all(|decision| !decision.submitted));
    assert!(decisions
        .iter()
        .all(
            |decision| decision.skipped.as_deref().is_some_and(|reason| reason
                .contains("producer-confidence")
                && reason.contains("disagrees"))
        ));

    let record = operator_exit(&session, &dir, &journal, "exit#0").await;
    assert_eq!(record.filled_quantity, 2);
    session.shutdown().await;
}

/// A journaled close-all executes through the loop even while a tripped
/// producer gate halts entries: the command fold runs outside the entry
/// gate, and exits route while halted.
#[tokio::test]
async fn journaled_close_all_routes_while_entries_halt() {
    use protopyne_venue::{close_all_detail, COMMAND_CLOSE_ALL_KIND, OPERATOR_CLIENT_ID};

    let bars = history();
    let (mut session, dir, journal) = session_with_journal();
    // The sensor observes this every bar through the loop's supervised
    // units, so the halt persists instead of being cleared.
    session.attach_reconcile(Arc::new(DriftingBook));
    assert!(!session.gate_reading().allows_entries());

    // Open a single before the loop: the close has something to flatten.
    session
        .registry()
        .submit(
            OrderIntent::new()
                .client_id("open#0".to_owned())
                .proposal_id("prop-open".to_owned())
                .strategy("ribbon".to_owned())
                .underlying("SPY".to_owned())
                .side(OrderSide::Buy)
                .quantity(2)
                .limit_price(money(601))
                .build(),
            &quote(),
            Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap(),
        )
        .await
        .expect("position opens");

    let permit = VenueStorage::new(dir.path().join("journal.db"));
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            journal
                .record_transition(
                    &permit,
                    OPERATOR_CLIENT_ID,
                    COMMAND_CLOSE_ALL_KIND,
                    &close_all_detail("paper"),
                    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap(),
                )
                .expect("command journals");
        })
        .await;

    let timeframe = Timeframe::infer(&bars).unwrap_or(Timeframe::D1);
    let mut receiver = feed(&bars);
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            Vec::new(),
            &strategy(),
            &risk(),
            &grant_with(0.5),
            &venue_config(),
            &runner_config(),
            timeframe,
            ChainPolicy::Unscreened,
            Duration::ZERO,
            11,
            Some((&journal, &permit)),
        )
        .await
        .expect("the loop refuses entries, it does not fail");
    assert_eq!(decisions.len(), bars.len());
    assert!(decisions.iter().all(|decision| !decision.submitted));
    let closes: Vec<_> = session
        .registry()
        .orders()
        .into_iter()
        .filter(|record| record.intent.client_id.starts_with("close-"))
        .collect();
    assert_eq!(closes.len(), 1, "the halted book still closes");
    assert_eq!(closes[0].status, OrderStatus::Filled);
    assert!(
        session
            .registry()
            .positions()
            .iter()
            .all(|position| position.quantity == 0),
        "the book is flat after the commanded close"
    );
    session.shutdown().await;
}
/// A reconnect gap suspends loop entries with a named reason: the feed pump
/// notes bars into the sensor, the per-bar resync cannot close the hole
/// with no source to fetch through, and the gate stays suspended while
/// exits still route through the submission path.
#[tokio::test]
async fn reconnect_gap_suspends_loop_entries() {
    let bars = history();
    let timeframe = Timeframe::infer(&bars).unwrap_or(Timeframe::D1);
    let mut gapped: Vec<Bar> = bars.iter().take(5).cloned().collect();
    let mut jumped = bars[5].clone();
    jumped.ts = gapped[4].ts + chrono::Duration::minutes(45);
    gapped.push(jumped);

    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("loop-backfill.db".into()),
        None,
    )
    .expect("boot");
    session.attach_backfill(
        SeriesContext::new("SPY", timeframe, GapPolicy::Sparse),
        Vec::new(),
        0,
    );
    assert!(!session.gate_reading().allows_entries());

    // The pump notes every bar into the sensor, so the gap suspends the
    // gate before the loop decides past it.
    let mut receiver = session.spawn_feed(gapped.clone(), gapped.len().max(1));
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            Vec::new(),
            &strategy(),
            &risk(),
            &grant_with(0.5),
            &venue_config(),
            &runner_config(),
            timeframe,
            ChainPolicy::Unscreened,
            Duration::ZERO,
            11,
            None,
        )
        .await
        .expect("the loop refuses entries, it does not fail");
    assert_eq!(decisions.len(), gapped.len());
    let halted = &decisions[5];
    assert!(!halted.submitted);
    assert!(
        halted.skipped.as_deref().is_some_and(
            |reason| reason.contains("producer-confidence") && reason.contains("suspend")
        )
    );
    assert!(!session.gate_reading().allows_entries());
    session.shutdown().await;
}

/// The reconcile projection: a resolved quote over a detached reading keeps
/// the sourced healthy bit, while a tripped reading reports unhealthy
/// instead of inventing health.
#[tokio::test]
async fn loop_quote_projects_the_reconcile_reading() {
    use protopyne_venue::TradingState;

    let bar = history()
        .into_iter()
        .nth(3)
        .expect("fixture has a fourth bar");
    let resolved = vec![Arc::new(FixedSource(Some(quote()))) as Arc<dyn QuoteSource>];

    let detached = GateReading::assemble(TradingState::Enabled, None, None, None);
    let (_, health) = super::loop_quote(&resolved, "SPY", &bar, &detached).await;
    assert!(health.reconciliation_healthy);

    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("loop-projection.db".into()),
        None,
    )
    .expect("boot");
    let handle = session.attach_reconcile(Arc::new(DriftingBook));
    handle.observe(&mismatch_report());
    let (_, health) = super::loop_quote(&resolved, "SPY", &bar, &session.gate_reading()).await;
    assert!(
        !health.reconciliation_healthy,
        "a tripped reading must not project healthy"
    );
    session.shutdown().await;
}

/// A stalled account stream halts loop entries with a named reason, while a
/// paper exit submitted through the session still routes.
#[tokio::test]
async fn stalled_stream_halts_loop_entries_but_routes_a_paper_exit() {
    let bars = history();
    let (mut session, dir, journal) = session_with_journal();
    let breaker = std::sync::Arc::new(StreamBreaker::new(StreamConfig::new().build()));
    breaker.record_error(&TransportError::Terminal("line cut".to_owned()));
    assert!(breaker.tripped());
    session.attach_stream_breaker(breaker);
    assert!(!session.gate_reading().allows_entries());

    let timeframe = Timeframe::infer(&bars).unwrap_or(Timeframe::D1);
    let mut receiver = feed(&bars);
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            Vec::new(),
            &strategy(),
            &risk(),
            &grant_with(0.5),
            &venue_config(),
            &runner_config(),
            timeframe,
            ChainPolicy::Unscreened,
            Duration::ZERO,
            11,
            None,
        )
        .await
        .expect("the loop refuses entries, it does not fail");
    assert_eq!(decisions.len(), bars.len());
    assert!(decisions.iter().all(|decision| !decision.submitted));
    assert!(decisions
        .iter()
        .all(
            |decision| decision.skipped.as_deref().is_some_and(|reason| reason
                .contains("producer-confidence")
                && reason.contains("breaker"))
        ));

    let record = operator_exit(&session, &dir, &journal, "exit#0").await;
    assert_eq!(record.filled_quantity, 2);
    session.shutdown().await;
}

/// A blocked feed pump never wedges shutdown: the scope owns the pump, so
/// closing the scope aborts the parked send and the drain reports clean.
#[tokio::test]
async fn shutdown_aborts_a_blocked_feed_pump() {
    let bars = history();
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("loop-blocked-pump.db".into()),
        None,
    )
    .expect("boot");
    // Capacity one with no consumer: the pump parks on its second send.
    let _parked = session.spawn_feed(bars, 1);
    let drained = tokio::time::timeout(Duration::from_secs(5), session.shutdown())
        .await
        .expect("shutdown stays bounded");
    assert!(drained, "a parked pump aborts with the scope");
}
