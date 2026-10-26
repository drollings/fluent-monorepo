//! Tastytrade market-data feed: supervised chunked streaming plus an adapter
//! for externally supplied prints.
//!
//! The transport opens one long-lived `POST {base}/market-data/stream` per
//! reconnect generation and consumes newline-delimited JSON messages through
//! [`NetCapability::http_post_json_stream`]. Quotes update the shared quote
//! cache through the existing `put_cached`; prints fan out to print
//! subscribers and into per-symbol timeframe adapters that synthesize
//! forming/closed bars. Every generation runs under the shared
//! [`ReconnectPolicy`](crate::stream::ReconnectPolicy): a dropped stream
//! reconnects with backoff, a terminal refusal stops without retrying, and an
//! abort preempts both the reconnect sleep and the in-flight pump. No
//! background task hides inside `subscribe`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chrono::{TimeZone, Utc};
use fluent_concurrency::io::net::NetCapability;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::validate::Validate;
use futures_util::stream::{Stream, StreamExt};
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use crate::feed::{
    bar_update, AdapterOutcome, FeedCapabilities, FeedError, FeedFuture, FeedSession, FeedStats,
    FeedSubscription, MarketDataFeed, TradeBarAdapter,
};
use crate::market::{Aggressor, MarketEvent, MarketEventKind, TradePrint};
use crate::stream::{StreamConfig, StreamOutcome, SupervisedSource};
use crate::tastytrade::{TastytradeTransport, TransportError};
use protopyne_types::Money;

/// One connected streaming generation: the venue's chunked body.
type ChunkStream = Pin<Box<dyn Stream<Item = Result<Bytes, common_core::error::IoError>> + Send>>;

/// A parsed stream message before routing.
enum StreamMessage {
    /// Newest top-of-book for `symbol`.
    Quote {
        symbol: String,
        quote: crate::quote::Quote,
    },
    /// One public trade print for `symbol`.
    Print { symbol: String, print: TradePrint },
}

/// Largest accepted stream line: a never-terminated line is dropped and
/// counted rather than buffering the connection.
const MAX_LINE_BYTES: usize = 64 * 1024;

struct LiveSubscription {
    subscription: FeedSubscription,
    out: mpsc::Sender<MarketEvent>,
    stats: Arc<FeedStats>,
}

/// One Tastytrade feed over the shared transport.
pub struct TastytradeFeed {
    transport: Arc<TastytradeTransport>,
    net: Arc<NetCapability>,
    subscriptions: Mutex<Vec<LiveSubscription>>,
    /// Bar adapters keyed by `(symbol, timeframe_ms)`: prints feed every
    /// adapter for their symbol, so one print fans out to every timeframe.
    adapters: Mutex<HashMap<(String, i64), TradeBarAdapter>>,
    capacity: usize,
}

impl TastytradeFeed {
    /// Bind a feed to the transport and authenticated network capability.
    #[must_use]
    pub fn new(
        transport: Arc<TastytradeTransport>,
        net: Arc<NetCapability>,
        capacity: usize,
    ) -> Self {
        Self {
            transport,
            net,
            subscriptions: Mutex::new(Vec::new()),
            adapters: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
        }
    }

    /// Publish one externally received print to the matching subscriptions.
    /// Used by tests and by supervisors that own their own transport; the
    /// streaming pump below is the primary path and shares the same routing.
    pub fn publish_print(&self, print: TradePrint, underlying: &str) -> usize {
        self.route_print(underlying, print)
    }

    /// One streaming generation: POST the subscribed symbols, return the
    /// chunked body. HTTP auth-plane refusals are terminal; everything else
    /// reconnects with backoff.
    async fn connect_generation(&self) -> Result<ChunkStream, TransportError> {
        let base = self.transport.config().base_url.clone();
        if base.is_empty() {
            return Err(TransportError::Terminal(
                "tastytrade base URL unconfigured: refusing to dial".to_owned(),
            ));
        }
        let mut symbols: Vec<String> = self
            .live_subscriptions()
            .iter()
            .map(|live| live.0.symbol.clone())
            .collect();
        symbols.sort();
        symbols.dedup();
        let url = format!("{base}/market-data/stream");
        let body = serde_json::json!({ "symbols": symbols }).to_string();
        let stream = self
            .net
            .http_post_json_stream(&url, &body)
            .await
            .map_err(|error| stream_connect_error(&error))?;
        Ok(Box::pin(stream))
    }

