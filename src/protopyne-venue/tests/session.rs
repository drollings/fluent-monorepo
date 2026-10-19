use super::*;
use crate::journal::{Journal, VenueStorage};
use crate::order::{OrderIntent, OrderSide};
use crate::paper::{PaperConfig, PaperVenue};
use crate::venue::{QuoteSource, Venue};
use chrono::{TimeZone, Utc};
use protopyne_types::{Bar, Money, Qty};
use rust_decimal::Decimal;
use std::sync::Arc;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn quote() -> Quote {
    Quote::new()
        .bid(money("600.00"))
        .ask(money("600.10"))
        .bid_size(50)
        .ask_size(50)
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap())
        .build()
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn bar(minute: u32) -> Bar {
    Bar::new()
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, minute, 0).unwrap())
        .o("600.00".parse().expect("decimal"))
        .h("601.00".parse().expect("decimal"))
        .l("599.00".parse().expect("decimal"))
        .c("600.50".parse().expect("decimal"))
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

fn session() -> TradingSession {
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("test.db".into()),
        None,
    )
    .expect("boot")
}

/// A session whose storage permit matches its journal: intent-first
/// submission refuses a permit bound to another file, so the boot storage
/// and the job journal must name the same path.
fn session_with_journal(
    config: &SessionConfig,
    registry: Arc<VenueRegistry>,
) -> (TradingSession, tempfile::TempDir, Arc<Journal>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Arc::new(Journal::open(&path).expect("open"));
    let session =
        TradingSession::boot(config, registry, VenueStorage::new(path), None).expect("boot");
    (session, dir, journal)
}

/// Risk approval minted for the job's proposal: a healthy entry under
/// default limits, so these tests pin the submission route — not risk.
fn approved_for(proposal_id: &str) -> protopyne_strategy::RiskApproved {
    use protopyne_strategy::{
        Direction, Proposal, ProposalSide, RiskEngine, RiskLimits, RiskState, StructureKind,
    };
    let proposal = Proposal::new()
        .id(proposal_id.to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money("600.50"))
        .max_loss(money("300"))
        .notional(money("1000"))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(now())
        .build();
    let state = RiskState {
        quote_age_secs: 0,
        clock_skew_secs: 0,
        reconciliation_healthy: true,
        facts: protopyne_strategy::PortfolioFacts::unobserved(),
    };
    RiskEngine::new(RiskLimits::default())
        .check(
            &proposal,
            protopyne_strategy::OrderShape::new(true, true),
            &state,
        )
        .expect("test proposal is healthy")
}

/// Minimal second venue: serves deterministic fills behind `dyn Venue`, so
/// the session test below proves the session is generic over handles rather
/// than `PaperVenue`-shaped.
struct StubVenue {
    served: std::sync::Mutex<Vec<crate::order::OrderRecord>>,
}

impl StubVenue {
    fn new() -> Self {
        Self {
            served: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Venue for StubVenue {
    fn dry_run(
        &self,
        _intent: &OrderIntent,
        _quote: &Quote,
    ) -> Result<crate::venue::DryRunOutcome, crate::error::VenueError> {
        Ok(crate::venue::DryRunOutcome {
            allowed: true,
            reserved_notional: Money::zero(),
        })
    }

    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        _quote: &'a Quote,
        now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::order::OrderRecord, crate::error::VenueError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let mut record = crate::order::OrderRecord::pending(intent, now);
            record.status = crate::order::OrderStatus::Filled;
            record.filled_quantity = 2;
            self.served.lock().expect("served").push(record.clone());
            Ok(record)
        })
    }

    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<crate::order::OrderRecord>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.served
                .lock()
                .expect("served")
                .iter()
                .find(|record| record.intent.client_id == client_id)
                .cloned()
        })
    }

    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        _now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::order::OrderRecord, crate::error::VenueError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.served
                .lock()
                .expect("served")
                .iter()
                .find(|record| record.intent.client_id == client_id)
                .cloned()
                .ok_or_else(|| crate::error::VenueError::Invalid("stub holds nothing".to_owned()))
        })
    }

    fn positions(&self) -> Vec<crate::order::Position> {
        Vec::new()
    }

    fn orders(&self) -> Vec<crate::order::OrderRecord> {
        self.served.lock().expect("served").clone()
    }

    fn apply_corporate_event(
        &self,
        _action: &crate::order::CorporateEventArgs,
    ) -> Result<crate::order::CorporateOutcome, crate::error::VenueError> {
        Ok(crate::order::CorporateOutcome::default())
    }
}

