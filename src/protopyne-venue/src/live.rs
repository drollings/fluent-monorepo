//! Authenticated live venue: order entry, query-by-client-ID, and dry-run
//! over the tastytrade REST surface, behind the existing [`Venue`] trait.
//!
//! The safety case is unchanged from paper — journal before venue calls,
//! query-by-ID instead of blind resubmit — only the truth source moves:
//! [`PaperVenue`](crate::paper::PaperVenue) answers from its in-memory book
//! while [`TastytradeVenue`] answers over REST through [`NetCapability`].
//! Every order call is query-first (a known client ID returns its record
//! before any POST, mirroring paper's submit shape), so a retry after a
//! lost acknowledgement resolves to the original order, never a second one.
//!
//! Authentication is a session token, never stored credentials: [`TastytradeVenue::login`]
//! POSTs the caller's username/password once, banks the token, and every
//! later call carries it as an `Authorization` header. Calls without a
//! token refuse (or, for [`Venue::query`], report absent) without dialing.
//! Credentials arrive per call from the production boot path (environment,
//! never config files); no credential field exists on any config struct.
//!
//! Truth mirrors: `positions`/`orders` serve last-observed venue state —
//! every successful mutation refolds the book, and [`TastytradeVenue::apply_account_event`]
//! folds stream events the same way — because the sync trait methods cannot
//! dial. Drive observations through `query` (or the account stream) before
//! reading truth you must trust; reconciliation already does.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use fluent_concurrency::io::net::NetCapability;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::validate::Validate;
use tokio::sync::mpsc;

use crate::auth::{
    AuthConfig, AuthError, AuthHandle, CredentialSource, CredentialStore, FileCredentialStore,
    TastytradeSessionDriver, TokenManager,
};
use crate::backfill::BackfillSource;
use crate::broker::{BrokerBundle, BrokerError, BrokerId, BrokerRegistry};
use crate::error::VenueError;
use crate::facts::VenueBookFacts;
use crate::journal::{Journal, VenueStorage};
use crate::order::{
    CorporateEventArgs, CorporateOutcome, Fill, OrderIntent, OrderRecord, OrderSide, Position,
};
use crate::quote::Quote;
use crate::reconcile::{journal_leg_net, JournalBook, ReconcileSource};
use crate::registry::VenueRegistry;
use crate::session::{SessionConfig, TradingSession};
use crate::stream::{StreamConfig, SupervisedStream};
use crate::tastytrade::{
    order_intent_of, order_record_of, order_wire_of, TastytradeConfig, TastytradeTransport,
    TransportError, ORDER_WIRE_VERSION, ORDER_WIRE_VERSION_HEADER,
};
use crate::tastytrade_account::TastytradeAccountReader;
use crate::venue::{AccountEvent, BalanceEntry, DryRunOutcome, Venue};
use protopyne_strategy::PortfolioFactsSource;
use protopyne_types::{Money, SeriesContext};

/// Session-token venue over REST. Shares the transport's quote cache for
/// the [`QuoteSource`](crate::venue::QuoteSource) ladder; order truth lives
/// here, in the refolded mirror.
pub struct TastytradeVenue {
    transport: Arc<TastytradeTransport>,
    net: Arc<NetCapability>,
    account: String,
    token: Arc<TokenManager>,
    driver: Arc<TastytradeSessionDriver>,
    book: Mutex<HashMap<String, OrderRecord>>,
    positions: Mutex<HashMap<String, Position>>,
    balances: Mutex<Vec<BalanceEntry>>,
}

/// Percent-encode one URL path segment. Client IDs carry `#`
/// ([`client_id_for`](crate::order::client_id_for)), which would otherwise
/// truncate at the fragment boundary — every request after login would
/// address the wrong order. Unreserved bytes pass through; everything else
/// becomes `%XX`.
fn encode_segment(segment: &str) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

impl TastytradeVenue {
    /// Bind a venue to its transport, capability, and account. Refuses an
    /// empty account or an unconfigured base URL without dialing.
    pub fn new(
        transport: Arc<TastytradeTransport>,
        net: Arc<NetCapability>,
        account: String,
    ) -> Result<Self, VenueError> {
        Self::with_auth(transport, net, account, AuthConfig::default(), None)
    }

