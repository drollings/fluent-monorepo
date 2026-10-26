//! Broker reads end to end: a fake account reader behind registered
//! bundles, served through the additive `/broker/...` routes and the
//! gateway's broker methods.
//!
//! Unsupported surfaces refuse on both paths (never empty lists), accounts
//! route to the bundle that serves them, and registering a broker changes
//! no existing route's bytes.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use protopyne_types::Money;
use protopyne_venue::{
    AccountError, AccountFuture, AccountRange, AccountReader, AccountSummary, BrokerBundle,
    BrokerCapabilities, BrokerDeal, BrokerId, Fill, OrderIntent, OrderRecord, OrderSide,
    PaperConfig, PaperVenue, Venue,
};
use protopyne_view::{GatewayError, MockGateway, TerminalGateway};

use crate::common_fixtures::{money, spy_history};
use crate::service::{serve_with_tape, ServerConfig};

fn stamp(hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 5, hour, 0, 0)
        .single()
        .expect("valid stamp")
}

fn summary(account: &str, net: &str) -> AccountSummary {
    AccountSummary {
        account: account.to_owned(),
        venue: "fake".to_owned(),
        currency: "USD".to_owned(),
        net_liquid: money(net),
        buying_power: money("500.00"),
        margin_used: Money::zero(),
        observed_at: stamp(10),
    }
}

fn order(client: &str, hour: u32) -> OrderRecord {
    OrderRecord::pending(
        OrderIntent::new()
            .client_id(client.to_owned())
            .proposal_id("p-1".to_owned())
            .strategy("s".to_owned())
            .underlying("SPY".to_owned())
            .side(OrderSide::Buy)
            .quantity(1)
            .limit_price(money("600.00"))
            .build(),
        stamp(hour),
    )
}

fn deal(id: &str, hour: u32, side: OrderSide) -> BrokerDeal {
    let at = stamp(hour);
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

/// A seeded account reader: accounts, summaries, orders, and deals by
/// account id, windowed by the requested range. Surfaces outside `caps`
/// refuse with [`AccountError::Unsupported`], never empty lists.
struct FakeReader {
    accounts: Vec<String>,
    summaries: HashMap<String, AccountSummary>,
    orders: HashMap<String, Vec<OrderRecord>>,
    deals: HashMap<String, Vec<BrokerDeal>>,
    caps: BrokerCapabilities,
}

impl FakeReader {
    fn full() -> Self {
        Self {
            accounts: vec!["A-1".to_owned()],
            summaries: HashMap::from([("A-1".to_owned(), summary("A-1", "1000.00"))]),
            orders: HashMap::from([(
                "A-1".to_owned(),
                vec![order("c-1", 10), order("c-2", 12), order("c-3", 14)],
            )]),
            deals: HashMap::from([(
                "A-1".to_owned(),
                vec![
                    deal("d-1", 10, OrderSide::Buy),
                    deal("d-2", 12, OrderSide::Sell),
                    deal("d-3", 14, OrderSide::Buy),
                ],
            )]),
            caps: BrokerCapabilities {
                accounts: true,
                orders: true,
                deals: true,
                ..BrokerCapabilities::default()
            },
        }
    }

    fn without_deals() -> Self {
        let mut reader = Self::full();
        reader.caps.deals = false;
        reader
    }
}

impl AccountReader for FakeReader {
    fn capabilities(&self) -> BrokerCapabilities {
        self.caps
    }

    fn accounts(&self) -> AccountFuture<'_, Result<Vec<String>, AccountError>> {
        let accounts = self.accounts.clone();
        Box::pin(async move { Ok(accounts) })
    }

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

    fn orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        Box::pin(async move {
            if !self.caps.orders {
                return Err(AccountError::Unsupported("orders".to_owned()));
            }
            Ok(range.window(
                self.orders
                    .get(account)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|record| (record.updated_at, record)),
            ))
        })
    }

    fn open_orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        self.orders(account, range)
    }

    fn deals<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<BrokerDeal>, AccountError>> {
        Box::pin(async move {
            if !self.caps.deals {
                return Err(AccountError::Unsupported("deals".to_owned()));
            }
            Ok(range.window(
                self.deals
                    .get(account)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|deal| (deal.executed_at, deal)),
            ))
        })
    }
}

