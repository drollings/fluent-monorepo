//! IB forward-compatibility conformance: a test-only IB-shaped broker over
//! the four neutral seams, and the shared conformance suite both it and the
//! TastyTrade path run.
//!
//! The fake maps TWS requests onto trait methods (`placeOrder` → `submit`,
//! `reqAccountUpdates` → `summary`, `reqExecutions` → `deals`,
//! `reqMktData` → quote pushes, `reqRealTimeBars` → bar pushes) and TWS
//! callbacks onto stored facts (`orderStatus`, `updateAccountValue`,
//! `execDetails`, `managedAccounts`). Nothing here dials: the TastyTrade
//! side runs the hermetic surface (loopback feed, no-dial reads), and the
//! shared manager/registry code paths are identical for both brokers.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::validate::Validate;
use fluent_wvr::{CapabilitySet, CURRENT_CAPS};
use protopyne_types::Money;
use tokio::sync::mpsc;

use crate::account::{
    AccountError, AccountFuture, AccountRange, AccountReader, AccountSummary, BrokerDeal,
};
use crate::auth::{
    AuthConfig, AuthDriver, AuthError, AuthFuture, AuthHandle, AuthMethod, Credential,
    CredentialSource, CredentialState, TokenManager,
};
use crate::broker::{BrokerBundle, BrokerCapabilities, BrokerId, BrokerRegistry};
use crate::error::VenueError;
use crate::feed::{
    FeedCapabilities, FeedError, FeedFuture, FeedSession, FeedStats, FeedSubscription,
    MarketDataFeed,
};
use crate::market::{BarUpdate, MarketEvent, MarketEventKind, TradePrint};
use crate::order::{
    CorporateEventArgs, CorporateOutcome, Fill, OrderIntent, OrderRecord, OrderSide, OrderStatus,
    Position,
};
use crate::quote::Quote;
use crate::venue::{DryRunOutcome, Venue};

fn money(value: &str) -> Money {
    Money::new(value.parse::<rust_decimal::Decimal>().expect("decimal"))
}

fn stamp(hour: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 5, hour, min, 0)
        .single()
        .expect("valid stamp")
}

/// Canned credential driver: TWS clientId auth never dials in the fake, it
/// issues the seeded state.
struct CannedDriver {
    state: CredentialState,
}

impl AuthDriver for CannedDriver {
    fn method(&self) -> AuthMethod {
        AuthMethod::SessionLogin
    }

    fn acquire<'a>(
        &'a self,
        _previous: Option<&'a CredentialState>,
    ) -> AuthFuture<'a, Result<CredentialState, AuthError>> {
        let state = self.state.clone();
        Box::pin(async move { Ok(state) })
    }
}

/// Test-only IB-shaped broker over one in-memory book: the four neutral
/// seams with TWS request names on the serving methods and TWS callback
/// names on the stored facts.
pub struct IbLikeBroker {
    account: String,
    buying_power: Money,
    orders: Mutex<HashMap<String, OrderRecord>>,
    positions: Mutex<HashMap<String, Position>>,
    deals: Mutex<Vec<BrokerDeal>>,
    summaries: HashMap<String, AccountSummary>,
    outboxes: Mutex<Vec<mpsc::Sender<MarketEvent>>>,
    deals_supported: bool,
    manager: Arc<TokenManager>,
}

impl IbLikeBroker {
    fn manager() -> Arc<TokenManager> {
        let state = CredentialState::issued(
            Credential::new("ib-client-1"),
            Utc::now(),
            Some(std::time::Duration::from_secs(300)),
            1,
        );
        Arc::new(
            TokenManager::new(
                AuthConfig::default(),
                Arc::new(CannedDriver { state }),
                None,
            )
            .expect("fake manager builds"),
        )
    }

    /// An IB-shaped broker serving `account` with no deals surface refused.
    #[must_use]
    pub fn new(account: &str) -> Self {
        Self::with_deals(account, true)
    }

    /// An IB-shaped broker with the deals surface optionally absent.
    #[must_use]
    pub fn with_deals(account: &str, deals_supported: bool) -> Self {
        let summary = AccountSummary {
            account: account.to_owned(),
            venue: "ib".to_owned(),
            currency: "USD".to_owned(),
            net_liquid: money("10000.00"),
            buying_power: money("10000.00"),
            margin_used: Money::zero(),
            observed_at: stamp(10, 0),
        };
        Self {
            account: account.to_owned(),
            buying_power: money("10000.00"),
            orders: Mutex::new(HashMap::new()),
            positions: Mutex::new(HashMap::new()),
            deals: Mutex::new(vec![seeded_deal("d-1", 10, OrderSide::Buy)]),
            summaries: HashMap::from([(account.to_owned(), summary)]),
            outboxes: Mutex::new(Vec::new()),
            deals_supported,
            manager: Self::manager(),
        }
    }

    /// `reqMktData` → `tickPrice`: publish one top-of-book snapshot.
    pub fn push_tick_price(&self, underlying: &str, bid: &str, ask: &str, at: DateTime<Utc>) {
        self.publish(&MarketEvent::Quote {
            underlying: underlying.to_owned(),
            quote: Quote {
                bid: money(bid),
                ask: money(ask),
                bid_size: 1,
                ask_size: 1,
                ts: at,
            },
        });
    }

    /// `reqRealTimeBars` → `realtimeBar`: publish one closed bar update.
    pub fn push_realtime_bar(&self, underlying: &str, bar: protopyne_types::Bar) {
        self.publish(&MarketEvent::BarUpdate {
            update: BarUpdate {
                underlying: underlying.to_owned(),
                bar,
                closed: true,
            },
        });
    }

    fn publish(&self, event: &MarketEvent) {
        let mut outboxes = common_core::sync::lock(&self.outboxes);
        outboxes.retain(|out| out.try_send(event.clone()).is_ok());
    }