    /// Bind a venue with an explicit credential policy and optional store.
    pub fn with_auth(
        transport: Arc<TastytradeTransport>,
        net: Arc<NetCapability>,
        account: String,
        auth: AuthConfig,
        store: Option<Arc<dyn CredentialStore>>,
    ) -> Result<Self, VenueError> {
        if transport.config().base_url.is_empty() {
            return Err(VenueError::Invalid(
                "tastytrade base URL unconfigured: refusing to dial".to_owned(),
            ));
        }
        if account.is_empty() {
            return Err(VenueError::Invalid(
                "tastytrade account is empty".to_owned(),
            ));
        }
        let credentials = Arc::new(Mutex::new(None));
        let driver = Arc::new(TastytradeSessionDriver::new(
            transport.config().base_url.clone(),
            Arc::clone(&net),
            Arc::clone(&credentials),
            auth.ttl_secs,
        ));
        let token = Arc::new(
            TokenManager::new(auth, driver.clone(), store)
                .map_err(|error| VenueError::Invalid(format!("auth manager refused: {error}")))?,
        );
        Ok(Self {
            transport,
            net,
            account,
            token,
            driver,
            book: Mutex::new(HashMap::new()),
            positions: Mutex::new(HashMap::new()),
            balances: Mutex::new(Vec::new()),
        })
    }

    /// Borrow the credential authority.
    #[must_use]
    pub fn token_manager(&self) -> &Arc<TokenManager> {
        &self.token
    }

    /// Read-only account surface over this venue's transport and mirrors.
    /// The reader borrows the same authenticated venue object — no second
    /// credential owner, no second transport.
    #[must_use]
    pub fn account_reader(self: &Arc<Self>) -> TastytradeAccountReader {
        TastytradeAccountReader::new(Arc::clone(self), self.account.clone())
    }

