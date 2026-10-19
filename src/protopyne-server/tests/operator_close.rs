//! Operator close/cancel wiring: the gateway verb and the HTTP route journal
//! the identical command row for the loop to execute, and the loop's
//! per-bar fold settles closes through the ledger while cancels hold no
//! budget.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::{
    BreakerLimits, Direction, Grant, GrantBudgets, GrantLedger, GrantPermission, GrantScope,
    PortfolioFacts, PortfolioThresholds, RiskEngine, RiskLimits, RiskState, SignalThresholds,
    StructureKind, TradeThresholds,
};
use protopyne_types::{LegSide, Money, OptionContract, OptionKind, SpreadLeg, SpreadLegs};
use protopyne_venue::{
    cancel_all_detail, close_all_detail, Journal, LegQuoteSource, OrderIntent, OrderRecord,
    OrderSide, OrderStatus, PaperConfig, PaperVenue, Quote, TradingSession, Venue, VenueRegistry,
    VenueStorage, COMMAND_CANCEL_ALL_KIND, COMMAND_CLOSE_ALL_KIND, OPERATOR_CLIENT_ID,
};
use protopyne_view::TerminalGateway;

use crate::service::{serve, ServerConfig};

fn bars() -> Vec<protopyne_types::Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

fn base(handle: &crate::service::ServerHandle) -> String {
    format!("http://{}", handle.addr)
}

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn quote() -> Quote {
    Quote::new()
        .bid(money("600.00"))
        .ask(money("600.10"))
        .bid_size(50)
        .ask_size(50)
        .ts(now())
        .build()
}

fn grant() -> Grant {
    Grant::new()
        .id("operator".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["operator".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical, StructureKind::Single])
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
                .max_loss(money("1000000"))
                .max_notional(money("100000000"))
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
                .max_loss_per_trade(money("1000"))
                .max_spread_width(money("50"))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money("10000000"))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

fn risk_state() -> RiskState {
    RiskState {
        quote_age_secs: 0,
        clock_skew_secs: 0,
        reconciliation_healthy: true,
        facts: PortfolioFacts::unobserved(),
    }
}

/// A close through the gateway equals a close through the route: both
/// journal the identical command row (same client, kind, and detail), so
/// the loop's fold cannot tell which surface ordered it.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_close_all_equals_route_close_all() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, bars()).await.expect("test server boots");
    let gateway = handle.gateway();
    gateway.close_all("paper").expect("gateway journals");
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/operator/close-all", base(&handle)))
        .json(&serde_json::json!({ "portfolio": "paper" }))
        .send()
        .await
        .expect("route posts");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let rows = gateway.journal().expect("journal reads");
    let closes: Vec<_> = rows
        .iter()
        .filter(|row| row.kind == COMMAND_CLOSE_ALL_KIND)
        .collect();
    assert_eq!(closes.len(), 2, "one row per surface: {rows:?}");
    for row in &closes {
        assert_eq!(row.client_id, OPERATOR_CLIENT_ID);
        assert_eq!(row.detail, close_all_detail("paper"));
    }
    handle.shutdown().await;
}

/// Same parity for cancel-all, plus both surfaces refuse a blank
/// portfolio before anything journals.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_cancel_all_equals_route_cancel_all() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, bars()).await.expect("test server boots");
    let gateway = handle.gateway();
    gateway.cancel_all("paper").expect("gateway journals");
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/operator/cancel-all", base(&handle)))
        .json(&serde_json::json!({ "portfolio": "paper" }))
        .send()
        .await
        .expect("route posts");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let rows = gateway.journal().expect("journal reads");
    let cancels: Vec<_> = rows
        .iter()
        .filter(|row| row.kind == COMMAND_CANCEL_ALL_KIND)
        .collect();
    assert_eq!(cancels.len(), 2, "one row per surface: {rows:?}");
    for row in &cancels {
        assert_eq!(row.client_id, OPERATOR_CLIENT_ID);
        assert_eq!(row.detail, cancel_all_detail("paper"));
    }
    assert!(gateway.close_all("  ").is_err(), "blank refuses");
    let blank = client
        .post(format!("{}/operator/cancel-all", base(&handle)))
        .json(&serde_json::json!({ "portfolio": "" }))
        .send()
        .await
        .expect("route posts");
    assert_eq!(blank.status(), reqwest::StatusCode::BAD_REQUEST);
    handle.shutdown().await;
}

/// Deterministic per-leg market data: OCC symbol → quote.
struct MapLegQuotes {
    map: HashMap<String, Quote>,
}

impl LegQuoteSource for MapLegQuotes {
    fn leg_quote(&self, contract: &OptionContract) -> Option<Quote> {
        self.map.get(&contract.occ_symbol()).copied()
    }

