//! Tastytrade transport skeleton: market/account data in, zero order
//! authority out.
//!
//! Two axes, tested separately:
//!
//! * Producer-confidence (validity): malformed wire messages refuse with
//!   reasons (never panics); unknown error codes and unparseable bodies
//!   retry rather than invent success; an unconfigured base URL refuses
//!   without dialing anything.
//! * Task-value (economics): the quote ladder serves primary → fallback → QE
//!   `None` fail-closed, so one dark feed never becomes a blind trade.
//!
//! The loopback speaks `200`-with-envelope because [`NetCapability`]
//! surfaces response bodies only — no status line reaches this module. The
//! envelope `error.code` is the status: `unauthorized`/`forbidden`/
//! `bad_request` stop immediately (terminal), `unavailable`/
//! `rate_limited`/`timeout` retry with backoff. A later milestone that
//! teaches the capability to surface HTTP status graduates this mapping;
//! until then the envelope is the contract, stated here and in the tests.
//!
//! No order authority lives in this module by construction: there is no
//! `Venue::submit` impl here, and submission keeps flowing through
//! `safe_submit` + `RiskEngine` + grant evaluation.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use chrono::{DateTime, Utc};
use fluent_concurrency::io::net::NetCapability;
use fluent_concurrency::scope::Scope;
use fluent_wvr::{Describable, FieldAccess};
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use protopyne_types::{ChainQuote, Greeks, Money, OptionContract, OptionKind};

use crate::order::Fill;
use crate::quote::Quote;
use crate::venue::{AccountEvent, BalanceEntry, QuoteSource};

/// Retry schedule frozen by the transport calibration: attempts total per
/// refresh (first try plus retries). The freeze test pins the value; any
/// move re-earns the terminal-zero/transient-recall tallies first.
pub const RETRY_MAX_ATTEMPTS: u32 = 3;
/// Backoff base in milliseconds between retries (frozen).
pub const RETRY_BACKOFF_BASE_MS: u64 = 100;
/// Backoff jitter percent (frozen).
pub const RETRY_JITTER_PCT: u32 = 50;

/// Transport configuration: endpoints plus policy knobs. Loaded via the
/// shared JSON loader (missing or broken files read as default, loudly),
/// never a second loader. No credential field: [`NetCapability`] sends no
/// auth headers, so this skeleton runs unauthenticated loopback only.
#[derive(
    Debug,
    Clone,
    bon::Builder,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    FieldAccess,
    Describable,
)]
#[builder(start_fn = new)]
pub struct TastytradeConfig {
    /// REST base URL (empty means unconfigured: refresh refuses without
    /// dialing).
    #[field(desc = "REST base URL (empty unconfigures the transport)")]
    #[builder(default)]
    #[serde(default)]
    pub base_url: String,
    /// Maximum cached-quote age in seconds before the ladder rung goes dark.
    #[field(desc = "Maximum cached-quote age in seconds", min = 1, max = 3600)]
    #[builder(default = 60)]
    #[serde(default = "default_quote_max_age_secs")]
    pub quote_max_age_secs: u64,
    /// REST attempts per refresh (first try plus retries).
    #[field(desc = "REST attempts per refresh", min = 1, max = 32)]
    #[builder(default = RETRY_MAX_ATTEMPTS)]
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Backoff base in milliseconds between retries.
    #[field(desc = "Retry backoff base in milliseconds", min = 1, max = 60_000)]
    #[builder(default = RETRY_BACKOFF_BASE_MS)]
    #[serde(default = "default_backoff_base_ms")]
    pub backoff_base_ms: u64,
}

fn default_quote_max_age_secs() -> u64 {
    60
}

fn default_max_attempts() -> u32 {
    RETRY_MAX_ATTEMPTS
}

fn default_backoff_base_ms() -> u64 {
    RETRY_BACKOFF_BASE_MS
}