    /// Whether a usable session credential is held.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.token.is_authenticated()
    }

    /// Drop the banked token. Later order calls refuse until [`TastytradeVenue::login`].
    pub fn logout(&self) {
        self.driver.clear_credentials();
        let _ = self.token.clear();
    }

    /// Borrow the quote transport (ladder rung + refresh).
    #[must_use]
    pub fn transport(&self) -> &Arc<TastytradeTransport> {
        &self.transport
    }

    /// Borrow the network capability.
    #[must_use]
    pub fn net(&self) -> &Arc<NetCapability> {
        &self.net
    }

    /// The venue account identifier.
    #[must_use]
    pub fn account_id(&self) -> &str {
        &self.account
    }

    /// Authenticate: one driver acquisition, credential banked on success.
    /// Empty credentials refuse without dialing; a venue refusal (bad login)
    /// is terminal and never retried; transient failures retry bounded by
    /// [`AuthConfig`]. At most one credential is ever banked.
    pub async fn login(&self, username: &str, password: &str) -> Result<(), VenueError> {
        if username.is_empty() || password.is_empty() {
            return Err(VenueError::Invalid(
                "tastytrade credentials are empty".to_owned(),
            ));
        }
        self.driver.set_credentials(username, password);
        self.token
            .refresh()
            .await
            .map(|_| ())
            .map_err(auth_into_venue)
    }

    /// Fold one account-stream event into the mirrors. Orders upsert (then
    /// the positions refold); balances upsert by asset; trades are
    /// bookkeeping-neutral here — their fills already ride inside the
    /// `OrderUpdated` records, so counting them again would double-apply.
    pub fn apply_account_event(&self, event: &AccountEvent) {
        match event {
            AccountEvent::BalanceUpdated(entry) => {
                let mut balances = common_core::sync::lock(&self.balances);
                if let Some(line) = balances.iter_mut().find(|line| line.asset == entry.asset) {
                    line.amount = entry.amount;
                } else {
                    balances.push(entry.clone());
                }
            }
            AccountEvent::OrderUpdated(record) => {
                let mut book = common_core::sync::lock(&self.book);
                book.insert(record.intent.client_id.clone(), record.clone());
                *common_core::sync::lock(&self.positions) = positions_of(book.values().cloned());
            }
            AccountEvent::Trade(_) => {}
        }
    }

    /// Parse one venue order envelope into a record; the intent is rebuilt
    /// from the wire alone (attribution the venue never carries reads empty).
    pub fn parse_order_record(body: &str) -> Result<OrderRecord, String> {
        let wire: serde_json::Value =
            serde_json::from_str(body).map_err(|e| format!("order wire is not JSON: {e}"))?;
        let intent = order_intent_of(&wire).map_err(|e| e.to_string())?;
        order_record_of(&wire, intent).map_err(|e| e.to_string())
    }

    /// Parse one venue order envelope against the known `intent`, keeping the
    /// caller's attribution.
    pub fn order_record_with(body: &str, intent: OrderIntent) -> Result<OrderRecord, String> {
        let wire: serde_json::Value =
            serde_json::from_str(body).map_err(|e| format!("order wire is not JSON: {e}"))?;
        order_record_of(&wire, intent).map_err(|e| e.to_string())
    }

    fn base(&self) -> &str {
        &self.transport.config().base_url
    }

    /// Dry-run over REST without bridging: the async read the submission
    /// protocol drives. Carries the session token, posts the order wire,
    /// and maps the verdict envelope onto the outcome.
    async fn dry_run_remote(
        &self,
        intent: &OrderIntent,
        _quote: &Quote,
    ) -> Result<DryRunOutcome, VenueError> {
        if intent.quantity == 0 {
            return Err(VenueError::Invalid("quantity must be positive".to_owned()));
        }
        let token = self.bearer_async().await?;
        let headers = [
            ("Authorization", token.as_str()),
            (ORDER_WIRE_VERSION_HEADER, ORDER_WIRE_VERSION),
        ];
        let url = format!("{}/dry-run", self.orders_path());
        let body = order_wire_of(intent)
            .and_then(|wire| {
                serde_json::to_string(&wire).map_err(|e| TransportError::Terminal(e.to_string()))
            })
            .map_err(into_venue)?;
        let raw = self
            .net
            .http_post_with_headers(&url, &body, &headers)
            .await
            .map_err(|e| VenueError::Transport(e.to_string()))?;
        dry_run_outcome_of(&raw)
    }

    fn orders_path(&self) -> String {
        format!("{}/accounts/{}/orders", self.base(), self.account)
    }

    async fn bearer_async(&self) -> Result<String, VenueError> {
        self.token
            .lease()
            .await
            .map(|lease| lease.credential().expose().to_owned())
            .map_err(auth_into_venue)
    }

    async fn get_record(&self, client_id: &str) -> Result<Option<OrderRecord>, VenueError> {
        let token = self.bearer_async().await?;
        let headers = [
            ("Authorization", token.as_str()),
            (ORDER_WIRE_VERSION_HEADER, ORDER_WIRE_VERSION),
        ];
        let url = format!("{}/{}", self.orders_path(), encode_segment(client_id));
        let raw = self
            .net
            .http_get_with_headers(&url, &headers)
            .await
            .map_err(|e| VenueError::Transport(e.to_string()))?;
        let wire: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| VenueError::Server(format!("response is not JSON: {e}")))?;
        // Absent is absent under any retry taxonomy: a `not_found` envelope
        // answers the lookup, so it short-circuits before error mapping.
        if is_not_found(&wire) {
            return Ok(None);
        }
        if let Some(failure) = crate::tastytrade::envelope_error(&wire) {
            return Err(into_venue(failure));
        }
        // Keep the journaled attribution when the local book holds it; a
        // post-crash query rebuilds the transported shape from the wire.
        let intent = common_core::sync::lock(&self.book)
            .get(client_id)
            .map(|record| record.intent.clone())
            .or_else(|| order_intent_of(&wire).ok());
        let Some(intent) = intent else {
            return Err(VenueError::Server(format!(
                "order {client_id} response carries no recognizable intent"
            )));
        };
        Self::order_record_with(&raw, intent)
            .map(Some)
            .map_err(VenueError::Server)
    }

    async fn post_order_body(
        &self,
        path: &str,
        body: &str,
        intent: &OrderIntent,
    ) -> Result<OrderRecord, VenueError> {
        let token = self.bearer_async().await?;
        let headers = [
            ("Authorization", token.as_str()),
            (ORDER_WIRE_VERSION_HEADER, ORDER_WIRE_VERSION),
        ];
        let raw = self
            .net
            .http_post_with_headers(path, body, &headers)
            .await
            .map_err(|e| VenueError::Transport(e.to_string()))?;
        let wire: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| VenueError::Server(format!("response is not JSON: {e}")))?;
        if let Some(failure) = crate::tastytrade::envelope_error(&wire) {
            return Err(into_venue(failure));
        }
        Self::order_record_with(&raw, intent.clone()).map_err(VenueError::Server)
    }

    fn remember(&self, record: &OrderRecord) {
        let mut book = common_core::sync::lock(&self.book);
        book.insert(record.intent.client_id.clone(), record.clone());
        *common_core::sync::lock(&self.positions) = positions_of(book.values().cloned());
    }
}

