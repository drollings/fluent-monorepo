use super::*;
use crate::journal::VenueStorage;
use crate::order::{OrderIntent, OrderSide};
use crate::paper::{PaperConfig, PaperVenue};
use crate::venue::{QuoteSource, Venue};
use chrono::{TimeZone, Utc};
use protopyne_types::{Bar, Money};
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

fn job(id: &str) -> SubmitJob {
    job_for(id, "SPY")
}

#[tokio::test]
async fn bounded_intake_drains_without_growing() {
    // The tightest bound the config allows (one worker, one queue slot, one
    // credit): sequential submits must all complete — intake credit throttles
    // the producer and the queue never needs to grow past its single slot.
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let session = TradingSession::boot(
        &SessionConfig::new()
            .workers(1)
            .queue_capacity(1)
            .credit_limit(1)
            .build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("test.db".into()),
    )
    .expect("tightest bound still boots");
    for index in 0..3 {
        let record = session
            .submit_order(job(&format!("bound#{index}")))
            .await
            .expect("bounded intake drains");
        assert_eq!(record.filled_quantity, 2);
    }
    session.shutdown().await;
}

fn job_for(id: &str, underlying: &str) -> SubmitJob {
    SubmitJob {
        intent: OrderIntent::new()
            .client_id(id.to_owned())
            .proposal_id("prop-1".to_owned())
            .strategy("ribbon".to_owned())
            .underlying(underlying.to_owned())
            .side(OrderSide::Buy)
            .quantity(2)
            .limit_price(money("600.50"))
            .build(),
        quote: quote(),
        at: now(),
        is_exit: false,
    }
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
        Box<dyn std::future::Future<Output = Result<crate::order::OrderRecord, crate::error::VenueError>> + Send + 'a>,
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<crate::order::OrderRecord>> + Send + 'a>>
    {
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
        Box<dyn std::future::Future<Output = Result<crate::order::OrderRecord, crate::error::VenueError>> + Send + 'a>,
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
    let session = session();
    let record = session.submit_order(job("p#0")).await.expect("fills");
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
    registry.add_route("AAPL", Arc::clone(&stub)).expect("route");
    registry.add_route("SPY", Arc::clone(&paper)).expect("route");
    let session = TradingSession::boot(
        &SessionConfig::new().build(),
        registry,
        VenueStorage::new("test.db".into()),
    )
    .expect("boot");

    let routed = session
        .submit_order(job_for("x#aapl", "AAPL"))
        .await
        .expect("stub serves");
    assert_eq!(routed.intent.underlying, "AAPL");
    assert_eq!(routed.filled_quantity, 2);

    let paper_job = job_for("x#spy", "SPY");
    let through_session = session.submit_order(paper_job.clone()).await.expect("fills");
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
    assert_eq!(summary.failed.len(), 1, "the duplicate name must fail the batch");
    let message = format!("{:?}", summary.failed[0]);
    assert!(
        message.contains("freshness"),
        "the failure names the duplicate unit, got: {message}"
    );
    assert!(summary.panicked.is_empty());
    session.shutdown().await;
}
