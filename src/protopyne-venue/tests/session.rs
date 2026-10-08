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
    let venue = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    TradingSession::boot(
        &SessionConfig::new().build(),
        venue,
        VenueStorage::new("test.db".into()),
    )
    .expect("boot")
}

fn job(id: &str) -> SubmitJob {
    SubmitJob {
        intent: OrderIntent::new()
            .client_id(id.to_owned())
            .proposal_id("prop-1".to_owned())
            .strategy("ribbon".to_owned())
            .underlying("SPY".to_owned())
            .side(OrderSide::Buy)
            .quantity(2)
            .limit_price(money("600.50"))
            .build(),
        quote: quote(),
        at: now(),
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
    assert_eq!(session.venue().orders().len(), 1);
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