fn paper_venue() -> Arc<dyn Venue> {
    Arc::new(PaperVenue::new(PaperConfig::new().build()))
}

/// Run a blocking gateway read from a plain OS thread: the gateway bridges
/// sync callers through the shared runtime, which refuses to drive from
/// inside an async runtime (like this test's). The production caller is the
/// UI thread, which has no runtime either.
fn on_sync_thread<T>(run: impl FnOnce() -> T + Send + 'static) -> T
where
    T: Send + 'static,
{
    std::thread::spawn(run).join().expect("sync thread joins")
}

fn gateway_of(handle: &crate::service::ServerHandle) -> crate::gateway::ServerGateway {
    crate::gateway::ServerGateway::new(std::sync::Arc::clone(&handle.state))
}

fn bundle(id: &str, reader: FakeReader) -> BrokerBundle {
    BrokerBundle::new(BrokerId::new(id).expect("broker id"), paper_venue())
        .with_account(Arc::new(reader))
}

async fn boot() -> crate::service::ServerHandle {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    serve_with_tape(config, spy_history(), None, None)
        .await
        .expect("test server boots")
}

async fn get_json(client: &reqwest::Client, url: &str) -> serde_json::Value {
    client
        .get(url)
        .send()
        .await
        .expect("route reachable")
        .json()
        .await
        .expect("route is JSON")
}

/// A registered bundle serves every broker surface through the routes, and
/// the gateway reads the identical views.
#[tokio::test]
async fn broker_routes_serve_a_registered_bundle() {
    let handle = boot().await;
    handle
        .state
        .broker
        .register(bundle("fake", FakeReader::full()))
        .expect("bundle registers");
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let venues: Vec<protopyne_view::BrokerCapabilityView> =
        serde_json::from_value(get_json(&client, &format!("{base}/broker/venues")).await)
            .expect("venues decode");
    assert_eq!(venues.len(), 1);
    assert_eq!(venues[0].venue, "fake");
    assert!(venues[0].accounts && venues[0].orders && venues[0].deals);
    assert_eq!(
        on_sync_thread({
            let gateway = gateway_of(&handle);
            move || gateway.broker_venues()
        })
        .expect("gateway venues"),
        venues,
        "the gateway reads the same venues as the route"
    );

    let accounts: Vec<protopyne_view::BrokerAccountView> =
        serde_json::from_value(get_json(&client, &format!("{base}/broker/accounts")).await)
            .expect("accounts decode");
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].venue, "fake");
    assert_eq!(accounts[0].account, "A-1");
    assert_eq!(accounts[0].net_liquid, money("1000.00"));
    assert_eq!(
        on_sync_thread({
            let gateway = gateway_of(&handle);
            move || gateway.broker_accounts()
        })
        .expect("gateway accounts"),
        accounts,
        "the gateway reads the same accounts as the route"
    );

    let account: protopyne_view::BrokerAccountView =
        serde_json::from_value(get_json(&client, &format!("{base}/broker/accounts/A-1")).await)
            .expect("account decodes");
    assert_eq!(account, accounts[0]);
    assert_eq!(
        on_sync_thread({
            let gateway = gateway_of(&handle);
            move || gateway.broker_account("A-1")
        })
        .expect("gateway account"),
        account
    );

    let orders: Vec<protopyne_view::BrokerOrderView> = serde_json::from_value(
        get_json(&client, &format!("{base}/broker/accounts/A-1/orders")).await,
    )
    .expect("orders decode");
    assert_eq!(orders.len(), 3);
    assert!(orders.iter().all(|order| order.venue == "fake"));
    assert_eq!(orders[0].client_id, "c-1");
    assert_eq!(
        on_sync_thread({
            let gateway = gateway_of(&handle);
            move || gateway.broker_orders("A-1")
        })
        .expect("gateway orders"),
        orders,
        "the gateway reads the same orders as the route"
    );

    let deals: Vec<protopyne_view::BrokerDealView> = serde_json::from_value(
        get_json(&client, &format!("{base}/broker/accounts/A-1/deals")).await,
    )
    .expect("deals decode");
    assert_eq!(deals.len(), 3);
    assert_eq!(deals[0].deal_id, "d-1");
    assert_eq!(deals[1].side, "Sell");
    assert_eq!(deals[2].quantity, 2);
    assert_eq!(
        on_sync_thread({
            let gateway = gateway_of(&handle);
            move || gateway.broker_deals("A-1")
        })
        .expect("gateway deals"),
        deals,
        "the gateway reads the same deals as the route"
    );
    handle.shutdown().await;
}

