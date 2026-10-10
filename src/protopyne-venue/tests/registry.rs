//! Execution routing registry: routing-table, cross-venue idempotency, and
//! paper-path-equality tests.
//!
//! Protocol: every test runs against real [`PaperVenue`] fills plus one
//! scripted stub — no sockets, no second venue SDK. Routing is by exact
//! underlying match over the registry's scoped index; unknown symbols
//! refuse with a reason; retryable failures fail over across the chain in
//! registration order while terminal refusals stop it (a merit refusal at
//! one venue must never fan out into a second order elsewhere).
//!
//! The paper-path-equality test pins the M8 compatibility promise: a
//! `VenueRegistry::single(paper)` serves byte-identical records to the bare
//! venue, so existing callers move without behavior change.

use super::*;
use crate::order::{CorporateEvent, OrderIntent, OrderRecord, OrderSide, OrderStatus, Position};
use crate::paper::{PaperConfig, PaperVenue};
use crate::quote::Quote;
use crate::venue::DryRunOutcome;
use chrono::{TimeZone, Utc};
use protopyne_types::Money;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
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

fn intent(client_id: &str, underlying: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(client_id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying(underlying.to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

fn paper() -> Arc<PaperVenue> {
    Arc::new(PaperVenue::new(PaperConfig::new().build()))
}

/// Scripted second venue: serves fills, fails retryably, or refuses on
/// merit — per test. Submit stays idempotent per client ID (query-first),
/// mirroring the trait contract, so the cross-venue idempotency test pins
/// the registry rather than stub sloppiness.
struct StubVenue {
    served: std::sync::Mutex<HashMap<String, OrderRecord>>,
    submit_count: std::sync::atomic::AtomicUsize,
    mode: std::sync::Mutex<StubMode>,
}

#[derive(Debug, Clone)]
enum StubMode {
    Serve,
    FailRetryable(String),
    Refuse(String),
}

impl StubVenue {
    fn serving() -> Self {
        Self::in_mode(StubMode::Serve)
    }

    fn in_mode(mode: StubMode) -> Self {
        Self {
            served: std::sync::Mutex::new(HashMap::new()),
            submit_count: std::sync::atomic::AtomicUsize::new(0),
            mode: std::sync::Mutex::new(mode),
        }
    }

    fn submits(&self) -> usize {
        self.submit_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Venue for StubVenue {
    fn dry_run(&self, _intent: &OrderIntent, _quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        Ok(DryRunOutcome {
            allowed: true,
            reserved_notional: Money::zero(),
        })
    }

    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        _quote: &'a Quote,
        now: chrono::DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(known) = self.query(&intent.client_id).await {
                return Ok(known);
            }
            self.submit_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.mode.lock().expect("mode").clone() {
                StubMode::Serve => {
                    let mut record = OrderRecord::pending(intent.clone(), now);
                    record.status = OrderStatus::Filled;
                    record.filled_quantity = intent.quantity;
                    record.avg_price = Some(intent.limit_price);
                    self.served
                        .lock()
                        .expect("served")
                        .insert(intent.client_id.clone(), record.clone());
                    Ok(record)
                }
                StubMode::FailRetryable(detail) => Err(VenueError::Server(detail)),
                StubMode::Refuse(detail) => Err(VenueError::Refused(detail)),
            }
        })
    }

    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<OrderRecord>> + Send + 'a>> {
        Box::pin(async move { self.served.lock().expect("served").get(client_id).cloned() })
    }

    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        _now: chrono::DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            self.served
                .lock()
                .expect("served")
                .get(client_id)
                .cloned()
                .ok_or_else(|| VenueError::Invalid("stub holds nothing".to_owned()))
        })
    }

    fn positions(&self) -> Vec<Position> {
        Vec::new()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        self.served.lock().expect("served").values().cloned().collect()
    }

    fn apply_corporate_event(
        &self,
        _underlying: &str,
        _event: CorporateEvent,
        _now: chrono::DateTime<Utc>,
    ) -> Result<String, VenueError> {
        Ok("stub".to_owned())
    }
}

// ── routing table ──────────────────────────────────────────────────

/// Exact match routes by symbol; once routes exist, unrouted symbols refuse
/// (the single-venue default only applies to a routeless registry, pinned
/// by `single_paper_path_is_byte_equal`).
#[tokio::test]
async fn exact_match_routes_by_symbol() {
    let paper = paper();
    let stub = Arc::new(StubVenue::serving());
    let registry = VenueRegistry::single(paper.clone());
    registry.add_route("AAPL", stub.clone()).expect("route");

    let routed = registry
        .submit(intent("c-aapl", "AAPL"), &quote(), now())
        .await
        .expect("stub serves");
    assert_eq!(routed.intent.underlying, "AAPL");
    assert_eq!(stub.submits(), 1, "routed symbol reaches the stub");
    assert!(paper.orders().is_empty(), "default never sees routed flow");

    let refusal = registry
        .submit(intent("c-spy", "SPY"), &quote(), now())
        .await
        .expect_err("unrouted symbol refuses once routes exist");
    assert!(
        refusal.to_string().contains("SPY"),
        "refusal names the symbol, got: {refusal}"
    );
    assert_eq!(stub.submits(), 1, "stub sees nothing further");
}

