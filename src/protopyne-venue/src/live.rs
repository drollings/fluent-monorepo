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

use crate::error::VenueError;
use crate::journal::{Journal, VenueStorage};
use crate::order::{CorporateEvent, Fill, OrderIntent, OrderRecord, OrderSide, Position};
use crate::quote::Quote;
use crate::registry::VenueRegistry;
use crate::session::{SessionConfig, TradingSession};
use crate::tastytrade::{TastytradeConfig, TastytradeTransport, TransportError};
use crate::venue::{AccountEvent, BalanceEntry, DryRunOutcome, Venue};
use protopyne_types::Money;

/// Session-token venue over REST. Shares the transport's quote cache for
/// the [`QuoteSource`](crate::venue::QuoteSource) ladder; order truth lives
/// here, in the refolded mirror.
pub struct TastytradeVenue {
    transport: Arc<TastytradeTransport>,
    net: Arc<NetCapability>,
    account: String,
    token: Mutex<Option<String>>,
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
        if transport.config().base_url.is_empty() {
            return Err(VenueError::Invalid(
                "tastytrade base URL unconfigured: refusing to dial".to_owned(),
            ));
        }
        if account.is_empty() {
            return Err(VenueError::Invalid("tastytrade account is empty".to_owned()));
        }
        Ok(Self {
            transport,
            net,
            account,
            token: Mutex::new(None),
            book: Mutex::new(HashMap::new()),
            positions: Mutex::new(HashMap::new()),
            balances: Mutex::new(Vec::new()),
        })
    }

    /// Whether a session token is banked.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        common_core::sync::lock(&self.token).is_some()
    }

    /// Drop the banked token. Later order calls refuse until [`TastytradeVenue::login`].
    pub fn logout(&self) {
        *common_core::sync::lock(&self.token) = None;
    }

    /// Borrow the quote transport (ladder rung + refresh).
    #[must_use]
    pub fn transport(&self) -> &Arc<TastytradeTransport> {
        &self.transport
    }

    /// Authenticate: one POST, token banked on success. Empty credentials
    /// refuse without dialing; a venue refusal (bad login) is terminal and
    /// never retried; transient failures retry bounded by the transport
    /// config. At most one token is ever banked.
    pub async fn login(&self, username: &str, password: &str) -> Result<(), VenueError> {
        if username.is_empty() || password.is_empty() {
            return Err(VenueError::Invalid("tastytrade credentials are empty".to_owned()));
        }
        let url = format!("{}/sessions", self.base());
        let body = serde_json::json!({ "login": username, "password": password }).to_string();
        let attempts = self.transport.config().max_attempts.max(1);
        let token = common_core::retry::retry_async(
            attempts,
            self.transport.config().backoff_base_ms,
            crate::tastytrade::RETRY_JITTER_PCT,
            TransportError::is_retryable,
            || async {
                let raw = self
                    .net
                    .http_post(&url, &body)
                    .await
                    .map_err(|e| TransportError::Rest(e.to_string()))?;
                parse_token(&raw)
            },
        )
        .await
        .map_err(into_venue)?;
        *common_core::sync::lock(&self.token) = Some(token);
        Ok(())
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

    /// Parse one order envelope (the fake and the venue both speak
    /// `{"order": {...}}`; a bare record reads too). Never panics on wire.
    pub fn parse_order_record(body: &str) -> Result<OrderRecord, String> {
        let wire: serde_json::Value =
            serde_json::from_str(body).map_err(|e| format!("order wire is not JSON: {e}"))?;
        let node = wire.get("order").unwrap_or(&wire);
        serde_json::from_value(node.clone())
            .map_err(|e| format!("order payload is not an order record: {e}"))
    }

    fn base(&self) -> &str {
        &self.transport.config().base_url
    }

    fn orders_path(&self) -> String {
        format!("{}/accounts/{}/orders", self.base(), self.account)
    }

    fn bearer(&self) -> Result<String, VenueError> {
        common_core::sync::lock(&self.token).clone().ok_or_else(|| {
            VenueError::Invalid("tastytrade venue is not authenticated".to_owned())
        })
    }

    async fn get_record(&self, client_id: &str) -> Result<Option<OrderRecord>, VenueError> {
        let token = self.bearer()?;
        let headers = [("Authorization", token.as_str())];
        let url = format!("{}/{}", self.orders_path(), encode_segment(client_id));
        let raw = self
            .net
            .http_get_with_headers(&url, &headers)
            .await
            .map_err(|e| VenueError::Transport(e.to_string()))?;
        let wire: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| VenueError::Server(format!("response is not JSON: {e}")))?;
        // Absent is absent under any retry taxonomy: a `not_found` envelope
        // answers the lookup, so it short-circuits before error mapping.
        if is_not_found(&wire) {
            return Ok(None);
        }
        if let Some(failure) = crate::tastytrade::envelope_error(&wire) {
            return Err(into_venue(failure));
        }
        Self::parse_order_record(&raw)
            .map(Some)
            .map_err(VenueError::Server)
    }

    async fn post_record(&self, path: &str, body: &str) -> Result<OrderRecord, VenueError> {
        let token = self.bearer()?;
        let headers = [("Authorization", token.as_str())];
        let raw = self
            .net
            .http_post_with_headers(path, body, &headers)
            .await
            .map_err(|e| VenueError::Transport(e.to_string()))?;
        let wire: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| VenueError::Server(format!("response is not JSON: {e}")))?;
        if let Some(failure) = crate::tastytrade::envelope_error(&wire) {
            return Err(into_venue(failure));
        }
        Self::parse_order_record(&raw).map_err(VenueError::Server)
    }

    fn remember(&self, record: &OrderRecord) {
        let mut book = common_core::sync::lock(&self.book);
        book.insert(record.intent.client_id.clone(), record.clone());
        *common_core::sync::lock(&self.positions) = positions_of(book.values().cloned());
    }
}