struct Stub {
    name: String,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
    fail: bool,
}

impl WorkUnit for Stub {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if self.fail {
            Err(WorkError::Execution("stale quote".to_owned()))
        } else {
            Ok(WorkOutput::ok("fresh"))
        }
    }
}

impl FieldAccess for Stub {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(name.to_owned()))
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        Err(FieldError::NotFound(name.to_owned()))
    }

    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}

impl Describable for Stub {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
}

impl_component!(Stub);

fn stub(name: &str, depends: &[&str], provides: &[&str], fail: bool) -> Arc<dyn Component> {
    Arc::new(Stub {
        name: name.to_owned(),
        depends: depends.iter().map(|d| ArcIntern::from(*d)).collect(),
        provides: provides.iter().map(|p| ArcIntern::from(*p)).collect(),
        fail,
    })
}

struct FixedSource {
    name: String,
    quote: Option<Quote>,
}

impl QuoteSource for FixedSource {
    fn latest(&self, _underlying: &str) -> Option<Quote> {
        self.quote
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[tokio::test]
async fn feed_scope_delivers_closed_bars() {
    let mut session = session();
    let mut rx = session.spawn_feed(vec![bar(0), bar(1), bar(2)], 8);
    let mut count = 0;
    while rx.recv().await.is_some() {
        count += 1;
    }
    assert_eq!(count, 3);
    session.shutdown().await;
}

/// One entry intent plus its risk token, bound by proposal id: the token
/// the protocol demands, minted for a healthy entry under default limits.
fn entry_intent(id: &str, underlying: &str) -> (OrderIntent, protopyne_strategy::RiskApproved) {
    let proposal_id = format!("prop-{id}");
    let intent = OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id(proposal_id.clone())
        .strategy("ribbon".to_owned())
        .underlying(underlying.to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build();
    (intent, approved_for(&proposal_id))
}

/// A two-venue registry routes each intent to its venue through the one
/// order path: the stub serves its symbol behind `dyn Venue` while the
/// paper path stays byte-identical to the bare venue. The session owns the
/// registry the protocol routes through — submission itself is the safe
/// protocol, never a session pool.
#[tokio::test]
async fn session_routes_across_two_venues_with_paper_equality() {
    use crate::submit::safe_submit;

    let paper: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let stub: Arc<dyn Venue> = Arc::new(StubVenue::new());
    let registry = Arc::new(VenueRegistry::single(Arc::clone(&paper)));
    registry
        .add_route("AAPL", Arc::clone(&stub))
        .expect("route");
    registry
        .add_route("SPY", Arc::clone(&paper))
        .expect("route");
    let (session, dir, journal) = session_with_journal(&SessionConfig::new().build(), registry);
    let submit = |intent: OrderIntent, approved: protopyne_strategy::RiskApproved| {
        let journal = Arc::clone(&journal);
        let registry = Arc::clone(session.registry());
        let quote = quote();
        let at = now();
        let permit = VenueStorage::new(dir.path().join("journal.db"));
        async move {
            let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
            fluent_wvr::CURRENT_CAPS
                .scope(
                    caps,
                    safe_submit(
                        &permit,
                        &journal,
                        registry.as_ref(),
                        &approved,
                        intent,
                        &quote,
                        at,
                    ),
                )
                .await
        }
    };
    let (aapl, aapl_token) = entry_intent("x#aapl", "AAPL");
    let routed = submit(aapl, aapl_token).await.expect("stub serves");
    assert_eq!(routed.intent.underlying, "AAPL");
    assert_eq!(routed.filled_quantity, 2);

    let (spy, spy_token) = entry_intent("x#spy", "SPY");
    let through_registry = submit(spy.clone(), spy_token).await.expect("fills");
    let bare_paper = PaperVenue::new(PaperConfig::new().build());
    let direct = bare_paper
        .submit(spy, &quote(), now())
        .await
        .expect("paper serves");
    assert_eq!(
        through_registry, direct,
        "paper path is byte-identical through registry routing"
    );
    assert_eq!(session.registry().orders().len(), 2);
    session.shutdown().await;
}

#[tokio::test]
async fn healthy_bar_completes_every_unit() {
    let session = session();
    let summary = session
        .run_bar(vec![
            stub("freshness", &[], &["quote.fresh"], false),
            stub("submit", &["quote.fresh"], &["order.record"], false),
        ])
        .await;
    assert_eq!(summary.completed.len(), 2);
    assert!(summary.failed.is_empty());
    assert!(summary.cancelled.is_empty());
    session.shutdown().await;
}

#[tokio::test]
async fn failing_unit_is_contained_while_neighbors_complete() {
    // Containment-only supervision: the failure is recorded, the independent
    // neighbor still completes, nothing panics, nothing restarts.
    // Transitive dependent cancellation is the supervisor's own contract,
    // covered by its e2e suite; per-bar units here are fast sync checks.
    let session = session();
    let summary = session
        .run_bar(vec![
            stub("freshness", &[], &["quote.fresh"], true),
            stub("monitor", &[], &["health.observed"], false),
        ])
        .await;
    assert_eq!(summary.failed.len(), 1);
    assert_eq!(summary.completed.len(), 1);
    assert!(summary.panicked.is_empty());
    session.shutdown().await;
}

#[tokio::test]
async fn quote_ladder_prefers_primary_and_fails_closed() {
    let primary: Arc<dyn QuoteSource> = Arc::new(FixedSource {
        name: "primary".to_owned(),
        quote: None,
    });
    let fallback: Arc<dyn QuoteSource> = Arc::new(FixedSource {
        name: "fallback".to_owned(),
        quote: Some(quote()),
    });
    let resolved =
        TradingSession::resolve_quote(vec![primary.clone(), fallback.clone()], "SPY").await;
    let (resolved_quote, source) = resolved.expect("fallback serves");
    assert_eq!(resolved_quote, quote());
    assert_eq!(source, "fallback");

    let dark = TradingSession::resolve_quote(vec![primary], "SPY").await;
    assert!(dark.is_none(), "every feed dark means no quote");
}

#[tokio::test]
async fn duplicate_registration_fails_the_bar_summary() {
    // A bar that registers the same unit name twice must report the failure
    // in its summary: silently running short while looking clean is an audit
    // hole, not containment.
    let session = session();
    let summary = session
        .run_bar(vec![
            stub("freshness", &[], &["quote.fresh"], false),
            stub("freshness", &[], &["quote.fresh"], false),
        ])
        .await;
    assert_eq!(
        summary.failed.len(),
        1,
        "the duplicate name must fail the batch"
    );
    let message = format!("{:?}", summary.failed[0]);
    assert!(
        message.contains("freshness"),
        "the failure names the duplicate unit, got: {message}"
    );
    assert!(summary.panicked.is_empty());
    session.shutdown().await;
}

/// A token minted for another proposal refuses before anything journals:
/// the protocol checks the token first, so a mismatched approval leaves
/// neither an intent row nor a venue order. The session owns the registry
/// the protocol routes through — the token binds here exactly as it binds
/// the runner.
#[tokio::test]
async fn mismatched_risk_token_refuses_before_any_journal_row() {
    use crate::submit::safe_submit;

    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let (session, dir, journal) = session_with_journal(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
    );
    let (intent, _) = entry_intent("x#bad", "SPY");
    let wrong = approved_for("prop-someone-else");
    let permit = VenueStorage::new(dir.path().join("journal.db"));
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let refusal = fluent_wvr::CURRENT_CAPS
        .scope(
            caps,
            safe_submit(
                &permit,
                &journal,
                session.registry().as_ref(),
                &wrong,
                intent,
                &quote(),
                now(),
            ),
        )
        .await
        .expect_err("token must bind");
    assert!(
        refusal.to_string().contains("different proposal"),
        "refusal names the cause, got: {refusal}"
    );
    let path = dir.path().join("journal.db");
    let caps = fluent_wvr::CapabilitySet::new().with(VenueStorage::new(path.clone()));
    let replayed = fluent_wvr::CURRENT_CAPS
        .scope(caps, async { journal.replay(&VenueStorage::new(path)) })
        .await
        .expect("replay")
        .events;
    assert!(
        replayed.is_empty(),
        "the token check precedes the first journal row"
    );
    assert!(session.registry().orders().is_empty());
    session.shutdown().await;
}

/// The feed pump is the backfill sensor's caller: a gapped feed suspends
/// entries before any consumer reads the hole, while a contiguous feed
/// leaves the gate open.
#[tokio::test]
async fn feed_pump_notes_bars_into_the_backfill_sensor() {
    use protopyne_types::{GapPolicy, SeriesContext, Timeframe};
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let (mut gapped, _dir, _) = session_with_journal(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
    );
    let gate = gapped.attach_backfill(
        SeriesContext::new("SPY", Timeframe::M1, GapPolicy::Dense),
        Vec::new(),
        0,
    );
    let mut rx = gapped.spawn_feed(vec![bar(0), bar(2)], 8);
    let mut count = 0;
    while rx.recv().await.is_some() {
        count += 1;
    }
    assert_eq!(count, 2, "the pipe still forwards every bar");
    assert!(
        gate.entries_suspended(),
        "the one-minute hole suspends entries"
    );
    gapped.shutdown().await;

    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let (mut clean, _dir, _) = session_with_journal(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
    );
    let gate = clean.attach_backfill(
        SeriesContext::new("SPY", Timeframe::M1, GapPolicy::Dense),
        Vec::new(),
        0,
    );
    let mut rx = clean.spawn_feed(vec![bar(0), bar(1), bar(2)], 8);
    while rx.recv().await.is_some() {}
    assert!(
        !gate.entries_suspended(),
        "a contiguous feed needs no fetch"
    );
    clean.shutdown().await;
}

/// The session polls its attached gates into the out-of-band sink:
/// banked mismatch lines deliver once, and the next poll is silent.
#[tokio::test]
async fn session_poll_alerts_delivers_banked_lines_exactly_once() {
    use crate::alert::{AlertCursor, AlertSource, MemorySink};
    use crate::order::Position;
    use crate::reconcile::{JournalBook, ReconcileMismatch, ReconcileReport, ReconcileSource};
    use std::collections::HashMap;
    struct StillBook;
    impl ReconcileSource for StillBook {
        fn journal(&self) -> JournalBook {
            JournalBook::default()
        }
        fn venue_positions(&self) -> Vec<Position> {
            Vec::new()
        }
        fn venue_legs(&self) -> HashMap<String, i64> {
            HashMap::new()
        }
    }
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let (mut session, _dir, _journal) = session_with_journal(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
    );
    let gate = session.attach_reconcile(Arc::new(StillBook));
    gate.observe(&ReconcileReport {
        matched: false,
        mismatches: vec![ReconcileMismatch {
            underlying: "SPY".to_owned(),
            journal_qty: 2,
            venue_qty: 0,
            reason: "test drift".to_owned(),
        }],
        alerts: vec!["RECONCILE HALT: test drift".to_owned()],
    });
    let sink = MemorySink::new();
    let mut cursor = AlertCursor::new();
    let delivered = session
        .poll_alerts(&mut cursor, &sink, now())
        .expect("poll delivers");
    assert_eq!(delivered, 1, "one banked line delivers once");
    let alerts = sink.alerts();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].source, AlertSource::ReconcileMismatch);
    let replay = session
        .poll_alerts(&mut cursor, &sink, now())
        .expect("second poll");
    assert_eq!(replay, 0, "delivery drains: no duplicate pages");
    assert_eq!(sink.alerts().len(), 1);
    session.shutdown().await;
}

/// A stub facts producer: observed-unavailable is enough to satisfy boot.
struct StubFacts;

impl protopyne_strategy::PortfolioFactsSource for StubFacts {
    fn facts(&self) -> protopyne_strategy::PortfolioFacts {
        protopyne_strategy::PortfolioFacts::unobserved()
    }
}

#[tokio::test]
async fn boot_refuses_aggregate_caps_without_a_facts_producer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = VenueStorage::new(dir.path().join("journal.db"));
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let registry = Arc::new(VenueRegistry::single(venue));
    let limits = RiskLimits::new().max_open_risk(1_000.0).build();
    let config = SessionConfig::new().risk_limits(limits).build();