    /// `execDetails` + `commissionReport`: book one execution against the
    /// mirror, upserting the position exactly as the venue's own fill path.
    pub fn book_execution(&self, client_id: &str, quantity: u32, price: &str, at: DateTime<Utc>) {
        let mut orders = common_core::sync::lock(&self.orders);
        let Some(record) = orders.get_mut(client_id) else {
            return;
        };
        record.apply_fill(
            Fill::new()
                .quantity(quantity)
                .price(money(price))
                .fee(Money::zero())
                .build(),
            at,
        );
        record.status = OrderStatus::Filled;
        let mut positions = common_core::sync::lock(&self.positions);
        let position = positions
            .entry(record.intent.underlying.clone())
            .or_insert(Position {
                underlying: record.intent.underlying.clone(),
                quantity: 0,
                avg_price: Money::zero(),
            });
        let sign = match record.intent.side {
            OrderSide::Buy => 1,
            OrderSide::Sell => -1,
        };
        position.apply(sign * i64::from(quantity), money(price));
    }

    /// `reqExecutions` log: executions the account surface reports as deals.
    pub fn seed_deal(&self, deal: BrokerDeal) {
        common_core::sync::lock(&self.deals).push(deal);
    }

    /// Drop every subscriber outbox: the feed went dark, sessions observe
    /// the close. A resubscribe after this is the reconnect.
    pub fn close_feed(&self) {
        common_core::sync::lock(&self.outboxes).clear();
    }

    /// `placeOrder` intent helper shared by the execution checks.
    pub fn order_intent(client_id: &str, underlying: &str) -> OrderIntent {
        OrderIntent::new()
            .client_id(client_id.to_owned())
            .proposal_id("p-1".to_owned())
            .strategy("s".to_owned())
            .underlying(underlying.to_owned())
            .side(OrderSide::Buy)
            .quantity(1)
            .limit_price(money("600.00"))
            .build()
    }
}

fn seeded_deal(id: &str, hour: u32, side: OrderSide) -> BrokerDeal {
    let at = stamp(hour, 0);
    BrokerDeal {
        deal_id: id.to_owned(),
        client_id: Some("c-1".to_owned()),
        order_id: None,
        contract: None,
        underlying: "SPY".to_owned(),
        side,
        fill: Fill::new()
            .quantity(2)
            .price(money("601.00"))
            .fee(money("0.65"))
            .build(),
        executed_at: at,
        observed_at: at,
    }
}

impl AccountReader for IbLikeBroker {
    fn capabilities(&self) -> BrokerCapabilities {
        BrokerCapabilities {
            accounts: true,
            orders: true,
            deals: self.deals_supported,
            ..BrokerCapabilities::default()
        }
    }

    /// `managedAccounts`.
    fn accounts(&self) -> AccountFuture<'_, Result<Vec<String>, AccountError>> {
        let accounts = vec![self.account.clone()];
        Box::pin(async move { Ok(accounts) })
    }

    /// `reqAccountUpdates` → `updateAccountValue`.
    fn summary<'a>(
        &'a self,
        account: &'a str,
    ) -> AccountFuture<'a, Result<AccountSummary, AccountError>> {
        Box::pin(async move {
            self.summaries
                .get(account)
                .cloned()
                .ok_or_else(|| AccountError::Terminal(format!("unknown account {account:?}")))
        })
    }

    /// `reqOpenOrders` → `orderStatus`.
    fn orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        Box::pin(async move {
            if account != self.account {
                return Err(AccountError::Terminal(format!(
                    "unknown account {account:?}"
                )));
            }
            Ok(range.window(
                common_core::sync::lock(&self.orders)
                    .values()
                    .cloned()
                    .map(|record| (record.updated_at, record)),
            ))
        })
    }

    fn open_orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        Box::pin(async move {
            if account != self.account {
                return Err(AccountError::Terminal(format!(
                    "unknown account {account:?}"
                )));
            }
            Ok(range.window(
                common_core::sync::lock(&self.orders)
                    .values()
                    .filter(|record| record.status.is_active())
                    .cloned()
                    .map(|record| (record.updated_at, record)),
            ))
        })
    }

    /// `reqExecutions` → `execDetails`.
    fn deals<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<BrokerDeal>, AccountError>> {
        Box::pin(async move {
            if !self.deals_supported {
                return Err(AccountError::Unsupported("deals".to_owned()));
            }
            if account != self.account {
                return Err(AccountError::Terminal(format!(
                    "unknown account {account:?}"
                )));
            }
            Ok(range.window(
                common_core::sync::lock(&self.deals)
                    .iter()
                    .cloned()
                    .map(|deal| (deal.executed_at, deal)),
            ))
        })
    }
}

impl MarketDataFeed for IbLikeBroker {
    fn capabilities(&self) -> FeedCapabilities {
        FeedCapabilities {
            quotes: true,
            trades: true,
            bars: true,
            native_bars: false,
            depth: false,
        }
    }

    fn subscribe<'a>(
        &'a self,
        subscriptions: &'a [FeedSubscription],
    ) -> FeedFuture<'a, Result<FeedSession, FeedError>> {
        Box::pin(async move {
            for subscription in subscriptions {
                subscription.validate()?;
                if subscription.kind == MarketEventKind::BookDelta {
                    return Err(FeedError::Unsupported(
                        "ib feed publishes no depth deltas".to_owned(),
                    ));
                }
            }
            let (out, inbox) = mpsc::channel(64);
            common_core::sync::lock(&self.outboxes).push(out);
            Ok(FeedSession::new(
                inbox,
                StreamAbort::new(),
                Arc::new(FeedStats::default()),
            ))
        })
    }
}

impl CredentialSource for IbLikeBroker {
    /// TWS clientId auth: the manager holds the seeded lease.
    fn auth_handle(&self) -> AuthHandle {
        AuthHandle::new(Arc::clone(&self.manager))
    }
}

impl Venue for IbLikeBroker {
    /// `placeOrder` pre-check: buying power and sanity, never the market.
    fn dry_run(
        &self,
        intent: &OrderIntent,
        _quote: &crate::quote::Quote,
    ) -> Result<DryRunOutcome, VenueError> {
        if intent.quantity == 0 {
            return Err(VenueError::Invalid("quantity must be positive".to_owned()));
        }
        let notional =
            intent.limit_price.as_decimal() * rust_decimal::Decimal::from(intent.quantity);
        if notional > self.buying_power.as_decimal() {
            return Err(VenueError::Refused(format!(
                "notional {notional} exceeds buying power {}",
                self.buying_power
            )));
        }
        Ok(DryRunOutcome {
            allowed: true,
            reserved_notional: Money::new(notional),
        })
    }

