//! Market-data feeds and tick-to-bar aggregation.
//!
//! A [`MarketDataFeed`] answers a subscription with one owned [`FeedSession`]:
//! a bounded event receiver, an abort handle, and observation counters. The
//! session is the only asynchronous seam; the trade-print to closed-bar
//! adapter below is synchronous and reuses [`BarRing`] for bucket
//! finalization.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use protopyne_kernels::bars::{BarParts, BarRing, BarsConfig};
use protopyne_types::{Bar, Money, Timeframe};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::validate::Validate;

use crate::market::{BarUpdate, MarketEvent, MarketEventKind, TradePrint};
use crate::stream::{StreamConfig, StreamOutcome};

/// What one feed subscription asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// What one feed subscription asks for.
pub struct FeedSubscription {
    /// Symbol to subscribe.
    pub symbol: String,
    /// Event kind to subscribe.
    pub kind: MarketEventKind,
    /// Required for bar subscriptions; ignored for quote/print kinds.
    #[serde(default)]
    pub timeframe: Option<Timeframe>,
}

impl FeedSubscription {
    /// A quote/trade/book subscription.
    #[must_use]
    pub fn of(symbol: impl Into<String>, kind: MarketEventKind) -> Self {
        Self {
            symbol: symbol.into(),
            kind,
            timeframe: None,
        }
    }

    /// A bar subscription at `timeframe`.
    #[must_use]
    pub fn bars(symbol: impl Into<String>, timeframe: Timeframe) -> Self {
        Self {
            symbol: symbol.into(),
            kind: MarketEventKind::Candle,
            timeframe: Some(timeframe),
        }
    }
}