fn parse_token(raw: &str) -> Result<String, TransportError> {
    let wire: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| TransportError::Transient(format!("response is not JSON: {e}")))?;
    if let Some(failure) = crate::tastytrade::envelope_error(&wire) {
        return Err(failure);
    }
    wire.get("token")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| TransportError::Transient("session wire carries no token".to_owned()))
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

/// Refold an order book into underlying positions: signed filled quantity
/// at the record's average price, skipping unfilled or priceless records
/// (no basis to invent). The one home for book→position projection —
/// mutations and stream events both fold through here, so no second
/// averaging can drift apart from it.
#[must_use]
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
        let position = positions.entry(record.intent.underlying.clone()).or_insert_with(|| {
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

impl Venue for TastytradeVenue {
    fn dry_run(&self, intent: &OrderIntent, _quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        // Sync trait method, network truth: bridges via the canonical
        // `block_on`. That bridge panics only for async callers pinned to a
        // current-thread runtime — production serves on multi-thread, and
        // the hermetic tests below run multi-thread for the same reason.
        if intent.quantity == 0 {
            return Err(VenueError::Invalid("quantity must be positive".to_owned()));
        }
        let token = self.bearer()?;
        let headers = [("Authorization", token.as_str())];
        let url = format!("{}/dry-run", self.orders_path());
        let body = serde_json::to_string(intent).map_err(|e| VenueError::Invalid(e.to_string()))?;
        let raw = common_core::runtime::block_on(
            self.net.http_post_with_headers(&url, &body, &headers),
        )
        .map_err(|e| VenueError::Transport(e.to_string()))?;
        let wire: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| VenueError::Server(format!("response is not JSON: {e}")))?;
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
        Ok(DryRunOutcome { allowed, reserved_notional: reserved })
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
            let body =
                serde_json::to_string(&intent).map_err(|e| VenueError::Invalid(e.to_string()))?;
            let record = self.post_record(&self.orders_path(), &body).await?;
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
            let known = self.get_record(client_id).await?.ok_or_else(|| {
                VenueError::Invalid(format!("unknown order {client_id}"))
            })?;
            if !known.status.is_active() {
                return Err(VenueError::Invalid(format!(
                    "cancel refused: order {client_id} already terminal ({:?})",
                    known.status
                )));
            }
            let url = format!("{}/{}/cancel", self.orders_path(), encode_segment(client_id));
            let record = self.post_record(&url, "{}").await?;
            self.remember(&record);
            Ok(record)
        })
    }

    fn positions(&self) -> Vec<Position> {
        common_core::sync::lock(&self.positions).values().cloned().collect()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        common_core::sync::lock(&self.book).values().cloned().collect()
    }

    fn apply_corporate_event(
        &self,
        _underlying: &str,
        _event: CorporateEvent,
        _now: DateTime<Utc>,
    ) -> Result<String, VenueError> {
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
pub struct LiveBoot {
    /// Session tunables (workers, queue, credit bound).
    pub session: SessionConfig,
    /// Transport endpoints and retry policy.
    pub transport: TastytradeConfig,
    /// Venue account number (path segment of every order call).
    pub account: String,
    /// Durable journal file the submission path records through.
    pub journal_path: PathBuf,
}

/// A booted production session: the supervised session, its durable
/// journal, and the authenticated venue behind the shared registry.
pub struct LiveSession {
    /// The supervised session (feeds, bars, fan-out under one scope).
    pub session: TradingSession,
    /// Durable journal: the [`safe_submit`](crate::submit::safe_submit)
    /// recorder for every production submission.
    pub journal: Journal,
    /// The authenticated venue (also reachable via `session.registry()`).
    pub venue: Arc<TastytradeVenue>,
}

/// Boot a production session: validate everything, log in, open the
/// durable journal, and boot the supervised session over a single-venue
/// registry. Any failure refuses before the first order can flow; nothing
/// dials until the config and credentials check out. Submission from here
/// flows through [`safe_submit`](crate::submit::safe_submit) + the durable
/// [`Journal`] — there is no second order path.
pub async fn boot_live(
    boot: &LiveBoot,
    net: Arc<NetCapability>,
    username: &str,
    password: &str,
) -> Result<LiveSession, VenueError> {
    if username.is_empty() || password.is_empty() {
        return Err(VenueError::Invalid("tastytrade credentials are empty".to_owned()));
    }
    let transport = Arc::new(TastytradeTransport::new(boot.transport.clone()));
    let venue = Arc::new(TastytradeVenue::new(transport, net, boot.account.clone())?);
    venue.login(username, password).await?;
    let storage = VenueStorage::new(boot.journal_path.clone());
    let journal = Journal::open(&boot.journal_path)?;
    let registry = Arc::new(VenueRegistry::single(Arc::clone(&venue) as Arc<dyn Venue>));
    let session = TradingSession::boot(&boot.session, registry, storage)?;
    Ok(LiveSession { session, journal, venue })
}

#[cfg(test)]
#[path = "../tests/live_venue.rs"]
mod tests;