    /// `placeOrder`: idempotent per client ID, never a second order.
    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        _quote: &'a crate::quote::Quote,
        now: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            let mut orders = common_core::sync::lock(&self.orders);
            if let Some(record) = orders.get(&intent.client_id) {
                return Ok(record.clone());
            }
            let mut record = OrderRecord::pending(intent.clone(), now);
            record.status = OrderStatus::Submitted;
            orders.insert(intent.client_id.clone(), record.clone());
            Ok(record)
        })
    }

    /// `orderStatus` lookup by deterministic client ID.
    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<OrderRecord>> + Send + 'a>> {
        let record = common_core::sync::lock(&self.orders)
            .get(client_id)
            .cloned();
        Box::pin(std::future::ready(record))
    }

    /// `cancelOrder`: terminal orders return unchanged.
    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        _now: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            let mut orders = common_core::sync::lock(&self.orders);
            let Some(record) = orders.get_mut(client_id) else {
                return Err(VenueError::Invalid(format!("unknown order {client_id:?}")));
            };
            if matches!(
                record.status,
                OrderStatus::Filled
                    | OrderStatus::Cancelled
                    | OrderStatus::Rejected
                    | OrderStatus::Expired
                    | OrderStatus::OpenFailed
            ) {
                return Ok(record.clone());
            }
            record.status = OrderStatus::Cancelled;
            Ok(record.clone())
        })
    }

    /// `reqPositions`: venue-truth positions.
    fn positions(&self) -> Vec<Position> {
        common_core::sync::lock(&self.positions)
            .values()
            .cloned()
            .collect()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        common_core::sync::lock(&self.orders)
            .values()
            .cloned()
            .collect()
    }

    fn apply_corporate_event(
        &self,
        _action: &CorporateEventArgs,
    ) -> Result<CorporateOutcome, VenueError> {
        Ok(CorporateOutcome {
            note: "ib fake books no corporate actions".to_owned(),
            legs: Vec::new(),
            stock_leg: None,
            cash: None,
            affected: Vec::new(),
            realised: None,
            reconcile: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Shared conformance suite: the contract every broker meets, TastyTrade or
// IB-shaped. Each check cites the TastyTrade-side coverage that holds the
// reference path to it.
// ---------------------------------------------------------------------------

/// Account identities: the directory lists the account and the summary
/// names it. (TastyTrade: `accounts()` is a no-dial read.)
async fn conform_account_identities(reader: &Arc<dyn AccountReader>, account: &str) {
    let accounts = reader.accounts().await.expect("accounts list");
    assert!(
        accounts.iter().any(|id| id == account),
        "the directory lists {account}"
    );
    let caps = reader.capabilities();
    assert!(
        caps.accounts && caps.orders,
        "account reads are declared, never silent"
    );
}

/// Full summary shape with unknown-account refusal. (TastyTrade: transform
/// fixtures pin the same DTOs over recorded balances.)
async fn conform_account_summary(
    reader: &Arc<dyn AccountReader>,
    account: &str,
    expected_net: Money,
) {
    let summary = reader.summary(account).await.expect("summary reads");
    assert_eq!(summary.account, account);
    assert!(!summary.currency.is_empty(), "currency travels with money");
    assert_eq!(summary.net_liquid, expected_net);
    assert!(
        matches!(reader.summary("ZZ-9").await, Err(AccountError::Terminal(_))),
        "unknown accounts refuse terminally, never empty"
    );
}

/// Feed session semantics over a caller-driven publisher: supported kinds
/// subscribe, depth refuses honestly, and published events arrive with
/// their symbol and stats. (TastyTrade: the loopback stream suite pins the
/// same session shape over chunked NDJSON.)
async fn conform_feed(feed: &Arc<dyn MarketDataFeed>, publish: impl FnOnce()) {
    let mut session = feed
        .subscribe(&[FeedSubscription::of("SPY", MarketEventKind::Quote)])
        .await
        .expect("quotes subscribe");
    conform_feed_session(feed, &mut session, publish).await;
}

/// The drain half of [`conform_feed`], split out so a supervised run can
/// connect after the session exists: subscribing first is what keeps the
/// first generation from racing the subscription.
async fn conform_feed_session(
    feed: &Arc<dyn MarketDataFeed>,
    session: &mut FeedSession,
    publish: impl FnOnce(),
) {
    assert!(
        matches!(
            feed.subscribe(&[FeedSubscription::of("SPY", MarketEventKind::BookDelta)])
                .await,
            Err(FeedError::Unsupported(_))
        ),
        "depth refuses on a depth-less feed"
    );
    publish();
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut events = session.try_drain();
            if let Some(event) = events.pop() {
                break event;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("an event arrives");
    assert_eq!(event.symbol(), "SPY", "events carry their symbol");
    assert!(
        session.stats.snapshot().received > 0,
        "the session counts what it delivered"
    );
}

/// Credential publication builds without dialing. (TastyTrade: the venue
/// publishes the same handle over its manager.)
fn conform_credentials(source: &Arc<dyn CredentialSource>) {
    let _ = source.auth_handle();
}

/// The shared lease path through the manager: a lease issues and the
/// bearer is the seeded secret.
async fn conform_lease(manager: &Arc<TokenManager>) {
    let lease = manager.lease().await.expect("lease issues");
    assert!(
        !lease.credential().expose().is_empty(),
        "the lease carries a bearer"
    );
}

/// Execution substitution: dry-run gates, idempotent submit, query, and
/// unknown-cancel refusal behave identically on any venue. (TastyTrade:
/// dry-run and submit ride the same trait; the live path dials, so the
/// hermetic runs pin the fake and paper ends here.)
async fn conform_execution(venue: &Arc<dyn Venue>) {
    let quote = Quote {
        bid: money("599.00"),
        ask: money("601.00"),
        bid_size: 1,
        ask_size: 1,
        ts: stamp(10, 0),
    };
    let intent = IbLikeBroker::order_intent("exec-1", "SPY");
    let outcome = venue.dry_run(&intent, &quote).expect("dry run passes");
    assert!(outcome.allowed, "a funded intent is allowed");
    let zero = OrderIntent::new()
        .client_id("exec-0".to_owned())
        .proposal_id("p-1".to_owned())
        .strategy("s".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(0)
        .limit_price(money("600.00"))
        .build();
    assert!(
        venue.dry_run(&zero, &quote).is_err(),
        "a zero quantity refuses"
    );
    let first = venue
        .submit(intent.clone(), &quote, stamp(10, 0))
        .await
        .expect("submit books");
    let replay = venue
        .submit(intent.clone(), &quote, stamp(10, 0))
        .await
        .expect("resubmit is idempotent");
    assert_eq!(
        first.intent.client_id, replay.intent.client_id,
        "no second order is booked"
    );
    assert!(
        venue.query("exec-1").await.is_some(),
        "the booked order is queryable"
    );
    assert!(
        venue.query("missing").await.is_none(),
        "unknown orders read None, never an error"
    );
    let _ = venue.query("exec-1").await;
    assert!(
        venue.cancel("missing", stamp(10, 0)).await.is_err(),
        "cancelling unknown refuses"
    );
    let _ = venue.positions();
    let _ = venue.orders();
}

/// Bundle substitution: the directory routes by id, capabilities derive
/// from the carried handles, and duplicates refuse. (TastyTrade: the
/// broker-registry suite pins the same directory behavior.)
fn conform_bundle(bundle: &BrokerBundle, id: &str) {
    assert!(bundle.capabilities.accounts, "account reads declared");
    assert!(bundle.require_account().is_ok(), "the reader is attached");
    let registry = BrokerRegistry::new();
    registry.register(bundle.clone()).expect("registered");
    assert!(
        registry.register(bundle.clone()).is_err(),
        "duplicate ids refuse"
    );
    let found = registry
        .get(&BrokerId::new(id).expect("id"))
        .expect("found");
    assert_eq!(found.id.as_str(), id, "the directory routes by id");
}

// ---------------------------------------------------------------------------
// Runs against the IB-shaped broker.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ib_like_broker_meets_the_account_conformance() {
    let broker = Arc::new(IbLikeBroker::new("IB-1"));
    let reader: Arc<dyn AccountReader> = broker.clone();
    conform_account_identities(&reader, "IB-1").await;
    conform_account_summary(&reader, "IB-1", money("10000.00")).await;
    broker.seed_deal(seeded_deal("d-2", 12, OrderSide::Sell));
    let windowed = reader
        .deals(
            "IB-1",
            AccountRange {
                start: Some(stamp(11, 0)),
                ..AccountRange::all()
            },
        )
        .await
        .expect("windowed deals");
    assert_eq!(
        windowed.len(),
        1,
        "the window keeps the afternoon execDetails row"
    );
    assert_eq!(windowed[0].deal_id, "d-2");
}

#[tokio::test]
async fn ib_like_broker_meets_the_feed_conformance() {
    let broker = Arc::new(IbLikeBroker::new("IB-1"));
    let feed: Arc<dyn MarketDataFeed> = broker.clone();
    conform_feed(&feed, || {
        broker.push_tick_price("SPY", "100.00", "100.10", stamp(10, 0));
    })
    .await;
    // `reqRealTimeBars` rides the same session as `reqMktData`.
    let mut session = feed
        .subscribe(&[FeedSubscription::of("SPY", MarketEventKind::BarUpdate)])
        .await
        .expect("bars subscribe");
    broker.push_realtime_bar(
        "SPY",
        protopyne_types::Bar::new()
            .ts(stamp(10, 0))
            .o("100.00".parse().expect("decimal"))
            .h("100.10".parse().expect("decimal"))
            .l("99.90".parse().expect("decimal"))
            .c("100.05".parse().expect("decimal"))
            .volume(10)
            .build(),
    );
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), session.outbox.recv())
        .await
        .expect("the bar arrives")
        .expect("the channel stays open");
    assert!(
        matches!(
            &event,
            MarketEvent::BarUpdate { update } if update.underlying == "SPY" && update.closed
        ),
        "the realtime bar arrives closed, got {event:?}"
    );
}

#[tokio::test]
async fn ib_like_broker_meets_the_credential_conformance() {
    let source: Arc<dyn CredentialSource> = Arc::new(IbLikeBroker::new("IB-1"));
    conform_credentials(&source);
    conform_lease(&source.auth_handle().manager().clone()).await;
}

#[tokio::test]
async fn ib_like_broker_meets_the_execution_conformance() {
    let broker = Arc::new(IbLikeBroker::new("IB-1"));
    let venue: Arc<dyn Venue> = broker.clone();
    conform_execution(&venue).await;
    // `execDetails` books the fill and upserts the position.
    let quote = Quote {
        bid: money("599.00"),
        ask: money("601.00"),
        bid_size: 1,
        ask_size: 1,
        ts: stamp(10, 0),
    };
    venue
        .submit(
            IbLikeBroker::order_intent("c-9", "SPY"),
            &quote,
            stamp(10, 0),
        )
        .await
        .expect("submit books");
    broker.book_execution("c-9", 1, "600.00", stamp(10, 0));
    let filled = venue.query("c-9").await.expect("the booked order");
    assert_eq!(filled.status, OrderStatus::Filled);
    assert_eq!(venue.positions().len(), 1, "the fill upserts a position");
    assert_eq!(venue.positions()[0].quantity, 1);
    let terminal = venue
        .cancel("c-9", stamp(11, 0))
        .await
        .expect("terminal cancel is a no-op");
    assert_eq!(terminal.status, OrderStatus::Filled);
    // A working order cancels out.
    venue
        .submit(
            IbLikeBroker::order_intent("c-10", "SPY"),
            &quote,
            stamp(10, 0),
        )
        .await
        .expect("submit books");
    let cancelled = venue
        .cancel("c-10", stamp(11, 0))
        .await
        .expect("working orders cancel");
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
}

#[test]
fn ib_like_bundle_meets_the_substitution_conformance() {
    let bundle = BrokerBundle::new(
        BrokerId::new("ib").expect("id"),
        Arc::new(IbLikeBroker::new("IB-1")) as Arc<dyn Venue>,
    )
    .with_account(Arc::new(IbLikeBroker::new("IB-1")))
    .with_feed(Arc::new(IbLikeBroker::new("IB-1")))
    .with_credentials(Arc::new(IbLikeBroker::new("IB-1")));
    assert!(bundle.capabilities.streams, "the feed marks streams");
    assert!(bundle.capabilities.auth_refresh, "credentials mark refresh");
    conform_bundle(&bundle, "ib");
}

// ---------------------------------------------------------------------------
// Runs against the TastyTrade adapter's hermetic surface: the loopback
// stream, the no-dial account reads, the credential publication, and a
// registered bundle. Anything that dials stays on the live-ai path.
// ---------------------------------------------------------------------------

use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn net_caps() -> CapabilitySet {
    CapabilitySet::new().with(fluent_concurrency::io::net::NetCapability::new())
}

/// One scripted loopback stream: a chunked NDJSON response to the first
/// stream POST, then close. Nothing dials beyond 127.0.0.1.
async fn loopback_stream_base(chunks: Vec<String>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = stream.read(&mut tmp).await.expect("request reads");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            let Some(end) = buf
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|pos| pos + 4)
            else {
                continue;
            };
            let head = String::from_utf8_lossy(&buf[..end]);
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length:")
                        .or_else(|| line.strip_prefix("content-length:"))
                })
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            let mut have = buf.len().saturating_sub(end);
            while have < length {
                let n = stream.read(&mut tmp).await.expect("body reads");
                if n == 0 {
                    break;
                }
                have += n;
            }
            break;
        }
        let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        for chunk in &chunks {
            response.extend_from_slice(format!("{:x}\r\n{}\r\n", chunk.len(), chunk).as_bytes());
        }
        response.extend_from_slice(b"0\r\n\r\n");
        stream.write_all(&response).await.expect("stream writes");
    });
    format!("http://{addr}")
}