fn auth_into_venue(error: AuthError) -> VenueError {
    match error {
        AuthError::Terminal(reason) => VenueError::Refused(reason),
        AuthError::Transient(reason) => VenueError::Transport(reason),
        AuthError::Malformed(reason) => VenueError::Server(reason),
        AuthError::Missing | AuthError::Expired => {
            VenueError::Invalid("tastytrade venue is not authenticated".to_owned())
        }
    }
}

fn is_not_found(wire: &serde_json::Value) -> bool {
    wire.get("error")
        .and_then(|error| error.get("code"))
        .and_then(serde_json::Value::as_str)
        == Some("not_found")
}

fn into_venue(failure: TransportError) -> VenueError {
    match failure {
        TransportError::Rest(detail) => VenueError::Transport(detail),
        TransportError::Transient(detail) => VenueError::Server(detail),
        TransportError::Terminal(detail) => VenueError::Refused(detail),
    }
}

/// Map a dry-run verdict envelope onto the outcome. Pure: no I/O.
fn dry_run_outcome_of(raw: &str) -> Result<DryRunOutcome, VenueError> {
    let wire: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| VenueError::Server(format!("response is not JSON: {e}")))?;
    if let Some(failure) = crate::tastytrade::envelope_error(&wire) {
        return Err(into_venue(failure));
    }
    let allowed = wire
        .get("allowed")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| VenueError::Server("dry-run wire carries no verdict".to_owned()))?;
    let reserved = wire
        .get("reserved_notional")
        .ok_or_else(|| VenueError::Server("dry-run wire carries no reserved notional".to_owned()))
        .and_then(|value| {
            value
                .as_str()
                .unwrap_or("")
                .parse::<rust_decimal::Decimal>()
                .map(Money::new)
                .map_err(|_| VenueError::Server("reserved notional is not decimal".to_owned()))
        })?;
    Ok(DryRunOutcome {
        allowed,
        reserved_notional: reserved,
    })
}

/// Refold an order book into underlying positions: signed filled quantity
/// at the record's average price, skipping unfilled or priceless records
/// (no basis to invent). The one home for book→position projection —
/// mutations and stream events both fold through here, so no second
/// averaging can drift apart from it.
#[must_use]
/// Refold an order book into underlying positions: signed filled quantity
/// at the record's average price, skipping unfilled or priceless records
/// (no basis to invent). The one home for book→position projection —
/// mutations and stream events both fold through here, so no second
/// averaging can drift apart from it.
pub fn positions_of(records: impl IntoIterator<Item = OrderRecord>) -> HashMap<String, Position> {
    let mut positions: HashMap<String, Position> = HashMap::new();
    for record in records {
        if record.filled_quantity == 0 {
            continue;
        }
        let Some(price) = record.avg_price else {
            continue;
        };
        let sign = match record.intent.side {
            OrderSide::Buy => 1,
            OrderSide::Sell => -1,
        };
        let position = positions
            .entry(record.intent.underlying.clone())
            .or_insert_with(|| {
                Position::new()
                    .underlying(record.intent.underlying.clone())
                    .quantity(0)
                    .avg_price(Money::zero())
                    .build()
            });
        position.apply(sign * i64::from(record.filled_quantity), price);
    }
    positions.retain(|_, position| position.quantity != 0);
    positions
}

impl CredentialSource for TastytradeVenue {
    fn auth_handle(&self) -> AuthHandle {
        AuthHandle::new(Arc::clone(&self.token))
    }
}

impl Venue for TastytradeVenue {
    fn dry_run(&self, intent: &OrderIntent, quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        // Sync trait method, network truth: bridges via the canonical
        // `block_on` for sync callers and tests. The guard beside the
        // protocol pins this as the only bridge left: production async
        // work answers through `dry_run_async` below, never here.
        common_core::runtime::block_on(self.dry_run_remote(intent, quote))
    }