impl Default for TastytradeConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl TastytradeConfig {
    /// Load from `path`, or default when missing or broken (the shared
    /// loader warns loudly on broken files).
    #[must_use]
    pub fn load(path: &Path) -> Self {
        common_core::config::load_json_or_default(path)
    }
}

/// Transport failures with an explicit retry taxonomy: transient failures
/// retry with backoff, terminal failures stop immediately with their reason
/// intact (first-class `Err`, never swallowed).
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// HTTP transport or body framing failed.
    #[error("transport failure: {0}")]
    Rest(String),
    /// The venue answered retryably (unavailable, rate-limited, timed out,
    /// or an unrecognized error code).
    #[error("transient venue error: {0}")]
    Transient(String),
    /// The venue refused outright (unauthorized, forbidden, bad request).
    /// Never retried.
    #[error("terminal venue error: {0}")]
    Terminal(String),
}

impl TransportError {
    /// Whether `retry_async` may try again.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::Terminal(_))
    }
}

/// Subscription validation: symbols route by exact match downstream, so the
/// transport refuses anything that cannot be a symbol here with a reason.
pub fn validate_subscription(symbol: &str) -> Result<(), String> {
    if symbol.is_empty() {
        return Err("subscription symbol is empty".to_owned());
    }
    if symbol.len() > 24 {
        return Err(format!(
            "subscription symbol {symbol} exceeds 24 characters"
        ));
    }
    if !symbol
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err(format!(
            "subscription symbol {symbol} is not a plain symbol"
        ));
    }
    Ok(())
}

fn as_decimal(value: &serde_json::Value) -> Result<Decimal, String> {
    match value {
        serde_json::Value::String(text) => text
            .parse::<Decimal>()
            .map_err(|_| format!("{text} is not a decimal number")),
        serde_json::Value::Number(number) => number
            .to_string()
            .parse::<Decimal>()
            .map_err(|_| format!("{number} is not a decimal number")),
        other => Err(format!("{other} is not a decimal number")),
    }
}

fn field<'a>(body: &'a serde_json::Value, name: &str) -> Result<&'a serde_json::Value, String> {
    body.get(name)
        .ok_or_else(|| format!("wire message is missing {name}"))
}

fn as_u64(value: &serde_json::Value, name: &str) -> Result<u64, String> {
    value
        .as_u64()
        .ok_or_else(|| format!("{name} is not a non-negative integer"))
}

fn as_str<'a>(value: &'a serde_json::Value, name: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("{name} is not a string"))
}

fn parse_time(text: &str) -> Result<DateTime<Utc>, String> {
    text.parse::<DateTime<Utc>>()
        .map_err(|_| format!("ts {text} is not RFC 3339"))
}

/// Venue JSON → [`Quote`]. Refuses missing fields, negative bids, and
/// crossed markets with reasons; never panics on wire input.
pub fn transform_quote(body: &str) -> Result<Quote, String> {
    let wire: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("quote wire is not JSON: {e}"))?;
    let bid = as_decimal(field(&wire, "bid")?)?;
    if bid.is_sign_negative() {
        return Err(format!("quote bid {bid} is negative"));
    }
    let ask = as_decimal(field(&wire, "ask")?)?;
    if ask <= bid {
        return Err(format!("quote crossed or invalid: bid {bid} ask {ask}"));
    }
    let bid_size = as_u64(field(&wire, "bid_size")?, "bid_size")?;
    let ask_size = as_u64(field(&wire, "ask_size")?, "ask_size")?;
    let ts = parse_time(as_str(field(&wire, "ts")?, "ts")?)?;
    Ok(Quote::new()
        .bid(Money::new(bid))
        .ask(Money::new(ask))
        .bid_size(bid_size)
        .ask_size(ask_size)
        .ts(ts)
        .build())
}

fn parse_kind(text: &str) -> Result<OptionKind, String> {
    match text.to_ascii_lowercase().as_str() {
        "call" => Ok(OptionKind::Call),
        "put" => Ok(OptionKind::Put),
        other => Err(format!("chain kind {other} names neither call nor put")),
    }
}