    /// Consume one generation to a clean end: parse lines, route events, and
    /// update the quote cache. A mid-stream fault ends the generation (the
    /// policy reconnects); the abort preempts the wait for the next chunk.
    async fn pump_generation(
        &self,
        mut stream: ChunkStream,
        abort: &StreamAbort,
    ) -> crate::tastytrade::PumpEnd {
        use crate::tastytrade::PumpEnd;
        let mut buf = Vec::new();
        loop {
            let chunk = tokio::select! {
                () = abort.cancelled() => return PumpEnd::Aborted,
                next = stream.next() => next,
            };
            match chunk {
                None => {
                    return if self.live_count() == 0 {
                        PumpEnd::OutboxClosed
                    } else {
                        PumpEnd::InboxClosed
                    };
                }
                Some(Err(_)) => return PumpEnd::InboxClosed,
                Some(Ok(bytes)) => {
                    buf.extend_from_slice(&bytes);
                    self.drain_lines(&mut buf);
                    if self.live_count() == 0 {
                        return PumpEnd::OutboxClosed;
                    }
                }
            }
        }
    }

    /// Split complete lines off `buf` and route them; an over-long line is
    /// dropped and counted rather than buffering the connection.
    fn drain_lines(&self, buf: &mut Vec<u8>) {
        while let Some(end) = buf.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = buf.drain(..=end).collect();
            while line
                .last()
                .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
            {
                line.pop();
            }
            self.handle_line(&line);
        }
        if buf.len() > MAX_LINE_BYTES {
            buf.clear();
            self.record_malformed_all();
        }
    }

    /// Parse one line and route it; anything unparseable counts once per
    /// live session and never stalls the stream.
    fn handle_line(&self, line: &[u8]) {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            self.record_malformed_all();
            return;
        };
        match parse_stream_message(&value) {
            Ok(StreamMessage::Quote { symbol, quote }) => {
                self.transport.put_cached(&symbol, quote);
                let event = MarketEvent::Quote {
                    underlying: symbol.clone(),
                    quote,
                };
                self.route_event(&symbol, MarketEventKind::Quote, &event);
            }
            Ok(StreamMessage::Print { symbol, print }) => {
                self.route_print(&symbol, print);
            }
            Err(_) => self.record_malformed_all(),
        }
    }

    /// Fan one print out to print subscribers and every bar adapter for its
    /// symbol. Returns the print-subscriber delivery count; bar synthesis
    /// routes separately by timeframe.
    fn route_print(&self, underlying: &str, print: TradePrint) -> usize {
        let event = MarketEvent::TradePrint {
            underlying: underlying.to_owned(),
            print,
        };
        let delivered = self.route_event(underlying, MarketEventKind::TradePrint, &event);
        self.fold_print_into_bars(underlying, print);
        delivered
    }

    /// Fold one print into every bar adapter for its symbol: forming prints
    /// paint `BarUpdate { closed: false }`, a boundary close emits the
    /// `Candle` plus the closed `BarUpdate`, then the new forming update.
    /// Duplicates and out-of-order prints are counted by the adapter and
    /// never re-emitted.
    fn fold_print_into_bars(&self, underlying: &str, print: TradePrint) {
        let mut adapters = common_core::sync::lock(&self.adapters);
        for ((symbol, timeframe_ms), adapter) in adapters.iter_mut() {
            if symbol != underlying {
                continue;
            }
            let timeframe_ms = *timeframe_ms;
            match adapter.ingest(print) {
                Ok(AdapterOutcome::Duplicate | AdapterOutcome::OutOfOrder) => {}
                Ok(AdapterOutcome::Forming(parts)) => {
                    let event = MarketEvent::BarUpdate {
                        update: bar_update(underlying, parts, false),
                    };
                    self.route_bar(underlying, timeframe_ms, MarketEventKind::BarUpdate, &event);
                }
                Ok(AdapterOutcome::Closed { bar, forming }) => {
                    let closed_parts = parts_of_bar(&bar);
                    let event = MarketEvent::Candle {
                        underlying: underlying.to_owned(),
                        bar,
                    };
                    self.route_bar(underlying, timeframe_ms, MarketEventKind::Candle, &event);
                    let event = MarketEvent::BarUpdate {
                        update: bar_update(underlying, closed_parts, true),
                    };
                    self.route_bar(underlying, timeframe_ms, MarketEventKind::BarUpdate, &event);
                    let event = MarketEvent::BarUpdate {
                        update: bar_update(underlying, forming, false),
                    };
                    self.route_bar(underlying, timeframe_ms, MarketEventKind::BarUpdate, &event);
                }
                Err(_) => self.record_malformed_all(),
            }
        }
    }
    /// Snapshot the live subscriptions; the pump never holds the lock
    /// across an await.
    fn live_subscriptions(
        &self,
    ) -> Vec<(FeedSubscription, mpsc::Sender<MarketEvent>, Arc<FeedStats>)> {
        common_core::sync::lock(&self.subscriptions)
            .iter()
            .map(|live| {
                (
                    live.subscription.clone(),
                    live.out.clone(),
                    Arc::clone(&live.stats),
                )
            })
            .collect()
    }

    /// Live subscription count.
    fn live_count(&self) -> usize {
        common_core::sync::lock(&self.subscriptions).len()
    }

    /// Count one malformed line on every live session: the whole stream is
    /// degraded, not one symbol.
    fn record_malformed_all(&self) {
        for (_, _, stats) in self.live_subscriptions() {
            stats.record_malformed();
        }
    }

    /// Send one event to every subscriber for `(symbol, kind)`. Slow
    /// consumers count a skip; gone consumers are removed. Returns the
    /// delivery count.
    fn route_event(&self, symbol: &str, kind: MarketEventKind, event: &MarketEvent) -> usize {
        let mut live = common_core::sync::lock(&self.subscriptions);
        let mut delivered = 0;
        let mut index = 0;
        while index < live.len() {
            if live[index].subscription.symbol == symbol && live[index].subscription.kind == kind {
                match live[index].out.try_send(event.clone()) {
                    Ok(()) => {
                        delivered += 1;
                        index += 1;
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        live[index].stats.record_skipped();
                        index += 1;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        live.remove(index);
                    }
                }
            } else {
                index += 1;
            }
        }
        delivered
    }

    /// Send one synthesized bar event to the bar subscribers for
    /// `(symbol, timeframe_ms)`.
    fn route_bar(
        &self,
        symbol: &str,
        timeframe_ms: i64,
        kind: MarketEventKind,
        event: &MarketEvent,
    ) {
        let mut live = common_core::sync::lock(&self.subscriptions);
        let mut index = 0;
        while index < live.len() {
            let matches = live[index].subscription.symbol == symbol
                && live[index].subscription.kind == kind
                && live[index]
                    .subscription
                    .timeframe
                    .is_some_and(|timeframe| timeframe.duration_ms() == timeframe_ms);
            if !matches {
                index += 1;
                continue;
            }
            match live[index].out.try_send(event.clone()) {
                Ok(()) => index += 1,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    live[index].stats.record_skipped();
                    index += 1;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    live.remove(index);
                }
            }
        }
    }
}