    /// These stubs model a newest-only feed: the opt-in is explicit, so the
    /// fill prices newest by declaration rather than by a silent default.
    fn newest_ok(&self) -> bool {
        true
    }
}

fn contract(strike: &str, kind: OptionKind) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(chrono::NaiveDate::from_ymd_opt(2026, 1, 16).expect("valid date"))
        .strike(money(strike))
        .kind(kind)
        .build()
}

fn vertical() -> SpreadLegs {
    SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(contract("600", OptionKind::Call))
            .side(LegSide::Long)
            .ratio(1)
            .build(),
        SpreadLeg::new()
            .contract(contract("610", OptionKind::Call))
            .side(LegSide::Short)
            .ratio(1)
            .build(),
    ])
    .expect("vertical must build")
}

fn leg_quotes() -> MapLegQuotes {
    let legs = vertical();
    let mut map = HashMap::new();
    map.insert(legs.legs()[0].contract.occ_symbol(), quote());
    map.insert(legs.legs()[1].contract.occ_symbol(), quote());
    MapLegQuotes { map }
}

/// A session whose storage permit matches its journal, over a paper venue
/// with leg quotes installed.
fn session_over(permit: VenueStorage) -> (TradingSession, Arc<VenueRegistry>) {
    let mut paper = PaperVenue::new(PaperConfig::new().build());
    paper.set_leg_quotes(Arc::new(leg_quotes()));
    let registry = Arc::new(VenueRegistry::single(Arc::new(paper)));
    let session = TradingSession::boot(
        &protopyne_venue::SessionConfig::new().build(),
        Arc::clone(&registry),
        permit,
        None,
    )
    .expect("boot");
    (session, registry)
}

async fn journaled(journal: &Journal, permit: &VenueStorage, kind: &str, detail: &str) {
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            journal
                .record_transition(permit, OPERATOR_CLIENT_ID, kind, detail, now())
                .expect("command row journals");
        })
        .await;
}

/// A close fills through the safe path, settles its own budget claim, and
/// drops the venue-book reading: the open vertical leaves the book, the
/// ledger holds one consumed trade with no pending claim, and the facts
/// read zero open risk after.
#[tokio::test]
async fn operator_close_settles_its_claim_and_drops_open_risk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Arc::new(Journal::open(&path).expect("open"));
    let permit = VenueStorage::new(path);
    let (session, registry) = session_over(permit.clone());
    let limits = RiskLimits::new().max_open_risk(10_000.0).build();
    let facts_of = || protopyne_venue::VenueBookFacts::new(registry.clone(), limits.clone());

    // Open one vertical directly through the registry: the book holds it.
    let intent = OrderIntent::new()
        .client_id("open#0".to_owned())
        .proposal_id("prop-open".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(1)
        .limit_price(money("5.00"))
        .maybe_legs(Some(vertical()))
        .build();
    registry
        .submit(intent, &quote(), now())
        .await
        .expect("vertical opens");
    let before = facts_of().facts_at(now());
    assert!(before.observed);
    assert!(
        before.open_risk.as_decimal() > Decimal::ZERO,
        "the open vertical reads committed risk"
    );

    journaled(
        &journal,
        &permit,
        COMMAND_CLOSE_ALL_KIND,
        &close_all_detail("paper"),
    )
    .await;
    let ledger = GrantLedger::new(grant());
    let risk = RiskEngine::new(RiskLimits::new().build());
    let cursor = crate::operator::execute_operator_commands(
        &session,
        &journal,
        &permit,
        &ledger,
        &risk,
        &risk_state(),
        &quote(),
        now(),
        0,
    )
    .await
    .expect("close executes");
    assert_eq!(cursor, 1);
    let closes: Vec<_> = registry
        .orders()
        .into_iter()
        .filter(|record| record.intent.client_id.starts_with("close-"))
        .collect();
    assert_eq!(closes.len(), 1, "one atomic close intent");
    assert_eq!(closes[0].status, OrderStatus::Filled);
    assert!(
        registry
            .positions()
            .iter()
            .all(|position| position.quantity == 0),
        "the book is flat after the close"
    );
    assert_eq!(
        ledger.pending(),
        protopyne_strategy::GrantUsage::new().build()
    );
    let usage = ledger.usage();
    assert_eq!(usage.used_trades, 1, "the close settles its own claim");
    assert_eq!(usage.used_loss, Money::zero());
    let after = facts_of().facts_at(now());
    assert_eq!(
        after.open_risk,
        Money::zero(),
        "committed risk drops out of the book reading"
    );
    session.shutdown().await;
}

/// A venue holding one working order, for the cancel sweep: the paper book
/// fills everything immediately, so the working order is planted.
struct WorkingVenue {
    inner: PaperVenue,
    working: Mutex<Option<OrderRecord>>,
}