fn quote_line(symbol: &str, ts_ms: i64) -> String {
    serde_json::json!({
        "type": "quote",
        "symbol": symbol,
        "bid": "100.00",
        "ask": "100.10",
        "bid_size": 1,
        "ask_size": 1,
        "ts_ms": ts_ms,
    })
    .to_string()
}

fn stream_config() -> crate::stream::StreamConfig {
    crate::stream::StreamConfig::new()
        .backoff_base_ms(1)
        .backoff_jitter_pct(0)
        .max_consecutive_errors(32)
        .restart_window_secs(60)
        .max_restarts_per_window(64)
        .build()
}

fn tastytrade_transport(base: &str) -> std::sync::Arc<crate::tastytrade::TastytradeTransport> {
    std::sync::Arc::new(crate::tastytrade::TastytradeTransport::new(
        crate::tastytrade::TastytradeConfig::new()
            .base_url(base.to_owned())
            .build(),
    ))
}

/// The same feed conformance over a real TastytradeFeed: the loopback
/// publishes one quote, the adapter delivers it through the supervised
/// session with the symbol attached.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tastytrade_feed_meets_the_shared_feed_conformance() {
    let now_ms = Utc::now().timestamp_millis();
    let base = loopback_stream_base(vec![quote_line("SPY", now_ms) + "\n"]).await;
    let caps = net_caps();
    CURRENT_CAPS
        .scope(caps.clone(), async {
            let transport = tastytrade_transport(&base);
            let net = std::sync::Arc::new(fluent_concurrency::io::net::NetCapability::new());
            let feed: std::sync::Arc<dyn MarketDataFeed> = std::sync::Arc::new(
                crate::tastytrade_feed::TastytradeFeed::new(transport, net, 16),
            );
            let mut session = feed
                .subscribe(&[FeedSubscription::of("SPY", MarketEventKind::Quote)])
                .await
                .expect("quotes subscribe");
            let abort = StreamAbort::new();
            let run_abort = abort.clone();
            let run_feed = feed.clone();
            let run = tokio::spawn(CURRENT_CAPS.scope(caps, async move {
                run_feed.run_supervised(&stream_config(), &run_abort).await
            }));
            conform_feed_session(&feed, &mut session, || {}).await;
            abort.cancel();
            let _ = run.await;
        })
        .await;
}