impl MarketDataFeed for TastytradeFeed {
    fn capabilities(&self) -> FeedCapabilities {
        FeedCapabilities {
            quotes: true,
            trades: true,
            // Synthesized from prints through the bar adapters, never native.
            bars: true,
            native_bars: false,
            depth: false,
        }
    }

    /// The supervised streaming loop: one chunked generation per reconnect.
    /// A dropped generation reconnects with backoff under the shared policy;
    /// a terminal connect refusal stops at once.
    fn run_supervised<'a>(
        &'a self,
        config: &'a StreamConfig,
        abort: &'a StreamAbort,
    ) -> FeedFuture<'a, StreamOutcome> {
        Box::pin(async move {
            if self.live_subscriptions().is_empty() {
                return StreamOutcome::ConsumerGone;
            }
            let source = SupervisedSource::<ChunkStream>::new(config.clone());
            source
                .run(
                    || self.connect_generation(),
                    abort,
                    |stream| self.pump_generation(stream, abort),
                )
                .await
        })
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
                        "tastytrade feed publishes no depth deltas".to_owned(),
                    ));
                }
                if subscription.kind == MarketEventKind::BarUpdate
                    && subscription.timeframe.is_none()
                {
                    return Err(FeedError::Malformed(
                        "bar update subscription must name a timeframe".to_owned(),
                    ));
                }
            }
            let (out, inbox) = mpsc::channel(self.capacity);
            let stats = Arc::new(FeedStats::default());
            {
                let mut live = common_core::sync::lock(&self.subscriptions);
                let mut adapters = common_core::sync::lock(&self.adapters);
                for subscription in subscriptions {
                    live.push(LiveSubscription {
                        subscription: subscription.clone(),
                        out: out.clone(),
                        stats: Arc::clone(&stats),
                    });
                    if matches!(
                        subscription.kind,
                        MarketEventKind::Candle | MarketEventKind::BarUpdate
                    ) {
                        let timeframe = subscription
                            .timeframe
                            .expect("bar subscriptions name a timeframe");
                        adapters
                            .entry((subscription.symbol.clone(), timeframe.duration_ms()))
                            .or_insert_with(|| {
                                TradeBarAdapter::new(timeframe).expect("validated timeframe")
                            });
                    }
                }
            }
            Ok(FeedSession::new(
                inbox,
                fluent_concurrency::stream::StreamAbort::new(),
                stats,
            ))
        })
    }
}