/// An unimplemented surface refuses on both paths with the absence named —
/// never an empty list. The implemented surfaces keep serving.
#[tokio::test]
async fn unsupported_deals_refuse_on_route_and_gateway() {
    let handle = boot().await;
    handle
        .state
        .broker
        .register(bundle("nodeals", FakeReader::without_deals()))
        .expect("bundle registers");
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{base}/broker/accounts/A-1/deals"))
        .send()
        .await
        .expect("route reachable");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::NOT_IMPLEMENTED,
        "an unsupported surface refuses as unimplemented"
    );
    let body: serde_json::Value = response.json().await.expect("error is JSON");
    assert!(
        body.to_string().contains("deals"),
        "the refusal names the surface: {body}"
    );
    let refused = on_sync_thread({
        let gateway = gateway_of(&handle);
        move || gateway.broker_deals("A-1")
    });
    assert!(
        matches!(refused, Err(GatewayError::Unavailable(_))),
        "the gateway refuses the same surface"
    );

    // The implemented surfaces are unaffected by the missing one.
    let accounts: Vec<protopyne_view::BrokerAccountView> =
        serde_json::from_value(get_json(&client, &format!("{base}/broker/accounts")).await)
            .expect("accounts decode");
    assert_eq!(accounts.len(), 1, "summaries still serve");
    let orders: Vec<protopyne_view::BrokerOrderView> = serde_json::from_value(
        get_json(&client, &format!("{base}/broker/accounts/A-1/orders")).await,
    )
    .expect("orders decode");
    assert_eq!(orders.len(), 3, "orders still serve");
    handle.shutdown().await;
}

/// Ranges window by execution stamp and limits truncate oldest-first, on
/// both orders and deals.
#[tokio::test]
async fn reader_windows_and_limits_orders_and_deals() {
    let reader = FakeReader::full();
    let windowed = reader
        .orders(
            "A-1",
            AccountRange {
                start: Some(stamp(11)),
                end: Some(stamp(13)),
                limit: None,
            },
        )
        .await
        .expect("windowed orders");
    assert_eq!(windowed.len(), 1, "only the noon order falls in the window");
    assert_eq!(windowed[0].intent.client_id, "c-2");
    let capped = reader
        .orders(
            "A-1",
            AccountRange {
                limit: Some(2),
                ..AccountRange::all()
            },
        )
        .await
        .expect("limited orders");
    assert_eq!(capped.len(), 2, "the limit truncates oldest-first");
    let windowed = reader
        .deals(
            "A-1",
            AccountRange {
                start: Some(stamp(13)),
                end: None,
                limit: None,
            },
        )
        .await
        .expect("windowed deals");
    assert_eq!(
        windowed.len(),
        1,
        "only the afternoon deal falls in the window"
    );
    assert_eq!(windowed[0].deal_id, "d-3");
}