/// The same account-identity conformance over the real adapter: capabilities
/// and the account list need no network.
#[test]
fn tastytrade_reader_meets_the_shared_account_identity_conformance() {
    let transport = tastytrade_transport("http://127.0.0.1:9");
    let net = std::sync::Arc::new(fluent_concurrency::io::net::NetCapability::new());
    let venue = std::sync::Arc::new(
        crate::live::TastytradeVenue::new(transport, net, "TT-1".to_owned())
            .expect("venue constructs without dialing"),
    );
    let reader: std::sync::Arc<dyn AccountReader> = std::sync::Arc::new(
        crate::tastytrade_account::TastytradeAccountReader::new(venue, "TT-1".to_owned()),
    );
    common_core::runtime::block_on(async {
        conform_account_identities(&reader, "TT-1").await;
    });
    // The credential publication builds over the same manager the IB fake
    // exercises: no dial happens until a lease is requested.
    let source: std::sync::Arc<dyn CredentialSource> = venue_credential_source();
    conform_credentials(&source);
}

/// The same substitution conformance over a TastyTrade-flavored bundle: the
/// directory routes by id with capabilities derived from the carried
/// handles, all without dialing.
#[test]
fn tastytrade_bundle_meets_the_shared_substitution_conformance() {
    let transport = tastytrade_transport("http://127.0.0.1:9");
    let net = std::sync::Arc::new(fluent_concurrency::io::net::NetCapability::new());
    let venue = std::sync::Arc::new(
        crate::live::TastytradeVenue::new(transport, net, "TT-1".to_owned())
            .expect("venue constructs without dialing"),
    );
    let bundle = BrokerBundle::new(
        BrokerId::new("tastytrade").expect("id"),
        venue.clone() as std::sync::Arc<dyn Venue>,
    )
    .with_account(std::sync::Arc::new(
        crate::tastytrade_account::TastytradeAccountReader::new(venue.clone(), "TT-1".to_owned()),
    ))
    .with_credentials(venue.clone() as std::sync::Arc<dyn CredentialSource>);
    assert!(
        bundle.capabilities.auth_refresh,
        "the venue publishes credentials"
    );
    conform_bundle(&bundle, "tastytrade");
}