impl WorkingVenue {
    fn plant(&self, record: OrderRecord) {
        *common_core::sync::lock(&self.working) = Some(record);
    }
}

impl Venue for WorkingVenue {
    fn dry_run(
        &self,
        intent: &OrderIntent,
        quote: &Quote,
    ) -> Result<protopyne_venue::DryRunOutcome, protopyne_venue::VenueError> {
        self.inner.dry_run(intent, quote)
    }

    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        quote: &'a Quote,
        now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<OrderRecord, protopyne_venue::VenueError>>
                + Send
                + 'a,
        >,
    > {
        self.inner.submit(intent, quote, now)
    }

    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<OrderRecord>> + Send + 'a>> {
        let planted = common_core::sync::lock(&self.working).clone();
        if planted
            .as_ref()
            .is_some_and(|record| record.intent.client_id == client_id)
        {
            return Box::pin(std::future::ready(planted));
        }
        self.inner.query(client_id)
    }

    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<OrderRecord, protopyne_venue::VenueError>>
                + Send
                + 'a,
        >,
    > {
        let mut working = common_core::sync::lock(&self.working);
        if working
            .as_ref()
            .is_some_and(|record| record.intent.client_id == client_id)
        {
            let mut record = working.take().expect("planted order cancels");
            record.status = OrderStatus::Cancelled;
            record.updated_at = now;
            return Box::pin(std::future::ready(Ok(record)));
        }
        drop(working);
        self.inner.cancel(client_id, now)
    }

    fn positions(&self) -> Vec<protopyne_venue::Position> {
        self.inner.positions()
    }

    fn open_spread_positions(&self) -> Option<Vec<protopyne_venue::SpreadPosition>> {
        self.inner.open_spread_positions()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        let mut orders = self.inner.orders();
        orders.extend(common_core::sync::lock(&self.working).clone());
        orders
    }

    fn apply_corporate_event(
        &self,
        action: &protopyne_venue::CorporateEventArgs,
    ) -> Result<protopyne_venue::CorporateOutcome, protopyne_venue::VenueError> {
        self.inner.apply_corporate_event(action)
    }

    fn fetch_balances(&self) -> Vec<protopyne_venue::BalanceEntry> {
        self.inner.fetch_balances()
    }

    fn fetch_open_orders(&self) -> Vec<OrderRecord> {
        let mut orders = self.inner.fetch_open_orders();
        orders.extend(
            common_core::sync::lock(&self.working)
                .clone()
                .into_iter()
                .filter(|record| record.status.is_active()),
        );
        orders
    }

    fn fetch_trades(&self) -> Vec<protopyne_venue::Fill> {
        self.inner.fetch_trades()
    }
}

/// A cancel sweep cancels the working order and holds no budget: the grant
/// ledger reads exactly as before — no claim taken, none released, usage
/// and pending both zero. Cancelling consumes nothing.
#[tokio::test]
async fn operator_cancel_holds_no_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Arc::new(Journal::open(&path).expect("open"));
    let permit = VenueStorage::new(path);
    let venue = Arc::new(WorkingVenue {
        inner: PaperVenue::new(PaperConfig::new().build()),
        working: Mutex::new(None),
    });
    let intent = OrderIntent::new()
        .client_id("work#0".to_owned())
        .proposal_id("prop-work".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build();
    venue.plant(
        OrderRecord::new()
            .intent(intent)
            .status(OrderStatus::Acknowledged)
            .filled_quantity(0)
            .maybe_avg_price(None)
            .fees_paid(Money::zero())
            .updated_at(now())
            .build(),
    );
    let registry = Arc::new(VenueRegistry::single(venue));
    let session = TradingSession::boot(
        &protopyne_venue::SessionConfig::new().build(),
        Arc::clone(&registry),
        permit.clone(),
        None,
    )
    .expect("boot");

    journaled(
        &journal,
        &permit,
        COMMAND_CANCEL_ALL_KIND,
        &cancel_all_detail("paper"),
    )
    .await;
    let ledger = GrantLedger::new(grant());
    let risk = RiskEngine::new(RiskLimits::new().build());
    let cursor = crate::operator::execute_operator_commands(
        &session,
        &journal,
        &permit,
        &ledger,
        &risk,
        &risk_state(),
        &quote(),
        now(),
        0,
    )
    .await
    .expect("cancel executes");
    assert_eq!(cursor, 1);
    assert!(
        registry.orders().is_empty(),
        "the working order cancelled away"
    );
    assert_eq!(
        ledger.pending(),
        protopyne_strategy::GrantUsage::new().build()
    );
    assert_eq!(
        ledger.usage(),
        protopyne_strategy::GrantUsage::new().build()
    );
    session.shutdown().await;
}