/// Retryable failure fails over to the next venue in chain order; the
/// routed venue is still tried first.
#[tokio::test]
async fn fallback_order_serves_after_retryable_failure() {
    let paper = paper();
    let flaky = Arc::new(StubVenue::in_mode(StubMode::FailRetryable(
        "status 500".to_owned(),
    )));
    let registry = VenueRegistry::single(paper.clone());
    registry.add_route("MSFT", flaky.clone()).expect("route");

    let record = registry
        .submit(intent("c-msft", "MSFT"), &quote(), now())
        .await
        .expect("fallback serves");
    assert_eq!(record.intent.underlying, "MSFT");
    assert_eq!(flaky.submits(), 1, "routed venue is tried first");
    assert_eq!(paper.orders().len(), 1, "chain order serves next");
}

/// Unknown symbols refuse with a reason on every routing surface; lookups
/// simply miss.
#[tokio::test]
async fn unknown_symbol_refuses_with_reason() {
    let registry = VenueRegistry::single(paper());
    registry
        .add_route("AAPL", Arc::new(StubVenue::serving()))
        .expect("route");

    let refusal = registry
        .submit(intent("c-tsla", "TSLA"), &quote(), now())
        .await
        .expect_err("unknown symbol must refuse");
    assert!(
        refusal.to_string().contains("TSLA"),
        "refusal names the symbol, got: {refusal}"
    );
    assert!(
        matches!(refusal, VenueError::Invalid(_)),
        "refusal is a routing refusal, got: {refusal:?}"
    );
    assert!(
        registry
            .dry_run(&intent("c-tsla", "TSLA"), &quote())
            .expect_err("dry run refuses too")
            .to_string()
            .contains("TSLA"),
        "dry run refuses with the same reason"
    );
    assert!(
        registry
            .apply_corporate_event("TSLA", CorporateEvent::Expiry, now())
            .expect_err("corporate events refuse too")
            .to_string()
            .contains("TSLA"),
        "corporate events refuse with the same reason"
    );
    assert!(
        registry.query("c-tsla").await.is_none(),
        "lookups miss without error"
    );
}

/// Terminal refusals stop the chain: a merit refusal at the routed venue
/// must never fan out into a second order elsewhere.
#[tokio::test]
async fn terminal_refusal_stops_the_chain() {
    let paper = paper();
    let refusing = Arc::new(StubVenue::in_mode(StubMode::Refuse(
        "stub would rest".to_owned(),
    )));
    let registry = VenueRegistry::single(paper.clone());
    registry.add_route("MSFT", refusing.clone()).expect("route");

    let refusal = registry
        .submit(intent("c-msft", "MSFT"), &quote(), now())
        .await
        .expect_err("merit refusal is final");
    assert!(
        matches!(refusal, VenueError::Refused(_)),
        "the stub's verdict survives, got: {refusal:?}"
    );
    assert_eq!(refusing.submits(), 1, "routed venue saw it once");
    assert!(
        paper.orders().is_empty(),
        "the fallback never receives refused flow"
    );
}

// ── cross-venue idempotency ─────────────────────────────────────────

/// Resubmitting a client ID after a re-route returns the original record:
/// the query chain finds the first venue's order, so the second venue never
/// mints a duplicate.
#[tokio::test]
async fn resubmit_by_client_id_returns_original_across_fallback() {
    let first = Arc::new(StubVenue::serving());
    let second = Arc::new(StubVenue::serving());
    let registry = VenueRegistry::single(first.clone());
    registry.add_route("AAPL", first.clone()).expect("route");

    let original = registry
        .submit(intent("c-aapl", "AAPL"), &quote(), now())
        .await
        .expect("first venue serves");
    assert_eq!(first.submits(), 1);

    registry.add_route("AAPL", second.clone()).expect("re-route");
    let again = registry
        .submit(intent("c-aapl", "AAPL"), &quote(), now())
        .await
        .expect("idempotent resubmit serves the original");
    assert_eq!(again, original, "same client ID returns the same record");
    assert_eq!(first.submits(), 1, "first venue never re-submits");
    assert_eq!(
        second.submits(),
        0,
        "second venue never mints a duplicate order"
    );
}

// ── paper-path equality ─────────────────────────────────────────────

/// A single-venue registry serves byte-identical records to the bare venue:
/// existing callers move to `VenueRegistry::single` without behavior change.
#[tokio::test]
async fn single_paper_path_is_byte_equal() {
    let direct = PaperVenue::new(PaperConfig::new().build());
    let expected = direct
        .submit(intent("c-spy", "SPY"), &quote(), now())
        .await
        .expect("paper serves");

    let paper = paper();
    let registry = VenueRegistry::single(paper.clone());
    let routed = registry
        .submit(intent("c-spy", "SPY"), &quote(), now())
        .await
        .expect("registry serves");
    assert_eq!(routed, expected, "registry record equals the bare record");
    assert_eq!(
        registry.positions(),
        direct.positions(),
        "venue truth matches too"
    );
}

/// Venue truth unions across venues in chain order.
#[tokio::test]
async fn truth_unions_across_venues() {
    let default_paper = paper();
    let aapl_paper = paper();
    let registry = VenueRegistry::single(default_paper.clone());
    registry.add_route("AAPL", aapl_paper.clone()).expect("route");
    registry
        .add_route("SPY", default_paper.clone())
        .expect("route");

    registry
        .submit(intent("c-aapl", "AAPL"), &quote(), now())
        .await
        .expect("aapl serves");
    registry
        .submit(intent("c-spy", "SPY"), &quote(), now())
        .await
        .expect("spy serves");

    assert_eq!(registry.orders().len(), 2, "orders union both venues");
    assert_eq!(registry.positions().len(), 2, "positions union both venues");
    assert_eq!(default_paper.orders().len(), 1, "default holds its own");
    assert_eq!(aapl_paper.orders().len(), 1, "routed holds its own");
}