/// Accounts route to the bundle that serves them; unknown accounts refuse
/// on both paths.
#[tokio::test]
async fn multi_account_routing_serves_each_from_its_bundle() {
    let handle = boot().await;
    let mut first = FakeReader::full();
    first.summaries = HashMap::from([("A-1".to_owned(), summary("A-1", "1000.00"))]);
    let mut second = FakeReader::full();
    second.accounts = vec!["B-9".to_owned()];
    second.summaries = HashMap::from([("B-9".to_owned(), summary("B-9", "250.00"))]);
    second.orders = HashMap::from([("B-9".to_owned(), vec![order("c-9", 11)])]);
    second.deals = HashMap::new();
    handle
        .state
        .broker
        .register(bundle("fake-1", first))
        .expect("first registers");
    handle
        .state
        .broker
        .register(bundle("fake-2", second))
        .expect("second registers");
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    let first: protopyne_view::BrokerAccountView =
        serde_json::from_value(get_json(&client, &format!("{base}/broker/accounts/A-1")).await)
            .expect("first account decodes");
    assert_eq!(first.venue, "fake-1");
    let second: protopyne_view::BrokerAccountView =
        serde_json::from_value(get_json(&client, &format!("{base}/broker/accounts/B-9")).await)
            .expect("second account decodes");
    assert_eq!(second.venue, "fake-2");
    assert_eq!(second.net_liquid, money("250.00"));
    assert_eq!(
        on_sync_thread({
            let gateway = gateway_of(&handle);
            move || gateway.broker_account("B-9")
        })
        .expect("gateway routes"),
        second
    );

    let missing = client
        .get(format!("{base}/broker/accounts/ZZ-0"))
        .send()
        .await
        .expect("route reachable");
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    let missing_gateway = on_sync_thread({
        let gateway = gateway_of(&handle);
        move || gateway.broker_account("ZZ-0")
    });
    assert!(
        matches!(missing_gateway, Err(GatewayError::Unavailable(_))),
        "the gateway names the unserved account"
    );
    handle.shutdown().await;
}

/// Registering a broker changes no existing route's bytes: `/health`
/// serves identically with and without the directory populated.
#[tokio::test]
async fn existing_health_stays_byte_identical_with_a_broker_registered() {
    let plain = boot().await;
    let plain_body = reqwest::Client::new()
        .get(format!("http://{}/health", plain.addr))
        .send()
        .await
        .expect("route reachable")
        .text()
        .await
        .expect("body reads");
    plain.shutdown().await;

    let wired = boot().await;
    wired
        .state
        .broker
        .register(bundle("fake", FakeReader::full()))
        .expect("bundle registers");
    let wired_body = reqwest::Client::new()
        .get(format!("http://{}/health", wired.addr))
        .send()
        .await
        .expect("route reachable")
        .text()
        .await
        .expect("body reads");
    wired.shutdown().await;
    assert_eq!(
        wired_body, plain_body,
        "the broker directory adds routes, never changes them"
    );
}