/// Venue JSON → [`ChainQuote`]. Greeks and IV ride along when present and
/// stay absent when not; every structural refusal names its field.
pub fn transform_chain_quote(body: &str) -> Result<ChainQuote, String> {
    let wire: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("chain wire is not JSON: {e}"))?;
    let underlying = as_str(field(&wire, "underlying")?, "underlying")?.to_owned();
    let expiry = as_str(field(&wire, "expiry")?, "expiry")?;
    let expiry = chrono::NaiveDate::parse_from_str(expiry, "%Y-%m-%d")
        .map_err(|_| format!("chain expiry {expiry} is not YYYY-MM-DD"))?;
    let strike = as_decimal(field(&wire, "strike")?)?;
    let kind = parse_kind(as_str(field(&wire, "kind")?, "kind")?)?;
    let contract = OptionContract::new()
        .underlying(underlying)
        .expiry(expiry)
        .strike(Money::new(strike))
        .kind(kind)
        .build();
    let bid = as_decimal(field(&wire, "bid")?)?;
    let ask = as_decimal(field(&wire, "ask")?)?;
    if bid.is_sign_negative() {
        return Err(format!("chain bid {bid} is negative"));
    }
    if ask <= bid {
        return Err(format!("chain crossed or invalid: bid {bid} ask {ask}"));
    }
    let bid_size = u32::try_from(as_u64(field(&wire, "bid_size")?, "bid_size")?)
        .map_err(|_| "chain bid_size exceeds u32".to_owned())?;
    let ask_size = u32::try_from(as_u64(field(&wire, "ask_size")?, "ask_size")?)
        .map_err(|_| "chain ask_size exceeds u32".to_owned())?;
    let iv = wire
        .get("iv")
        .map(|value| {
            value
                .as_f64()
                .ok_or_else(|| format!("chain iv {value} is not a number"))
        })
        .transpose()?;
    let greeks = wire
        .get("delta")
        .map(|delta| -> Result<Greeks, String> {
            let delta = delta
                .as_f64()
                .ok_or_else(|| format!("chain delta {delta} is not a number"))?;
            Ok(Greeks::new()
                .delta(delta)
                .gamma(optional_f64(&wire, "gamma")?)
                .theta(optional_f64(&wire, "theta")?)
                .vega(optional_f64(&wire, "vega")?)
                .build())
        })
        .transpose()?;
    let timestamp = wire
        .get("ts")
        .map(|value| parse_time(as_str(value, "ts")?))
        .transpose()?
        .unwrap_or_else(Utc::now);
    Ok(ChainQuote::new()
        .contract(contract)
        .bid(Money::new(bid))
        .ask(Money::new(ask))
        .bid_size(bid_size)
        .ask_size(ask_size)
        .maybe_iv(iv)
        .maybe_greeks(greeks)
        .timestamp(timestamp)
        .build())
}

fn optional_f64(wire: &serde_json::Value, name: &str) -> Result<f64, String> {
    wire.get(name)
        .map(|value| {
            value
                .as_f64()
                .ok_or_else(|| format!("chain {name} {value} is not a number"))
        })
        .transpose()
        .map(|optional| optional.unwrap_or(0.0))
}

/// Venue JSON → [`AccountEvent`], routed on the envelope `kind`. Unknown
/// kinds and short payloads refuse with reasons.
pub fn transform_account_event(body: &str) -> Result<AccountEvent, String> {
    let wire: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("account wire is not JSON: {e}"))?;
    let kind = as_str(field(&wire, "kind")?, "kind")?;
    match kind {
        "balance" => {
            let asset = as_str(field(&wire, "asset")?, "asset")?.to_owned();
            let amount = as_decimal(field(&wire, "amount")?)?;
            Ok(AccountEvent::BalanceUpdated(BalanceEntry {
                asset,
                amount: Money::new(amount),
            }))
        }
        "order" => {
            let order = field(&wire, "order")?;
            serde_json::from_value(order.clone())
                .map(AccountEvent::OrderUpdated)
                .map_err(|e| format!("order payload is not an order record: {e}"))
        }
        "trade" => {
            let quantity = u32::try_from(as_u64(field(&wire, "quantity")?, "quantity")?)
                .map_err(|_| "trade quantity exceeds u32".to_owned())?;
            let price = as_decimal(field(&wire, "price")?)?;
            let fee = as_decimal(field(&wire, "fee")?)?;
            Ok(AccountEvent::Trade(
                Fill::new()
                    .quantity(quantity)
                    .price(Money::new(price))
                    .fee(Money::new(fee))
                    .build(),
            ))
        }
        other => Err(format!(
            "account kind {other} is not balance, order, or trade"
        )),
    }
}