    fn dry_run_async<'a>(
        &'a self,
        intent: &'a OrderIntent,
        quote: &'a Quote,
    ) -> Pin<Box<dyn Future<Output = Result<DryRunOutcome, VenueError>> + Send + 'a>> {
        Box::pin(self.dry_run_remote(intent, quote))
    }

    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        _quote: &'a Quote,
        _now: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(known) = self.get_record(&intent.client_id).await? {
                return Ok(known);
            }
            let body = order_wire_of(&intent)
                .and_then(|wire| {
                    serde_json::to_string(&wire)
                        .map_err(|e| TransportError::Terminal(e.to_string()))
                })
                .map_err(into_venue)?;
            let record = self
                .post_order_body(&self.orders_path(), &body, &intent)
                .await?;
            self.remember(&record);
            Ok(record)
        })
    }

    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<OrderRecord>> + Send + 'a>> {
        Box::pin(async move {
            match self.get_record(client_id).await {
                Ok(found) => {
                    if let Some(record) = &found {
                        self.remember(record);
                    }
                    found
                }
                // Fail-closed conflation, stated: without a verdict the
                // caller must assume nothing — `submit_guarded` turns this
                // `None` into a timeout, never a blind resubmit.
                Err(_) => None,
            }
        })
    }

    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        _now: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            let known = self
                .get_record(client_id)
                .await?
                .ok_or_else(|| VenueError::Invalid(format!("unknown order {client_id}")))?;
            if !known.status.is_active() {
                return Err(VenueError::Invalid(format!(
                    "cancel refused: order {client_id} already terminal ({:?})",
                    known.status
                )));
            }
            let url = format!(
                "{}/{}/cancel",
                self.orders_path(),
                encode_segment(client_id)
            );
            let record = self.post_order_body(&url, "{}", &known.intent).await?;
            self.remember(&record);
            Ok(record)
        })
    }

    fn positions(&self) -> Vec<Position> {
        common_core::sync::lock(&self.positions)
            .values()
            .cloned()
            .collect()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        common_core::sync::lock(&self.book)
            .values()
            .cloned()
            .collect()
    }

    fn apply_corporate_event(
        &self,
        _action: &CorporateEventArgs,
    ) -> Result<CorporateOutcome, VenueError> {
        Err(VenueError::Invalid(
            "corporate events arrive over the account stream; manual application would invent venue truth".to_owned(),
        ))
    }

    fn fetch_balances(&self) -> Vec<BalanceEntry> {
        common_core::sync::lock(&self.balances).clone()
    }

    fn fetch_open_orders(&self) -> Vec<OrderRecord> {
        common_core::sync::lock(&self.book)
            .values()
            .filter(|record| record.status.is_active())
            .cloned()
            .collect()
    }

    fn fetch_trades(&self) -> Vec<Fill> {
        Vec::new()
    }
}

/// Production boot inputs. Built by the caller (the server binary reads
/// environment); this module never touches `std::env`, so tests and the
/// binary construct identical bundles from explicit values.
#[derive(Debug, Clone, bon::Builder)]
#[builder(start_fn = new)]
/// Production boot inputs. Built by the caller (the server binary reads
/// environment); this module never touches `std::env`, so tests and the
/// binary construct identical bundles from explicit values.
pub struct LiveBoot {
    /// Session tunables (risk caps, shutdown bound).
    pub session: SessionConfig,
    /// Transport endpoints and retry policy.
    pub transport: TastytradeConfig,
    /// Credential lifecycle policy.
    #[builder(default)]
    pub auth: AuthConfig,
    /// Venue account number (path segment of every order call).
    pub account: String,
    /// Durable journal file the submission path records through.
    pub journal_path: PathBuf,
    /// Optional durable credential store path.
    pub credential_store_path: Option<PathBuf>,
    /// Out-of-band alert file the session poller appends to: one JSON
    /// object per line for every breaker trip and gate halt, tailed
    /// outside the terminal. `None` runs no poller — the gates still gate,
    /// they only report in-band.
    /// (Optional fields default to `None`; no attribute needed.)
    pub alerts_path: Option<PathBuf>,
}

/// How often the session-owned alert poller collects. A cadence bound,
/// never a correctness threshold: trips and banks are edge-triggered, so a
/// slower poll delays the page without duplicating or losing one.
pub const ALERT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// One reconnect-generation factory for the supervised account stream: each
/// call opens a wire-message inbox, the driver pumps it through the
/// transport's message loop. Boxed so the boot takes exactly one shape
/// whether the caller dials a socket or returns a canned error.
pub type StreamConnect = Box<
    dyn FnMut() -> Pin<
            Box<dyn Future<Output = Result<mpsc::Receiver<String>, TransportError>> + Send>,
        > + Send,