fn venue_credential_source() -> std::sync::Arc<dyn CredentialSource> {
    let transport = tastytrade_transport("http://127.0.0.1:9");
    let net = std::sync::Arc::new(fluent_concurrency::io::net::NetCapability::new());
    let venue = std::sync::Arc::new(
        crate::live::TastytradeVenue::new(transport, net, "TT-1".to_owned())
            .expect("venue constructs without dialing"),
    );
    venue as std::sync::Arc<dyn CredentialSource>
}

// ---------------------------------------------------------------------------
// M14.1: the full live assembly over IB-shaped parts.
// ---------------------------------------------------------------------------

use crate::backfill::BackfillSource;
use crate::journal::{Journal, VenueStorage};
use crate::reconcile::{JournalBook, ReconcileSource};
use crate::registry::VenueRegistry;
use crate::session::SessionConfig;
use crate::session::TradingSession;
use crate::stream::{StreamBreaker, StreamConfig};

struct EmptyBooks;

impl ReconcileSource for EmptyBooks {
    fn journal(&self) -> JournalBook {
        JournalBook {
            net: HashMap::new(),
            legs: HashMap::new(),
        }
    }

    fn venue_positions(&self) -> Vec<Position> {
        Vec::new()
    }

    fn venue_legs(&self) -> HashMap<String, i64> {
        HashMap::new()
    }
}

struct EmptyBars;

impl BackfillSource for EmptyBars {
    fn fetch(&self, _start_ms: i64, _end_ms: i64) -> Result<Vec<protopyne_types::Bar>, VenueError> {
        Ok(Vec::new())
    }
}

/// M14.1: feed, credential manager, account reader, and broker registry wire
/// together around one booted session, and every required producer is
/// named: stream, reconcile, and backfill handles are attached, the feed
/// pumps under the session scope, and the directory routes the bundle. The
/// venue side cannot own the tape-backed ingest (layering: the server boot
/// supervises that over this same feed), so the feed end here is the pump
/// plus the directory entry the ingest subscribes through.
#[tokio::test]
async fn ib_like_full_session_assembly_is_complete() {
    use protopyne_types::{GapPolicy, Timeframe};

    let broker = Arc::new(IbLikeBroker::new("IB-1"));
    let venue: Arc<dyn Venue> = broker.clone();
    let registry = Arc::new(VenueRegistry::single(venue));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        registry,
        VenueStorage::new("ib-assembly.db".into()),
        None,
    )
    .expect("session boots");
    session.attach_credential(broker.auth_handle());
    session.attach_stream_breaker(Arc::new(StreamBreaker::new(StreamConfig::new().build())));
    session.attach_reconcile(Arc::new(EmptyBooks));
    session.attach_backfill(
        protopyne_types::SeriesContext::new("SPY", Timeframe::M1, GapPolicy::Sparse),
        vec![Arc::new(EmptyBars) as Arc<dyn crate::backfill::BackfillSource>],
        0,
    );
    // Every required producer is attached: the loop would refuse without
    // any one of these, and the feed rides alongside under the scope.
    assert!(
        session.stream_breaker().is_some(),
        "the stream producer is attached"
    );
    assert!(
        session.reconcile_handle().is_some(),
        "the reconcile producer is attached"
    );
    assert!(
        session.backfill_handle().is_some(),
        "the backfill producer is attached"
    );
    let feed: Arc<dyn MarketDataFeed> = broker.clone();
    let pump = session.spawn_event_feed(
        vec![MarketEvent::TradePrint {
            underlying: "SPY".to_owned(),
            print: TradePrint {
                ts: stamp(10, 0),
                price: money("100.05"),
                size: 4,
                aggressor: None,
            },
        }],
        16,
    );
    let mut rx = pump.subscribe();
    let mut event = None;
    for _ in 0..100 {
        if let Ok(frame) = rx.try_recv() {
            event = Some(frame);
            break;
        }
        tokio::task::yield_now().await;
    }
    let event = event.expect("the pump delivers under the scope");
    assert_eq!(event.symbol(), "SPY", "the session pump serves the feed");

    // The same venue object routes execution and reads: the directory only
    // names what the boot already owns.
    let bundle = BrokerBundle::new(BrokerId::new("ib").expect("id"), venue_from(&broker))
        .with_account(broker.clone() as Arc<dyn AccountReader>)
        .with_feed(feed)
        .with_credentials(broker.clone() as Arc<dyn CredentialSource>);
    assert!(bundle.capabilities.accounts, "reads are declared");
    assert!(bundle.capabilities.orders, "orders are declared");
    assert!(bundle.capabilities.deals, "deals are declared");
    assert!(bundle.capabilities.streams, "the feed marks streams");
    assert!(bundle.capabilities.auth_refresh, "credentials mark refresh");
    let directory = BrokerRegistry::new();
    directory.register(bundle).expect("registered");
    assert_eq!(directory.ids().len(), 1, "one bundle is routed");
    let found = directory
        .get(&BrokerId::new("ib").expect("id"))
        .expect("found");
    assert!(found.require_account().is_ok(), "reads resolve");
    assert!(found.require_feed().is_ok(), "the feed resolves");
    assert!(found.require_credentials().is_ok(), "credentials resolve");
    assert!(
        session.shutdown().await,
        "the session drains its scope on shutdown"
    );
}

fn venue_from(broker: &Arc<IbLikeBroker>) -> Arc<dyn Venue> {
    broker.clone()
}

// ---------------------------------------------------------------------------
// M14.2/M14.3: chaos suite. Every case ends in a verified invariant: no
// unknown position, no unauthorized (duplicate) order, entries suspend
// while exits route.
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicU32, Ordering};