    let Err(refused) = TradingSession::boot(&config, Arc::clone(&registry), storage.clone(), None)
    else {
        panic!("no producer must refuse");
    };
    assert!(matches!(refused, VenueError::Invalid(_)));

    let mut session = TradingSession::boot(
        &config,
        Arc::clone(&registry),
        storage.clone(),
        Some(Arc::new(StubFacts)),
    )
    .expect("a producer satisfies the caps");
    assert!(session.portfolio_facts().is_some());
    session.attach_portfolio_facts(Arc::new(StubFacts));
    session.shutdown().await;
}

#[tokio::test]
async fn a_session_without_aggregate_caps_boots_without_a_producer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = VenueStorage::new(dir.path().join("journal.db"));
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let registry = Arc::new(VenueRegistry::single(venue));
    let session = TradingSession::boot(&SessionConfig::new().build(), registry, storage, None)
        .expect("inert caps need no producer");
    assert!(session.portfolio_facts().is_none());
    session.shutdown().await;
}

/// The session-owned alert poller delivers a breaker trip exactly once on
/// the edge and never on the level, through the file the operator tails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alert_poller_delivers_breaker_trip_once_to_its_file() {
    use crate::alert::{AlertSink, FileSink};
    use crate::stream::{StreamBreaker, StreamConfig};
    use crate::tastytrade::TransportError;

    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let registry = Arc::new(VenueRegistry::single(venue));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        registry,
        VenueStorage::new("alert-poller.db".into()),
        None,
    )
    .expect("boot");
    let breaker = Arc::new(StreamBreaker::new(StreamConfig::new().build()));
    breaker.record_error(&TransportError::Terminal("gone: scripted".to_owned()));
    session.attach_stream_breaker(Arc::clone(&breaker));
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("alerts.jsonl");
    let sink: Arc<dyn AlertSink> = Arc::new(FileSink::new(path.clone()));
    session.spawn_alert_poller(sink, std::time::Duration::from_millis(10));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let first = std::fs::read_to_string(&path).expect("alert file");
    assert_eq!(
        first.lines().count(),
        1,
        "one trip, one line, got: {first:?}"
    );
    assert!(first.contains("BreakerTrip"), "got: {first:?}");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let second = std::fs::read_to_string(&path).expect("alert file");
    assert_eq!(
        second.lines().count(),
        1,
        "a held trip never re-alerts, got: {second:?}"
    );
    session.shutdown().await;
}