>;

/// The supervised stream's driver inputs: the connect factory plus the
/// consumer it reports to. The breaker is attached from the stream config;
/// the driver only spawns when these are all present, so a breaker-only
/// boot (hermetic tests) and a driven boot share one attachment path.
pub struct StreamDriver {
    /// One reconnect generation per call.
    pub connect: StreamConnect,
    /// Consumer cancellation: fires to stop the driver.
    pub abort: StreamAbort,
    /// Account-event outbox the pump reports into (caller-bounded).
    pub out: Option<mpsc::Sender<AccountEvent>>,
}

/// One backfill sensor's inputs: the pre-drop series, the history ladder it
/// fetches through, and the indicator warmup headroom. Empty sources are
/// honest fail-closed (a gap suspends until a source can close it).
pub struct LiveBackfillSpec {
    /// The series the reconnect bars join.
    pub ctx: SeriesContext,
    /// History rungs, in order; empty stays empty.
    pub sources: Vec<Arc<dyn BackfillSource>>,
    /// Indicator warmup bars widening each fetch.
    pub warmup: usize,
}

impl LiveBackfillSpec {
    /// Bundle one sensor's inputs.
    #[must_use]
    pub fn new(ctx: SeriesContext, sources: Vec<Arc<dyn BackfillSource>>, warmup: usize) -> Self {
        Self {
            ctx,
            sources,
            warmup,
        }
    }
}

/// Which supervised producers the boot attaches. Absent is absent (the
/// loop later refuses); present-but-invalid refuses the boot itself, so a
/// misconfigured producer can never boot into a silently unsupervised loop.
pub struct LiveAttachments {
    /// Account-stream supervision (breaker, plus the driver when one is
    /// supplied to the boot).
    pub stream: Option<StreamConfig>,
    /// Journal-vs-venue reconciliation over the booted book.
    pub reconcile: bool,
    /// Reconnect backfill over the named series.
    pub backfill: Option<LiveBackfillSpec>,
    /// Market-data feed attached to the session.
    pub feed: Option<Arc<dyn crate::feed::MarketDataFeed>>,
}

impl LiveAttachments {
    /// Attach nothing: the loop refuses until producers arrive.
    #[must_use]
    pub fn none() -> Self {
        Self {
            stream: None,
            reconcile: false,
            backfill: None,
            feed: None,
        }
    }

    /// Attach an account-stream breaker built from `config`.
    #[must_use]
    pub fn with_stream(mut self, config: StreamConfig) -> Self {
        self.stream = Some(config);
        self
    }

    /// Attach journal-vs-venue reconciliation over the booted book.
    #[must_use]
    pub fn with_reconcile(mut self) -> Self {
        self.reconcile = true;
        self
    }

    /// Attach reconnect backfill over `spec`.
    #[must_use]
    pub fn with_backfill(mut self, spec: LiveBackfillSpec) -> Self {
        self.backfill = Some(spec);
        self
    }

    /// Attach a market-data feed.
    #[must_use]
    pub fn with_feed(mut self, feed: Arc<dyn crate::feed::MarketDataFeed>) -> Self {
        self.feed = Some(feed);
        self
    }
}

/// The supervised producers a boot carries, in pipeline order.
const PRODUCER_NAMES: [&str; 4] = ["stream", "reconcile", "backfill", "feed"];

/// A booted production session: the supervised session, its durable
/// journal, and the authenticated venue behind the shared registry.
pub struct LiveSession {
    /// The supervised session (feeds, bars, fan-out under one scope).
    pub session: TradingSession,
    /// Durable journal: the [`safe_submit`](crate::submit::safe_submit)
    /// recorder for every production submission. Shared with the
    /// reconciliation sensor, so the gate reads the same book the loop
    /// journals.
    pub journal: Arc<Journal>,
    /// Storage permit bound to the journal file: the live loop records its
    /// intents and transitions durably through it.
    pub storage: VenueStorage,
    /// The authenticated venue (also reachable via `session.registry()`).
    pub venue: Arc<TastytradeVenue>,
    /// The attached market-data feed, when one exists.
    pub feed: Option<Arc<dyn crate::feed::MarketDataFeed>>,
}