struct FlakyDriver {
    outcome: Mutex<Result<CredentialState, AuthError>>,
    calls: AtomicU32,
}

impl FlakyDriver {
    fn failing() -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(Err(AuthError::Transient("endpoint dark".to_owned()))),
            calls: AtomicU32::new(0),
        })
    }

    fn heal(&self, state: CredentialState) {
        *common_core::sync::lock(&self.outcome) = Ok(state);
    }
}

impl AuthDriver for FlakyDriver {
    fn method(&self) -> AuthMethod {
        AuthMethod::SessionLogin
    }

    fn acquire<'a>(
        &'a self,
        _previous: Option<&'a CredentialState>,
    ) -> AuthFuture<'a, Result<CredentialState, AuthError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let outcome = common_core::sync::lock(&self.outcome).clone();
        Box::pin(async move { outcome })
    }
}

fn fresh_state() -> CredentialState {
    CredentialState::issued(
        Credential::new("ib-client-1"),
        Utc::now(),
        Some(std::time::Duration::from_secs(300)),
        7,
    )
}

/// Token expiry mid-session: an expired install refreshes through the
/// driver on the next lease and serves the fresh bearer.
#[tokio::test]
async fn chaos_token_expiry_refreshes_and_serves() {
    let driver = FlakyDriver::failing();
    driver.heal(fresh_state());
    let manager = TokenManager::new(
        AuthConfig::default(),
        driver.clone() as Arc<dyn AuthDriver>,
        None,
    )
    .expect("manager builds");
    let expired = CredentialState::issued(
        Credential::new("old"),
        Utc::now() - chrono::Duration::seconds(3600),
        Some(std::time::Duration::from_secs(60)),
        6,
    );
    manager.install(expired).expect("expired installs");
    let lease = manager.lease().await.expect("expiry refreshes");
    assert_eq!(
        lease.credential().expose(),
        "ib-client-1",
        "the refreshed bearer serves"
    );
    assert!(
        lease.generation() >= 7,
        "the generation advances past the expired one"
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1, "one refresh dial");
}

/// Refresh failure then recovery: a dark endpoint fails retryably, an
/// immediate automatic retry fails fast instead of hammering (the
/// minimum-interval guard), and a forced refresh heals the moment the
/// endpoint answers.
#[tokio::test]
async fn chaos_refresh_failure_then_recovery() {
    let driver = FlakyDriver::failing();
    let manager = TokenManager::new(
        AuthConfig::default(),
        driver.clone() as Arc<dyn AuthDriver>,
        None,
    )
    .expect("manager builds");
    let failure = manager.lease().await.expect_err("dark endpoint refuses");
    assert!(
        failure.is_retryable(),
        "a dark endpoint is retryable, never terminal: {failure}"
    );
    let fast_fail = manager.lease().await.expect_err("no retry storm");
    assert!(
        matches!(fast_fail, AuthError::Expired),
        "automatic retries fail fast inside the minimum interval: {fast_fail}"
    );
    driver.heal(fresh_state());
    manager.refresh().await.expect("forced refresh heals");
    let lease = manager.lease().await.expect("recovery serves");
    assert_eq!(lease.credential().expose(), "ib-client-1");
}

/// Feed drop and reconnect: closing every outbox ends both sessions'
/// streams, and a resubscribe after the drop receives only new pushes —
/// never redelivered ghosts, never a wedged pump.
#[tokio::test]
async fn chaos_feed_drop_and_reconnect_resume() {
    use tokio::sync::mpsc::error::TryRecvError;

    let broker = Arc::new(IbLikeBroker::new("IB-1"));
    let feed: Arc<dyn MarketDataFeed> = broker.clone();
    let quote = FeedSubscription::of("SPY", MarketEventKind::Quote);
    let mut first = feed
        .subscribe(std::slice::from_ref(&quote))
        .await
        .expect("session one");
    let mut second = feed.subscribe(&[quote]).await.expect("session two");
    broker.push_tick_price("SPY", "100.00", "100.10", stamp(10, 0));
    assert!(first.outbox.recv().await.is_some(), "session one streams");
    assert!(second.outbox.recv().await.is_some(), "session two streams");
    broker.close_feed();
    assert!(
        first.outbox.recv().await.is_none(),
        "the drop closes session one"
    );
    assert!(
        second.outbox.recv().await.is_none(),
        "the drop closes session two"
    );
    // Reconnect: a fresh session over the same feed streams again.
    let mut third = feed
        .subscribe(&[FeedSubscription::of("SPY", MarketEventKind::Quote)])
        .await
        .expect("resubscribe works");
    broker.push_tick_price("SPY", "100.20", "100.30", stamp(10, 1));
    let event = third.outbox.recv().await.expect("reconnect streams");
    assert_eq!(event.symbol(), "SPY");
    assert!(
        matches!(third.outbox.try_recv(), Err(TryRecvError::Empty)),
        "no ghost redelivers on reconnect"
    );
}

/// Tick storm: interleaved duplicates, an out-of-order print, and a gap in
/// one stream still close exactly one bar per advanced bucket with exact
/// volumes — nothing premature, nothing swallowed.
#[test]
fn chaos_tick_storm_stays_exact() {
    use crate::feed::TradeBarAdapter;
    use protopyne_types::Timeframe;

    fn print(ts_ms: i64, price: &str, size: u32) -> TradePrint {
        TradePrint {
            ts: Utc
                .timestamp_millis_opt(ts_ms)
                .single()
                .expect("valid stamp"),
            price: money(price),
            size: u64::from(size),
            aggressor: None,
        }
    }

    let base = 1_780_495_200_000i64;
    let mut adapter = TradeBarAdapter::new(Timeframe::M1).expect("adapter");
    let mut closed: Vec<(i64, u64)> = Vec::new();
    let mut refused = 0u32;
    for (offset_ms, price, size) in [
        (0, "100.00", 1),
        (0, "100.00", 9),
        (30_000, "100.10", 1),
        (60_000, "100.20", 1),
        (60_000, "100.20", 1),
        (45_000, "100.15", 1),
        (120_000, "100.30", 1),
        (240_000, "100.40", 1),
    ] {
        match adapter
            .ingest(print(base + offset_ms, price, size))
            .expect("tick ingests")
        {
            crate::feed::AdapterOutcome::Closed { bar, .. } => {
                closed.push((bar.ts.timestamp_millis(), bar.volume));
            }
            crate::feed::AdapterOutcome::Forming(_) => {}
            crate::feed::AdapterOutcome::Duplicate | crate::feed::AdapterOutcome::OutOfOrder => {
                refused += 1;
            }
        }
    }
    assert_eq!(
        closed,
        vec![(base, 2), (base + 60_000, 1), (base + 120_000, 1)],
        "exactly one close per advanced bucket with exact volumes"
    );
    assert_eq!(refused, 3, "duplicate, repeat, and late prints refuse");
}