/// An empty directory refuses on every broker route, and the served gateway
/// agrees with the mock on the absence: both name it, neither answers empty.
#[tokio::test]
async fn empty_registry_refuses_broker_routes_and_matches_the_mock() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let mock = MockGateway::new();

    for path in ["/broker/venues", "/broker/accounts"] {
        let response = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .expect("route reachable");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "{path} refuses without a broker"
        );
        let body: serde_json::Value = response.json().await.expect("error is JSON");
        assert!(
            body.to_string().contains("no broker is registered"),
            "{path} names the absence: {body}"
        );
    }
    for path in [
        "/broker/accounts/A-1",
        "/broker/accounts/A-1/orders",
        "/broker/accounts/A-1/deals",
    ] {
        let response = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .expect("route reachable");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path} refuses without a broker"
        );
    }
    for name in ["venues", "accounts", "account", "orders", "deals"] {
        let served = on_sync_thread({
            let gateway = gateway_of(&handle);
            move || match name {
                "venues" => gateway.broker_venues().map(|_| ()),
                "accounts" => gateway.broker_accounts().map(|_| ()),
                "account" => gateway.broker_account("A-1").map(|_| ()),
                "orders" => gateway.broker_orders("A-1").map(|_| ()),
                _ => gateway.broker_deals("A-1").map(|_| ()),
            }
        });
        let mocked = match name {
            "venues" => mock.broker_venues().map(|_| ()),
            "accounts" => mock.broker_accounts().map(|_| ()),
            "account" => mock.broker_account("A-1").map(|_| ()),
            "orders" => mock.broker_orders("A-1").map(|_| ()),
            _ => mock.broker_deals("A-1").map(|_| ()),
        };
        assert!(
            matches!(served, Err(GatewayError::Unavailable(_))),
            "the served gateway refuses {name} without a broker"
        );
        assert!(
            matches!(mocked, Err(GatewayError::Unavailable(_))),
            "the mock refuses {name} the same way"
        );
    }
    handle.shutdown().await;
}

/// A reader whose deals surface goes dark and recovers: the outage is
/// transient (advertised, then failing), never an unadvertised absence.
struct DarkDeals {
    inner: FakeReader,
    dark: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl AccountReader for DarkDeals {
    fn capabilities(&self) -> BrokerCapabilities {
        BrokerCapabilities {
            deals: true,
            ..self.inner.caps
        }
    }

    fn accounts(&self) -> AccountFuture<'_, Result<Vec<String>, AccountError>> {
        self.inner.accounts()
    }

    fn summary<'a>(
        &'a self,
        account: &'a str,
    ) -> AccountFuture<'a, Result<AccountSummary, AccountError>> {
        self.inner.summary(account)
    }

    fn orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        self.inner.orders(account, range)
    }

    fn open_orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        self.inner.open_orders(account, range)
    }

    fn deals<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<BrokerDeal>, AccountError>> {
        Box::pin(async move {
            if self.dark.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(AccountError::Transient("broker read outage".to_owned()));
            }
            self.inner.deals(account, range).await
        })
    }
}

/// A broker read outage refuses with 503 on the route and `Unavailable`
/// through the gateway, then serves again after the heal — no restart, no
/// re-registration.
#[tokio::test]
async fn broker_read_outage_refuses_then_recovers() {
    let handle = boot().await;
    let dark = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    handle
        .state
        .broker
        .register(
            BrokerBundle::new(BrokerId::new("flaky").expect("id"), paper_venue()).with_account(
                std::sync::Arc::new(DarkDeals {
                    inner: FakeReader::full(),
                    dark: dark.clone(),
                }),
            ),
        )
        .expect("bundle registers");
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    let outage = client
        .get(format!("{base}/broker/accounts/A-1/deals"))
        .send()
        .await
        .expect("route reachable");
    assert_eq!(
        outage.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "a dark read is a 503, not an empty list"
    );
    let refused = on_sync_thread({
        let gateway = gateway_of(&handle);
        move || gateway.broker_deals("A-1")
    });
    assert!(
        matches!(refused, Err(GatewayError::Unavailable(_))),
        "the gateway names the outage too"
    );

    // Heal: the very same bundle serves without re-registration.
    dark.store(false, std::sync::atomic::Ordering::SeqCst);
    let deals: Vec<protopyne_view::BrokerDealView> = client
        .get(format!("{base}/broker/accounts/A-1/deals"))
        .send()
        .await
        .expect("route reachable")
        .json()
        .await
        .expect("deals decode");
    assert_eq!(deals.len(), 3, "the healed read serves every deal");
    let via_gateway = on_sync_thread({
        let gateway = gateway_of(&handle);
        move || gateway.broker_deals("A-1")
    })
    .expect("gateway reads after the heal");
    assert_eq!(via_gateway, deals, "route and gateway agree after recovery");
    handle.shutdown().await;
}