/// The poller drains a banked reconcile line exactly once: delivery never
/// clears the halt, and the drained bank never re-alerts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alert_poller_drains_a_reconcile_bank_exactly_once() {
    use crate::alert::{AlertSink, MemorySink};
    use crate::order::Position;
    use crate::reconcile::{JournalBook, ReconcileMismatch, ReconcileReport, ReconcileSource};
    use std::collections::HashMap;

    struct ScriptedBook;
    impl ReconcileSource for ScriptedBook {
        fn journal(&self) -> JournalBook {
            JournalBook::default()
        }
        fn venue_positions(&self) -> Vec<Position> {
            Vec::new()
        }
        fn venue_legs(&self) -> HashMap<String, i64> {
            HashMap::new()
        }
    }

    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let registry = Arc::new(VenueRegistry::single(venue));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        registry,
        VenueStorage::new("alert-bank.db".into()),
        None,
    )
    .expect("boot");
    let gate = session.attach_reconcile(Arc::new(ScriptedBook));
    gate.observe(&ReconcileReport {
        matched: false,
        mismatches: vec![ReconcileMismatch {
            underlying: "SPY".to_owned(),
            journal_qty: 2,
            venue_qty: 0,
            reason: "scripted drift".to_owned(),
        }],
        alerts: vec!["RECONCILE HALT: scripted".to_owned()],
    });
    assert!(gate.entries_halted());
    let sink = MemorySink::new();
    let serving: Arc<dyn AlertSink> = Arc::new(sink.clone());
    session.spawn_alert_poller(serving, std::time::Duration::from_millis(10));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(sink.alerts().len(), 1, "the bank drains exactly once");
    assert!(gate.entries_halted(), "delivery never clears the halt");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(sink.alerts().len(), 1, "the drained bank never re-alerts");
    session.shutdown().await;
}