/// Envelope error codes the loopback speaks. Terminal codes stop the retry
/// walk immediately with their reason; everything else is transient — named
/// (`unavailable`/5xx, `rate_limited`/429, `timeout`, `reset`) or unknown —
/// and retries bounded by `max_attempts`, which still fails closed on
/// exhaustion. The boundary table itself lives in
/// [`crate::stream::terminal_reason`] (one closed set, one stated reason
/// per code); this function only attaches the venue's message. The
/// calibration freezes the table: terminal codes must show zero retries.
pub(crate) fn envelope_error(body: &serde_json::Value) -> Option<TransportError> {
    let error = body.get("error")?;
    let code = error.get("code")?.as_str()?;
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("venue refused without detail");
    let failure = match crate::stream::terminal_reason(code) {
        Some(_) => TransportError::Terminal(format!("{code}: {message}")),
        None => TransportError::Transient(format!("{code}: {message}")),
    };
    Some(failure)
}

/// How one message-pump generation ended. The pump reports; the caller
/// (scope task or supervised driver) decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PumpEnd {
    /// The feed republic ended (inbox closed): reconnect to resume.
    InboxClosed,
    /// The consumer is gone (outbox closed): stop rather than wedge.
    OutboxClosed,
    /// The consumer's abort fired: stop without counting an error.
    Aborted,
}

const REJECTION_LOG_CAP: usize = 16;

/// The skeleton transport: REST refresh through [`NetCapability`] into a
/// timestamped quote cache, and a scope-owned stream loop forwarding
/// account events. Reads serve the [`QuoteSource`] ladder; nothing here
/// submits orders.
pub struct TastytradeTransport {
    config: TastytradeConfig,
    cache: Mutex<HashMap<String, Quote>>,
    dropped: AtomicUsize,
    skipped: AtomicUsize,
    rejections: Mutex<VecDeque<String>>,
    live: AtomicUsize,
}