/// Parse one NDJSON stream message. Unknown types and missing fields refuse;
/// nothing is defaulted into a different shape.
fn parse_stream_message(value: &serde_json::Value) -> Result<StreamMessage, FeedError> {
    let malformed =
        |detail: &str| FeedError::Malformed(format!("stream message refused: {detail}"));
    let kind = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| malformed("missing type"))?;
    let symbol = value
        .get("symbol")
        .and_then(serde_json::Value::as_str)
        .filter(|symbol| !symbol.trim().is_empty())
        .ok_or_else(|| malformed("missing symbol"))?
        .to_owned();
    let stamp = value
        .get("ts_ms")
        .and_then(serde_json::Value::as_i64)
        .and_then(|ts_ms| Utc.timestamp_millis_opt(ts_ms).single())
        .ok_or_else(|| malformed("missing ts_ms"))?;
    match kind {
        "quote" => {
            let money = |field: &str| {
                value
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .and_then(|raw| raw.parse::<Decimal>().ok())
                    .map(Money::new)
                    .ok_or_else(|| malformed(field))
            };
            let quote = crate::quote::Quote::new()
                .bid(money("bid")?)
                .ask(money("ask")?)
                .bid_size(
                    value
                        .get("bid_size")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                )
                .ask_size(
                    value
                        .get("ask_size")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                )
                .ts(stamp)
                .build();
            Ok(StreamMessage::Quote { symbol, quote })
        }
        "print" => {
            let price = value
                .get("price")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| raw.parse::<Decimal>().ok())
                .map(Money::new)
                .ok_or_else(|| malformed("price"))?;
            let size = value
                .get("size")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| malformed("size"))?;
            let aggressor = value
                .get("side")
                .and_then(serde_json::Value::as_str)
                .and_then(|side| match side {
                    "buy" => Some(Aggressor::Buy),
                    "sell" => Some(Aggressor::Sell),
                    _ => None,
                });
            Ok(StreamMessage::Print {
                symbol,
                print: TradePrint {
                    ts: stamp,
                    price,
                    size,
                    aggressor,
                },
            })
        }
        other => Err(malformed(&format!("unknown type {other:?}"))),
    }
}

/// View one closed `Bar` back as adapter parts for the closed paint update.
fn parts_of_bar(bar: &protopyne_types::Bar) -> protopyne_kernels::bars::BarParts {
    use rust_decimal::prelude::ToPrimitive as _;
    protopyne_kernels::bars::BarParts {
        ts_ms: bar.ts.timestamp_millis(),
        o: bar.o.to_f64().unwrap_or(0.0),
        h: bar.h.to_f64().unwrap_or(0.0),
        l: bar.l.to_f64().unwrap_or(0.0),
        c: bar.c.to_f64().unwrap_or(0.0),
        volume: bar.volume as f64,
        buy_volume: 0.0,
    }
}

/// Map a stream-connect failure into the retry taxonomy: auth-plane refusals
/// stop at once, everything else reconnects with backoff.
fn stream_connect_error(error: &common_core::error::IoError) -> TransportError {
    let text = error.to_string();
    let terminal = text
        .strip_prefix("HTTP ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| code == 401 || code == 403);
    if terminal {
        TransportError::Terminal(text)
    } else {
        TransportError::Rest(text)
    }
}

#[cfg(test)]
#[path = "../tests/tastytrade_feed.rs"]
mod tests;
