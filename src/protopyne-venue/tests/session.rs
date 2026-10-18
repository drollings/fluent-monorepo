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
    let session = TradingSession::boot(config, registry, VenueStorage::new(path)).expect("boot");
    (session, dir, journal)
}

fn job(journal: &Arc<Journal>, id: &str) -> SubmitJob {
    job_for(journal, id, "SPY")
}

#[tokio::test]
async fn bounded_intake_drains_without_growing() {
    // The tightest bound the config allows (one worker, one queue slot, one
    // credit): sequential submits must all complete — intake credit throttles
    // the producer and the queue never needs to grow past its single slot.
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let config = SessionConfig::new()
        .workers(1)
        .queue_capacity(1)
        .credit_limit(1)
        .build();
    let (session, _dir, journal) =
        session_with_journal(&config, Arc::new(VenueRegistry::single(venue)));
    for index in 0..3 {
        let record = session
            .submit_order(job(&journal, &format!("bound#{index}")))
            .await
            .expect("bounded intake drains");
        assert_eq!(record.filled_quantity, 2);
    }
    session.shutdown().await;
}

fn job_for(journal: &Arc<Journal>, id: &str, underlying: &str) -> SubmitJob {
    let proposal_id = format!("prop-{id}");
    SubmitJob {
        intent: OrderIntent::new()
            .client_id(id.to_owned())
            .proposal_id(proposal_id.clone())
            .strategy("ribbon".to_owned())
            .underlying(underlying.to_owned())
            .side(OrderSide::Buy)
            .quantity(2)
            .limit_price(money("600.50"))
            .build(),
        quote: quote(),
        at: now(),
        is_exit: false,
        approved: approved_for(&proposal_id),
        journal: Arc::clone(journal),
    }
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
        is_limit_order: true,
        is_spread: true,
    };
    RiskEngine::new(RiskLimits::default())
        .check(&proposal, &state)
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
        _underlying: &str,
        _event: crate::order::CorporateEvent,
        _now: chrono::DateTime<Utc>,
    ) -> Result<String, crate::error::VenueError> {
        Ok("stub".to_owned())
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
    assert!(!session.is_blocked());
    session.shutdown().await;
}

#[tokio::test]
async fn pool_fanout_returns_each_verdict() {
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let (session, _dir, journal) = session_with_journal(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
    );
    let record = session
        .submit_order(job(&journal, "p#0"))
        .await
        .expect("fills");
    assert_eq!(record.filled_quantity, 2);
    assert_eq!(session.registry().orders().len(), 1);
    session.shutdown().await;
}

/// A session over a two-venue registry routes each job to its venue: the
/// stub serves its symbol behind `dyn Venue` while the paper path stays
/// byte-identical to the bare venue through the whole session stack.
#[tokio::test]
async fn session_routes_across_two_venues_with_paper_equality() {
    let paper: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let stub: Arc<dyn Venue> = Arc::new(StubVenue::new());
    let registry = Arc::new(VenueRegistry::single(Arc::clone(&paper)));
    registry
        .add_route("AAPL", Arc::clone(&stub))
        .expect("route");
    registry
        .add_route("SPY", Arc::clone(&paper))
        .expect("route");
    let (session, _dir, journal) = session_with_journal(&SessionConfig::new().build(), registry);
    let routed = session
        .submit_order(job_for(&journal, "x#aapl", "AAPL"))
        .await
        .expect("stub serves");
    assert_eq!(routed.intent.underlying, "AAPL");
    assert_eq!(routed.filled_quantity, 2);

    let paper_job = job_for(&journal, "x#spy", "SPY");
    let through_session = session
        .submit_order(paper_job.clone())
        .await
        .expect("fills");
    let bare_paper = PaperVenue::new(PaperConfig::new().build());
    let direct = bare_paper
        .submit(paper_job.intent, &paper_job.quote, paper_job.at)
        .await
        .expect("paper serves");
    assert_eq!(
        through_session, direct,
        "paper path is byte-identical through session + registry"
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
/// neither an intent row nor a venue order.
#[tokio::test]
async fn mismatched_risk_token_refuses_before_any_journal_row() {
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let (session, dir, journal) = session_with_journal(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
    );
    let mut bad = job_for(&journal, "x#bad", "SPY");
    bad.approved = approved_for("prop-someone-else");
    let refusal = session
        .submit_order(bad)
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

/// A reconciliation mismatch halts session entries while exits route: the
/// attached gate binds the submission path, not just the bar graph.
#[tokio::test]
async fn reconcile_mismatch_halts_entries_while_exits_route() {
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
    let (mut session, _dir, journal) = session_with_journal(
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
    let refusal = session
        .submit_order(job_for(&journal, "r#entry", "SPY"))
        .await
        .expect_err("mismatched books halt entries");
    assert!(
        refusal.to_string().contains("disagree") || refusal.to_string().contains("halt"),
        "refusal names the cause, got: {refusal}"
    );
    let mut exit = job_for(&journal, "r#exit", "SPY");
    exit.is_exit = true;
    let record = session.submit_order(exit).await.expect("exits route");
    assert_eq!(record.filled_quantity, 2);
    session.shutdown().await;
}

/// An unspliced backfill hole suspends session entries while exits route.
#[tokio::test]
async fn backfill_hole_suspends_entries_while_exits_route() {
    use protopyne_types::{GapPolicy, SeriesContext, Timeframe};
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let (mut session, _dir, journal) = session_with_journal(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
    );
    let gate = session.attach_backfill(
        SeriesContext::new("SPY", Timeframe::M1, GapPolicy::Dense),
        Vec::new(),
        0,
    );
    gate.suspend("test hole".to_owned());
    let refusal = session
        .submit_order(job_for(&journal, "b#entry", "SPY"))
        .await
        .expect_err("unspliced holes suspend entries");
    assert!(
        refusal.to_string().contains("suspend") || refusal.to_string().contains("hole"),
        "refusal names the cause, got: {refusal}"
    );
    let mut exit = job_for(&journal, "b#exit", "SPY");
    exit.is_exit = true;
    let record = session.submit_order(exit).await.expect("exits route");
    assert_eq!(record.filled_quantity, 2);
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