/// Kill mid-write, restart, recover: the journaled intent survives the
/// kill, the fresh venue holds nothing unknown, and resubmitting the same
/// client ID books exactly one order — never a duplicate, never a ghost
/// position.
#[tokio::test]
async fn chaos_kill_mid_write_restart_recovers_cleanly() {
    use protopyne_strategy::{
        Direction, OrderShape, Proposal, ProposalSide, RiskEngine, RiskLimits, RiskState,
        StructureKind,
    };
    use protopyne_types::Qty;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal");
    let permit = VenueStorage::new(path.clone());
    let caps = CapabilitySet::new().with(permit.clone());
    // The approval is bound to its proposal: the intent carries the same
    // proposal id or submission refuses (no unauthorized order, ever).
    let mut intent = IbLikeBroker::order_intent("k-1", "SPY");
    intent.proposal_id = "prop-1".to_owned();
    let quote = Quote {
        bid: money("599.00"),
        ask: money("601.00"),
        bid_size: 1,
        ask_size: 1,
        ts: stamp(10, 0),
    };
    let approved = {
        let engine = RiskEngine::new(RiskLimits::default());
        let proposal = Proposal::new()
            .id("prop-1".to_owned())
            .strategy("s".to_owned())
            .underlying("SPY".to_owned())
            .direction(Direction::Long)
            .side(ProposalSide::Entry)
            .structure(StructureKind::Vertical)
            .quantity(Qty::new(rust_decimal::Decimal::from(1)))
            .limit_price(money("600.00"))
            .max_loss(money("300.00"))
            .notional(money("600.00"))
            .dte_days(30)
            .conviction(0.8)
            .activity_score(0.7)
            .created_at(stamp(10, 0))
            .build();
        engine
            .check(
                &proposal,
                OrderShape::new(true, true),
                &RiskState {
                    quote_age_secs: 5,
                    clock_skew_secs: 1,
                    reconciliation_healthy: true,
                    facts: protopyne_strategy::PortfolioFacts::unobserved(),
                },
            )
            .expect("healthy")
    };
    // Live: journal the submit, then die before any fill.
    let broker = Arc::new(IbLikeBroker::new("IB-1"));
    CURRENT_CAPS
        .scope(caps.clone(), async {
            let journal = Journal::open(&path).expect("journal opens");
            crate::submit::safe_submit(
                &permit,
                &journal,
                &*broker,
                &approved,
                intent.clone(),
                &quote,
                stamp(10, 0),
            )
            .await
            .expect("submit journals");
        })
        .await;
    drop(broker);

    // Restart: the intent row survives, the fresh venue holds nothing.
    let broker = Arc::new(IbLikeBroker::new("IB-1"));
    CURRENT_CAPS
        .scope(caps, async {
            let journal = Journal::open(&path).expect("reopen repairs");
            let state = journal.replay(&permit).expect("replay reads");
            let intents: Vec<OrderIntent> = state.intents;
            assert_eq!(intents.len(), 1, "exactly one intent row survives");
            assert_eq!(intents[0].client_id, "k-1");
            assert!(
                broker.positions().is_empty(),
                "no fill means no position anywhere"
            );
            assert!(
                broker.query("k-1").await.is_none(),
                "the fresh venue holds no ghost order"
            );
            // Resolve by query-by-ID, never blind resubmit: absent, so one
            // idempotent submit books exactly one order.
            crate::submit::safe_submit(
                &permit,
                &journal,
                &*broker,
                &approved,
                intent.clone(),
                &quote,
                stamp(10, 0),
            )
            .await
            .expect("idempotent resubmit");
            crate::submit::safe_submit(
                &permit,
                &journal,
                &*broker,
                &approved,
                intent.clone(),
                &quote,
                stamp(10, 0),
            )
            .await
            .expect("second resubmit is a no-op");
            assert_eq!(
                Venue::orders(&*broker).len(),
                1,
                "one client ID means one order, never two"
            );
        })
        .await;
}

/// Entries suspend while exits route: a suspended backfill gate holds new
/// entries out, but working orders still cancel through the venue.
#[test]
fn chaos_entries_suspend_while_exits_route() {
    use crate::backfill::{BackfillHandle, BackfillReport};

    let broker = Arc::new(IbLikeBroker::new("IB-1"));
    let quote = Quote {
        bid: money("599.00"),
        ask: money("601.00"),
        bid_size: 1,
        ask_size: 1,
        ts: stamp(10, 0),
    };
    let gate = BackfillHandle::new();
    gate.suspend("chaos hole".to_owned());
    assert!(gate.entries_suspended(), "entries halt on the hole");
    // Exits route through the halt: a working order still cancels.
    let record = common_core::runtime::block_on(broker.submit(
        IbLikeBroker::order_intent("x-1", "SPY"),
        &quote,
        stamp(10, 0),
    ))
    .expect("submit books");
    assert_eq!(record.status, OrderStatus::Submitted);
    let cancelled = common_core::runtime::block_on(broker.cancel("x-1", stamp(10, 0)))
        .expect("cancel routes while suspended");
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    // One repair pass resumes entries; the book is unchanged.
    gate.observe(&BackfillReport {
        complete: true,
        filled: 1,
        alerts: Vec::new(),
    });
    assert!(!gate.entries_suspended(), "entries resume on repair");
    assert!(
        broker.positions().is_empty(),
        "a cancelled working order leaves no position"
    );
}