impl TastytradeTransport {
    /// Open a transport over `config`. No task spawns here; the stream loop
    /// spawns under the caller's [`Scope`] on demand.
    #[must_use]
    pub fn new(config: TastytradeConfig) -> Self {
        Self {
            config,
            cache: Mutex::new(HashMap::new()),
            dropped: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            rejections: Mutex::new(VecDeque::new()),
            live: AtomicUsize::new(0),
        }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &TastytradeConfig {
        &self.config
    }

    /// Seed or overwrite one cached quote (tests and the stream loop; the
    /// ladder only ever reads fresh entries).
    pub fn put_cached(&self, underlying: &str, quote: Quote) {
        common_core::sync::lock(&self.cache).insert(underlying.to_owned(), quote);
    }

    /// Stream loops currently owned by a scope (task accounting for the
    /// drain assertion: `Scope::close` must bring this back to zero).
    #[must_use]
    pub fn active_streams(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Account one supervised run (or unsupervised loop) as live. Paired
    /// with [`TastytradeTransport::note_stream_ended`]; the supervised
    /// driver holds the count for its whole run across reconnects.
    pub fn note_stream_started(&self) {
        self.live.fetch_add(1, Ordering::SeqCst);
    }

    /// Release one live count previously taken with
    /// [`TastytradeTransport::note_stream_started`].
    pub fn note_stream_ended(&self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }

    /// Wire messages dropped as malformed since boot.
    #[must_use]
    pub fn dropped_messages(&self) -> usize {
        self.dropped.load(Ordering::SeqCst)
    }

    /// Valid events skipped for a slow subscriber since boot. Skips are
    /// lag, not loss by policy: the loop never wedges on a full outbox.
    #[must_use]
    pub fn skipped_messages(&self) -> usize {
        self.skipped.load(Ordering::SeqCst)
    }

    /// Banked malformed-message reasons, oldest first, capped so a hostile
    /// feed cannot grow memory without bound.
    #[must_use]
    pub fn rejections(&self) -> Vec<String> {
        common_core::sync::lock(&self.rejections)
            .iter()
            .cloned()
            .collect()
    }

    fn bank_rejection(&self, reason: String) {
        let mut log = common_core::sync::lock(&self.rejections);
        if log.len() >= REJECTION_LOG_CAP {
            log.pop_front();
        }
        log.push_back(reason);
    }

    /// Refresh one quote over REST: GET, envelope check, transform, cache.
    /// Transient failures retry with backoff through the shared helper;
    /// terminal failures return immediately.
    pub async fn refresh_quote(
        &self,
        net: &NetCapability,
        underlying: &str,
    ) -> Result<Quote, TransportError> {
        if self.config.base_url.is_empty() {
            return Err(TransportError::Terminal(
                "tastytrade base URL unconfigured: refusing to dial".to_owned(),
            ));
        }
        let url = format!("{}/quotes/{underlying}", self.config.base_url);
        let attempts = self.config.max_attempts.max(1);
        let quote = common_core::retry::retry_async(
            attempts,
            self.config.backoff_base_ms,
            RETRY_JITTER_PCT,
            TransportError::is_retryable,
            || async {
                let body = net
                    .http_get(&url)
                    .await
                    .map_err(|e| TransportError::Rest(e.to_string()))?;
                let wire: serde_json::Value = serde_json::from_str(&body)
                    .map_err(|e| TransportError::Transient(format!("response is not JSON: {e}")))?;
                if let Some(failure) = envelope_error(&wire) {
                    return Err(failure);
                }
                transform_quote(&body).map_err(TransportError::Transient)
            },
        )
        .await?;
        self.put_cached(underlying, quote);
        Ok(quote)
    }

    /// Fetch one chain snapshot over REST: GET the snapshot envelope,
    /// translate every entry through [`transform_chain_quote`], and return
    /// the quotes. The wire speaks `{"quotes":[<chain-wire>, ...]}` with
    /// each entry in the documented chain-wire shape; errors ride the same
    /// `error.code` envelope as the quote path, so the terminal/transient
    /// taxonomy is shared, never reimplemented. Any entry that will not
    /// transform — or an empty snapshot — refuses the whole fetch: a
    /// partial window never becomes half a file downstream. The bearer
    /// token travels one call (never stored, never logged); an empty token
    /// or an unconfigured base refuses before anything dials.
    pub async fn fetch_chain_snapshot(
        &self,
        net: &NetCapability,
        underlying: &str,
        token: &str,
    ) -> Result<Vec<ChainQuote>, TransportError> {
        if self.config.base_url.is_empty() {
            return Err(TransportError::Terminal(
                "tastytrade base URL unconfigured: refusing to dial".to_owned(),
            ));
        }
        validate_subscription(underlying).map_err(TransportError::Terminal)?;
        if token.is_empty() {
            return Err(TransportError::Terminal(
                "tastytrade credentials absent: refusing to dial".to_owned(),
            ));
        }
        let url = format!("{}/chains/{underlying}", self.config.base_url);
        let attempts = self.config.max_attempts.max(1);
        let bearer = format!("Bearer {token}");
        common_core::retry::retry_async(
            attempts,
            self.config.backoff_base_ms,
            RETRY_JITTER_PCT,
            TransportError::is_retryable,
            || async {
                let body = net
                    .http_get_with_headers(&url, &[("Authorization", bearer.as_str())])
                    .await
                    .map_err(|e| TransportError::Rest(e.to_string()))?;
                let wire: serde_json::Value = serde_json::from_str(&body)
                    .map_err(|e| TransportError::Transient(format!("response is not JSON: {e}")))?;
                if let Some(failure) = envelope_error(&wire) {
                    return Err(failure);
                }
                let entries = wire
                    .get("quotes")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| {
                        TransportError::Transient("chain snapshot names no quotes array".to_owned())
                    })?;
                if entries.is_empty() {
                    return Err(TransportError::Transient(
                        "chain snapshot holds no quotes".to_owned(),
                    ));
                }
                entries
                    .iter()
                    .map(|entry| transform_chain_quote(&entry.to_string()))
                    .map(|parsed| {
                        parsed.map_err(|reason| {
                            TransportError::Transient(format!("chain entry refused: {reason}"))
                        })
                    })
                    .collect()
            },
        )
        .await
    }

    /// Run the stream loop under `scope`: every inbox message transforms to
    /// an account event or drops with a banked reason. Delivery to a slow
    /// subscriber skips lag (`try_send`, counted) instead of wedging the
    /// loop; a closed receiver or a dropped outbox ends it, and
    /// `Scope::close` aborts it either way. Both channels stay
    /// caller-bounded — this loop constructs none, so there is no hidden
    /// buffer to audit; a full outbox skips by policy, never blocks. The
    /// loop holds no order authority — it forwards data. Message handling is
    /// byte-identical to the supervised driver's generations: both run
    /// [`TastytradeTransport::pump_account_events`].
    pub fn run_stream(
        self: &Arc<Self>,
        scope: &mut Scope,
        inbox: mpsc::Receiver<String>,
        out: mpsc::Sender<AccountEvent>,
    ) {
        let worker = Arc::clone(self);
        worker.live.fetch_add(1, Ordering::SeqCst);
        scope.spawn(async move {
            let _guard = StreamGuard { live: &worker.live };
            let abort = fluent_concurrency::stream::StreamAbort::new();
            let _ = worker.pump_account_events(inbox, out, &abort).await;
        });
    }

    /// Pump one message generation: transform each inbox message to an
    /// account event or drop it with a banked reason, skipping lag on a
    /// slow subscriber and stopping on a closed outbox or the consumer's
    /// abort. Shared by the unsupervised stream loop and the supervised
    /// driver, so parsing behavior is identical under both — only
    /// supervision changes.
    pub(crate) async fn pump_account_events(
        &self,
        mut inbox: mpsc::Receiver<String>,
        out: mpsc::Sender<AccountEvent>,
        abort: &fluent_concurrency::stream::StreamAbort,
    ) -> PumpEnd {
        loop {
            let message = tokio::select! {
                received = inbox.recv() => received,
                () = abort.cancelled() => return PumpEnd::Aborted,
            };
            let Some(message) = message else {
                return PumpEnd::InboxClosed;
            };
            let event = match transform_account_event(&message) {
                Ok(event) => event,
                Err(reason) => {
                    self.dropped.fetch_add(1, Ordering::SeqCst);
                    self.bank_rejection(reason);
                    continue;
                }
            };
            match out.try_send(event) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.skipped.fetch_add(1, Ordering::SeqCst);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return PumpEnd::OutboxClosed,
            }
        }
    }
}

struct StreamGuard<'a> {
    live: &'a AtomicUsize,
}

impl Drop for StreamGuard<'_> {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

impl QuoteSource for TastytradeTransport {
    fn latest(&self, underlying: &str) -> Option<Quote> {
        let quote = common_core::sync::lock(&self.cache)
            .get(underlying)
            .copied()?;
        if quote.age_secs(Utc::now()) > self.config.quote_max_age_secs {
            return None;
        }
        Some(quote)
    }

    fn name(&self) -> &str {
        "tastytrade"
    }
}

#[cfg(test)]
#[path = "../tests/tastytrade.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/transport_calibration.rs"]
mod transport_calibration_tests;