impl LiveSession {
    /// Attached producer names, in pipeline order.
    #[must_use]
    pub fn producers(&self) -> Vec<&'static str> {
        let mut attached = Vec::with_capacity(PRODUCER_NAMES.len());
        if self.session.stream_breaker().is_some() {
            attached.push("stream");
        }
        if self.session.reconcile_handle().is_some() {
            attached.push("reconcile");
        }
        if self.session.backfill_handle().is_some() {
            attached.push("backfill");
        }
        if self.feed.is_some() {
            attached.push("feed");
        }
        attached
    }

    /// Required producers with no attachment, in pipeline order.
    #[must_use]
    pub fn missing_producers(&self) -> Vec<String> {
        let attached = self.producers();
        PRODUCER_NAMES
            .into_iter()
            .filter(|name| !attached.contains(name))
            .map(str::to_owned)
            .collect()
    }

    /// Assemble the booted venue as a broker bundle: the same venue object
    /// the session registry routes, plus the account reader, the attached
    /// feed when one exists, and the venue's credential handle. Pure
    /// assembly — the `VenueRegistry` keeps routing execution exactly as
    /// before; the directory only names what the boot already owns.
    pub fn broker_bundle(&self) -> Result<BrokerBundle, BrokerError> {
        let venue = Arc::clone(&self.venue);
        let mut bundle = BrokerBundle::new(
            BrokerId::new("tastytrade")?,
            Arc::clone(&venue) as Arc<dyn Venue>,
        )
        .with_account(Arc::new(venue.account_reader()))
        .with_credentials(venue as Arc<dyn CredentialSource>);
        if let Some(feed) = &self.feed {
            bundle = bundle.with_feed(Arc::clone(feed));
        }
        Ok(bundle)
    }

    /// Register the booted bundle in `directory`. A second registration
    /// refuses (`Duplicate`) instead of replacing the execution route.
    pub fn register_broker_bundle(
        &self,
        directory: &BrokerRegistry,
    ) -> Result<BrokerBundle, BrokerError> {
        let bundle = self.broker_bundle()?;
        directory.register(bundle.clone())?;
        Ok(bundle)
    }
}

/// Journal-vs-venue reconciliation over the booted book: the journal's
/// filled intents plus leg fills against the registry's truth. Filled
/// intent quantities assume full fills (the journal records no partial
/// quantities); a partial fill mismatches fail-closed until the status
/// lands filled. Leg truth over the live registry is currently unobserved,
/// so a spread fill halts entries until leg reporting exists. Replay
/// failures mismatch the same way rather than reporting a clean book.
struct LiveReconcileSource {
    journal: Arc<Journal>,
    storage: VenueStorage,
    registry: Arc<VenueRegistry>,
}

impl ReconcileSource for LiveReconcileSource {
    fn journal(&self) -> JournalBook {
        let events = match self.journal.replay(&self.storage) {
            Ok(state) => state.events,
            Err(_) => {
                return JournalBook {
                    net: HashMap::from([("journal-unreadable".to_owned(), 1)]),
                    legs: HashMap::new(),
                };
            }
        };
        let legs = journal_leg_net(&events).unwrap_or_default();
        let mut last_status: HashMap<&str, &str> = HashMap::new();
        for event in &events {
            if matches!(
                event.kind.as_str(),
                "pending"
                    | "submitted"
                    | "acknowledged"
                    | "partial"
                    | "filled"
                    | "cancelled"
                    | "rejected"
                    | "expired"
                    | "open_failed"
            ) {
                last_status.insert(event.client_id.as_str(), event.kind.as_str());
            }
        }
        let mut net: HashMap<String, i64> = HashMap::new();
        for intent in events
            .iter()
            .filter(|event| event.kind == crate::journal::INTENT_KIND)
        {
            if last_status.get(intent.client_id.as_str()) != Some(&"filled") {
                continue;
            }
            let Ok(intent): Result<OrderIntent, _> = serde_json::from_str(&intent.detail) else {
                return JournalBook {
                    net: HashMap::from([("journal-unreadable".to_owned(), 1)]),
                    legs: HashMap::new(),
                };
            };
            let signed = match intent.side {
                OrderSide::Buy => i64::from(intent.quantity),
                OrderSide::Sell => -i64::from(intent.quantity),
            };
            *net.entry(intent.underlying).or_default() += signed;
        }
        JournalBook { net, legs }
    }

    fn venue_positions(&self) -> Vec<Position> {
        self.registry.positions()
    }