/// A failing sink reports loudly and delivers nothing twice: the first poll
/// returns the error, and the drained bank is gone on the second poll —
/// never redelivered, never retried.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failing_sink_reports_loudly_and_delivers_nothing_twice() {
    use crate::alert::{Alert, AlertCursor};
    use crate::order::Position;
    use crate::reconcile::{JournalBook, ReconcileMismatch, ReconcileReport, ReconcileSource};
    use std::collections::HashMap;

    struct Dark;
    impl crate::alert::AlertSink for Dark {
        fn send(&self, _alert: &Alert) -> Result<(), crate::VenueError> {
            Err(crate::VenueError::Alert("dark sink: scripted".to_owned()))
        }
    }

    struct ScriptedBook;
    impl ReconcileSource for ScriptedBook {
        fn journal(&self) -> JournalBook {
            JournalBook::default()
        }
        fn venue_positions(&self) -> Vec<Position> {
            Vec::new()
        }
        fn venue_legs(&self) -> HashMap<String, i64> {
            HashMap::new()
        }
    }

    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let registry = Arc::new(VenueRegistry::single(venue));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        registry,
        VenueStorage::new("alert-dark.db".into()),
        None,
    )
    .expect("boot");
    let gate = session.attach_reconcile(Arc::new(ScriptedBook));
    gate.observe(&ReconcileReport {
        matched: false,
        mismatches: vec![ReconcileMismatch {
            underlying: "SPY".to_owned(),
            journal_qty: 2,
            venue_qty: 0,
            reason: "scripted drift".to_owned(),
        }],
        alerts: vec!["RECONCILE HALT: scripted".to_owned()],
    });
    let mut cursor = AlertCursor::new();
    let refused = session.poll_alerts(&mut cursor, &Dark, now());
    assert!(refused.is_err(), "a dark sink reports loudly");
    let redelivered = session
        .poll_alerts(&mut cursor, &Dark, now())
        .expect("an empty poll delivers cleanly");
    assert_eq!(redelivered, 0, "the drained bank is never redelivered");
    session.shutdown().await;
}