impl fluent_wvr::validate::Validate for FeedSubscription {
    type Err = FeedError;
    /// Fail-closed shape check.
    fn validate(&self) -> Result<(), Self::Err> {
        if self.symbol.trim().is_empty() {
            return Err(FeedError::Malformed(
                "feed subscription symbol must not be blank".to_owned(),
            ));
        }
        if self.kind == MarketEventKind::Candle && self.timeframe.is_none() {
            return Err(FeedError::Malformed(
                "bar subscription must name a timeframe".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Producer surfaces a feed implements.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
/// Producer surfaces a feed implements.
pub struct FeedCapabilities {
    /// Top-of-book quotes.
    pub quotes: bool,
    /// Trade prints.
    pub trades: bool,
    /// Bar updates.
    pub bars: bool,
    /// Native broker bars, rather than synthesized ones.
    pub native_bars: bool,
    /// Order-book depth.
    pub depth: bool,
}

/// Feed failure taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Feed failure taxonomy.
pub enum FeedError {
    /// The feed cannot serve this subscription.
    #[error("unsupported feed subscription: {0}")]
    Unsupported(String),
    /// A terminal feed failure.
    #[error("terminal feed failure: {0}")]
    Terminal(String),
    /// A retryable transport failure.
    #[error("transient feed failure: {0}")]
    Transient(String),
    /// A malformed inbound message.
    #[error("malformed feed message: {0}")]
    Malformed(String),
}

/// Boxed future for object-safe feed subscription.
pub type FeedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Object-safe market-data producer.
pub trait MarketDataFeed: Send + Sync {
    /// Surfaces this feed implements.
    fn capabilities(&self) -> FeedCapabilities;

    /// Open one session for `subscriptions`. Bounded; the feed never blocks
    /// a producer on a slow consumer.
    fn subscribe<'a>(
        &'a self,
        subscriptions: &'a [FeedSubscription],
    ) -> FeedFuture<'a, Result<FeedSession, FeedError>>;

    /// Run the supervised pump until abort or consumer loss. The default
    /// fits feeds with no transport of their own: wait for cancellation,
    /// then end. Feeds that own a stream override this with the reconnect
    /// loop; the owner (session scope, ingest service) drives it.
    fn run_supervised<'a>(
        &'a self,
        _config: &'a StreamConfig,
        abort: &'a StreamAbort,
    ) -> FeedFuture<'a, StreamOutcome> {
        Box::pin(async move {
            abort.cancelled().await;
            StreamOutcome::Aborted
        })
    }
}

/// Observation counters for one session. Producer confidence only: counters
/// never gate a trade by themselves.
#[derive(Debug, Default)]
/// Observation counters for one session. Producer confidence only: counters
/// never gate a trade by themselves.
pub struct FeedStats {
    received: AtomicU64,
    skipped: AtomicU64,
    malformed: AtomicU64,
    last_event_ms: AtomicI64,
}

impl FeedStats {
    /// Count one delivered event.
    pub fn record_received(&self) {
        self.received.fetch_add(1, Ordering::Relaxed);
        self.last_event_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Count one event skipped because the consumer was slow.
    pub fn record_skipped(&self) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
    }

    /// Count one malformed inbound message.
    pub fn record_malformed(&self) {
        self.malformed.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot the counters.
    #[must_use]
    pub fn snapshot(&self) -> FeedStatsSnapshot {
        FeedStatsSnapshot {
            received: self.received.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            malformed: self.malformed.load(Ordering::Relaxed),
            last_event_ms: match self.last_event_ms.load(Ordering::Relaxed) {
                0 => None,
                ms => Some(ms),
            },
        }
    }
}

/// One immutable counter snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
/// One immutable counter snapshot.
pub struct FeedStatsSnapshot {
    /// Events delivered.
    pub received: u64,
    /// Events skipped for a slow consumer.
    pub skipped: u64,
    /// Malformed messages dropped.
    pub malformed: u64,
    /// Last delivered event stamp in epoch milliseconds.
    pub last_event_ms: Option<i64>,
}

/// Producer-confidence health: is the feed keeping up, and how old is the
/// last fact?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Producer-confidence health: is the feed keeping up, and how old is the
/// last fact?
pub struct FeedHealth {
    /// Whether any fact has arrived.
    pub live: bool,
    /// Age of the last delivered event, in milliseconds.
    pub last_event_age_ms: Option<u64>,
    /// Whether the feed has observed skipped or malformed messages.
    pub degraded: bool,
    /// Why the reading is degraded, if it is.
    pub reason: Option<String>,
}

impl FeedHealth {
    /// Derive a reading from a snapshot and `now`.
    #[must_use]
    pub fn of(stats: FeedStatsSnapshot, now_ms_value: i64) -> Self {
        let age = stats
            .last_event_ms
            .map(|at| u64::try_from(now_ms_value.saturating_sub(at)).unwrap_or(0));
        let degraded = stats.skipped > 0 || stats.malformed > 0;
        Self {
            live: stats.last_event_ms.is_some(),
            last_event_age_ms: age,
            degraded,
            reason: degraded.then(|| {
                format!(
                    "feed degraded: {} skipped, {} malformed",
                    stats.skipped, stats.malformed
                )
            }),
        }
    }
}

/// One owned feed session.
pub struct FeedSession {
    /// Bounded inbound events.
    pub outbox: mpsc::Receiver<MarketEvent>,
    /// Session cancellation.
    pub abort: StreamAbort,
    /// Shared observation counters.
    pub stats: Arc<FeedStats>,
}

impl FeedSession {
    /// Build a session over caller-owned parts.
    #[must_use]
    pub fn new(
        outbox: mpsc::Receiver<MarketEvent>,
        abort: StreamAbort,
        stats: Arc<FeedStats>,
    ) -> Self {
        Self {
            outbox,
            abort,
            stats,
        }
    }

    /// Bounded channel capacity used by the shipped feeds.
    pub const DEFAULT_CAPACITY: usize = 256;

    /// Drain what is ready without waiting.
    pub fn try_drain(&mut self) -> Vec<MarketEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.outbox.try_recv() {
            self.stats.record_received();
            events.push(event);
        }
        events
    }

    /// Current health reading.
    #[must_use]
    pub fn health(&self) -> FeedHealth {
        FeedHealth::of(self.stats.snapshot(), now_ms())
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// One trade-print-to-bar adapter: exact close boundaries, duplicate drops,
/// and out-of-order refusal.
#[derive(Debug)]
/// One trade-print-to-bar adapter: exact close boundaries, duplicate drops,
/// and out-of-order refusal.
pub struct TradeBarAdapter {
    ring: BarRing,
    timeframe_ms: i64,
    bucket_start: Option<i64>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    cumulative_volume: f64,
    last_ts_ms: Option<i64>,
    duplicates: u64,
    out_of_order: u64,
}

/// What one print produced.
#[derive(Debug, Clone, PartialEq)]
/// What one print produced.
pub enum AdapterOutcome {
    /// A duplicate timestamp; counted, never folded.
    Duplicate,
    /// A print older than the last accepted print; counted, never folded.
    OutOfOrder,
    /// The forming bar moved.
    Forming(BarParts),
    /// A boundary closed the prior bar and started the next.
    Closed {
        /// Closed bar.
        bar: Bar,
        /// New forming bar.
        forming: BarParts,
    },
}

impl TradeBarAdapter {
    /// Build an adapter for `timeframe`; the ring uses the same span for
    /// every validation leg because only the bucket loop is read.
    pub fn new(timeframe: Timeframe) -> Result<Self, FeedError> {
        let timeframe_ms = timeframe.duration_ms();
        if timeframe_ms <= 0 {
            return Err(FeedError::Malformed(
                "adapter timeframe must be positive".to_owned(),
            ));
        }
        let config = BarsConfig::new(timeframe_ms, timeframe_ms, timeframe_ms, timeframe_ms);
        let ring = BarRing::new(&config)
            .map_err(|error| FeedError::Malformed(format!("bar ring refused: {error}")))?;
        Ok(Self {
            ring,
            timeframe_ms,
            bucket_start: None,
            open: 0.0,
            high: f64::NEG_INFINITY,
            low: f64::INFINITY,
            close: 0.0,
            cumulative_volume: 0.0,
            last_ts_ms: None,
            duplicates: 0,
            out_of_order: 0,
        })
    }

    /// Duplicate prints observed.
    #[must_use]
    pub fn duplicates(&self) -> u64 {
        self.duplicates
    }

    /// Out-of-order prints observed.
    #[must_use]
    pub fn out_of_order(&self) -> u64 {
        self.out_of_order
    }

    /// Current forming bar, if a bucket is open.
    #[must_use]
    pub fn forming(&self) -> Option<BarParts> {
        self.bucket_start.map(|ts_ms| BarParts {
            ts_ms,
            o: self.open,
            h: self.high,
            l: self.low,
            c: self.close,
            volume: self.cumulative_volume,
            buy_volume: 0.0,
        })
    }

    /// Ingest one print. `size` is the print's own volume.
    pub fn ingest(&mut self, print: TradePrint) -> Result<AdapterOutcome, FeedError> {
        let ts_ms = print.ts.timestamp_millis();
        if let Some(last) = self.last_ts_ms {
            if ts_ms == last {
                self.duplicates += 1;
                return Ok(AdapterOutcome::Duplicate);
            }
            if ts_ms < last {
                self.out_of_order += 1;
                return Ok(AdapterOutcome::OutOfOrder);
            }
        }
        let price = decimal_to_f64(print.price);
        if !price.is_finite() || price <= 0.0 {
            return Err(FeedError::Malformed(format!(
                "trade print price {price} is not positive"
            )));
        }
        let bucket = ts_ms.div_euclid(self.timeframe_ms) * self.timeframe_ms;
        let closed = match self.bucket_start {
            Some(current) if bucket > current => {
                let _ = self.ring.confirm_ltf(ts_ms, price, None);
                let bar = self.forming().map(Self::to_bar);
                self.begin_bucket(bucket, price);
                bar
            }
            Some(current) if bucket < current => {
                self.out_of_order += 1;
                return Ok(AdapterOutcome::OutOfOrder);
            }
            None => {
                self.begin_bucket(bucket, price);
                None
            }
            _ => None,
        };
        self.cumulative_volume += print.size as f64;
        self.high = self.high.max(price);
        self.low = self.low.min(price);
        self.close = price;
        self.last_ts_ms = Some(ts_ms);
        self.ring.ingest_tick(
            protopyne_kernels::indicator::BarTick {
                open: self.open,
                high: self.high,
                low: self.low,
                close: price,
                volume: self.cumulative_volume,
            },
            ts_ms,
        );
        let forming = self
            .forming()
            .ok_or_else(|| FeedError::Malformed("adapter lost its forming bucket".to_owned()))?;
        match closed {
            Some(bar) => Ok(AdapterOutcome::Closed { bar, forming }),
            None => Ok(AdapterOutcome::Forming(forming)),
        }
    }

    fn begin_bucket(&mut self, start_ms: i64, price: f64) {
        self.bucket_start = Some(start_ms);
        self.open = price;
        self.high = price;
        self.low = price;
        self.close = price;
        self.cumulative_volume = 0.0;
        self.ring.on_bar_open(price, price, price, price, start_ms);
    }

    fn to_bar(parts: BarParts) -> Bar {
        Bar::new()
            .ts(epoch_ms(parts.ts_ms))
            .o(decimal_of(parts.o))
            .h(decimal_of(parts.h))
            .l(decimal_of(parts.l))
            .c(decimal_of(parts.c))
            .volume(parts.volume.max(0.0).round() as u64)
            .build()
    }
}

/// Convert a forming/closing parts row into the chart event.
#[must_use]
/// Convert a forming/closing parts row into the chart event.
pub fn bar_update(symbol: &str, bar: BarParts, closed: bool) -> BarUpdate {
    BarUpdate {
        underlying: symbol.to_owned(),
        bar: Bar::new()
            .ts(epoch_ms(bar.ts_ms))
            .o(decimal_of(bar.o))
            .h(decimal_of(bar.h))
            .l(decimal_of(bar.l))
            .c(decimal_of(bar.c))
            .volume(bar.volume.max(0.0).round() as u64)
            .build(),
        closed,
    }
}

fn epoch_ms(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(Utc::now)
}

fn decimal_of(value: f64) -> Decimal {
    if !value.is_finite() {
        return Decimal::ZERO;
    }
    Decimal::try_from(value).unwrap_or(Decimal::ZERO)
}

fn decimal_to_f64(value: Money) -> f64 {
    use rust_decimal::prelude::ToPrimitive as _;
    value.as_decimal().to_f64().unwrap_or(f64::NAN)
}

/// A session over a bounded channel plus its producer task, for adapters
/// that publish already-normalized events.
pub struct ChannelFeed {
    capabilities: FeedCapabilities,
    capacity: usize,
}

impl ChannelFeed {
    /// Build a feed with an explicit capacity.
    #[must_use]
    pub fn new(capabilities: FeedCapabilities, capacity: usize) -> Self {
        Self {
            capabilities,
            capacity: capacity.max(1),
        }
    }
}

impl MarketDataFeed for ChannelFeed {
    fn capabilities(&self) -> FeedCapabilities {
        self.capabilities
    }

    fn subscribe<'a>(
        &'a self,
        subscriptions: &'a [FeedSubscription],
    ) -> FeedFuture<'a, Result<FeedSession, FeedError>> {
        Box::pin(async move {
            for subscription in subscriptions {
                subscription.validate()?;
            }
            if subscriptions
                .iter()
                .any(|s| s.kind == MarketEventKind::Trade && !self.capabilities.trades)
            {
                return Err(FeedError::Unsupported(
                    "feed does not publish trade prints".to_owned(),
                ));
            }
            let (_tx, rx) = mpsc::channel(self.capacity);
            Ok(FeedSession::new(
                rx,
                StreamAbort::new(),
                Arc::new(FeedStats::default()),
            ))
        })
    }
}

/// Resolve a session timeout for consumers that poll rather than await.
#[must_use]
/// Resolve a session timeout for consumers that poll rather than await.
pub fn poll_interval() -> Duration {
    Duration::from_millis(10)
}

#[cfg(test)]
#[path = "../tests/feed.rs"]
mod tests;