    fn venue_legs(&self) -> HashMap<String, i64> {
        HashMap::new()
    }
}

/// Boot a production session: validate everything, log in, open the
/// durable journal, boot the supervised session over a single-venue
/// registry, then attach the requested supervised producers. Producer
/// validation lands before the first dial; any failure refuses before the
/// first order can flow. Submission from here flows through
/// [`safe_submit`](crate::submit::safe_submit) + the durable
/// [`Journal`] — there is no second order path.
pub async fn boot_live(
    boot: &LiveBoot,
    net: Arc<NetCapability>,
    username: &str,
    password: &str,
    attachments: LiveAttachments,
    driver: Option<StreamDriver>,
) -> Result<LiveSession, VenueError> {
    if username.is_empty() || password.is_empty() {
        return Err(VenueError::Invalid(
            "tastytrade credentials are empty".to_owned(),
        ));
    }
    if let Some(config) = &attachments.stream {
        config
            .validate()
            .map_err(|reason| VenueError::Invalid(format!("stream config invalid: {reason}")))?;
    }
    if let Some(spec) = &attachments.backfill {
        if spec.ctx.symbol().is_empty() {
            return Err(VenueError::Invalid(
                "backfill series must name its underlying".to_owned(),
            ));
        }
    }
    if let Some(driver) = &driver {
        if attachments.stream.is_none() {
            return Err(VenueError::Invalid(
                "stream driver needs a stream breaker to report into".to_owned(),
            ));
        }
        if driver.out.is_none() {
            return Err(VenueError::Invalid(
                "stream driver needs an outbox to report into".to_owned(),
            ));
        }
    }
    let transport = Arc::new(TastytradeTransport::new(boot.transport.clone()));
    let store: Option<Arc<dyn CredentialStore>> = boot
        .credential_store_path
        .clone()
        .map(|path| Arc::new(FileCredentialStore::new(path)) as Arc<dyn CredentialStore>);
    let venue = Arc::new(TastytradeVenue::with_auth(
        transport,
        net,
        boot.account.clone(),
        boot.auth.clone(),
        store,
    )?);
    venue.login(username, password).await?;
    let storage = VenueStorage::new(boot.journal_path.clone());
    let journal = Arc::new(Journal::open(&boot.journal_path)?);
    let registry = Arc::new(VenueRegistry::single(Arc::clone(&venue) as Arc<dyn Venue>));
    let limits = boot.session.risk_limits.clone();
    let facts: Option<Arc<dyn PortfolioFactsSource>> = limits.has_aggregate_caps().then(|| {
        Arc::new(VenueBookFacts::new(registry.clone(), limits)) as Arc<dyn PortfolioFactsSource>
    });
    let mut session =
        TradingSession::boot(&boot.session, registry.clone(), storage.clone(), facts)?;
    session.attach_credential(venue.auth_handle());
    if let Some(config) = attachments.stream {
        let supervised = SupervisedStream::new(config);
        session.attach_stream_breaker(Arc::clone(supervised.breaker()));
        if let Some(mut driver) = driver {
            let out = driver.out.expect("validated present");
            let transport = Arc::clone(venue.transport());
            session.spawn_supervised_stream(
                &supervised,
                move || (driver.connect)(),
                driver.abort,
                out,
                transport,
            );
        }
    } else if driver.is_some() {
        return Err(VenueError::Invalid(
            "stream driver needs a stream breaker to report into".to_owned(),
        ));
    }
    if attachments.reconcile {
        let source = LiveReconcileSource {
            journal: Arc::clone(&journal),
            storage: storage.clone(),
            registry: registry.clone(),
        };
        session.attach_reconcile(Arc::new(source));
    }
    if let Some(spec) = attachments.backfill {
        session.attach_backfill(spec.ctx, spec.sources, spec.warmup);
    }
    let feed = attachments.feed;
    // The out-of-band page: a Scope-owned poller over the attached gates
    // into the operator's file. Reporting only — the poller never gates a
    // trade, and a dark file reports loudly without retrying.
    if let Some(path) = &boot.alerts_path {
        session.spawn_alert_poller(
            Arc::new(crate::alert::FileSink::new(path.clone())),
            ALERT_POLL_INTERVAL,
        );
    }
    Ok(LiveSession {
        session,
        journal,
        storage,
        venue,
        feed,
    })
}

#[cfg(test)]
#[path = "../tests/live_venue.rs"]
mod tests;