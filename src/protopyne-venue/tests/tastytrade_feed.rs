//! Supervised Tastytrade streaming transport: chunked quote/print delivery,
//! reconnect resumption, malformed counting, and adapter-backed bars — all
//! against a loopback chunked-NDJSON fake. Nothing here dials beyond
//! `127.0.0.1`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use fluent_concurrency::io::net::NetCapability;
use fluent_wvr::{CapabilitySet, CURRENT_CAPS};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::feed::{FeedSubscription, MarketDataFeed};
use crate::market::{MarketEvent, MarketEventKind};
use crate::stream::StreamConfig;
use crate::tastytrade::{TastytradeConfig, TastytradeTransport};
use crate::venue::QuoteSource;

fn net_caps() -> CapabilitySet {
    CapabilitySet::new().with(NetCapability::new())
}

async fn scoped<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    CURRENT_CAPS.scope(net_caps(), f).await
}

fn stream_config() -> StreamConfig {
    StreamConfig::new()
        .backoff_base_ms(1)
        .backoff_jitter_pct(0)
        .max_consecutive_errors(32)
        .restart_window_secs(60)
        .max_restarts_per_window(64)
        .build()
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

fn print_line(symbol: &str, ts_ms: i64, size: u64) -> String {
    serde_json::json!({
        "type": "print",
        "symbol": symbol,
        "price": "100.50",
        "size": size,
        "ts_ms": ts_ms,
    })
    .to_string()
}

/// One scripted connection: newline-delimited messages, then either a clean
/// terminating chunk or an abrupt close.
struct Scripted {
    chunks: Vec<String>,
    clean_end: bool,
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

/// Serve each scripted connection in order on loopback, then stop accepting.
async fn serve_scripted(scripts: Vec<Scripted>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        for script in scripts {
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
                if let Some(end) = find_head_end(&buf) {
                    let head = String::from_utf8_lossy(&buf[..end]);
                    assert!(
                        head.starts_with("POST /market-data/stream "),
                        "the stream path is pinned, got {head}"
                    );
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
                        buf.extend_from_slice(&tmp[..n]);
                        have += n;
                    }
                    break;
                }
            }
            let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                .to_vec();
            for chunk in &script.chunks {
                response
                    .extend_from_slice(format!("{:x}\r\n{}\r\n", chunk.len(), chunk).as_bytes());
            }
            if script.clean_end {
                response.extend_from_slice(b"0\r\n\r\n");
            }
            stream.write_all(&response).await.expect("response writes");
        }
    });
    format!("http://{addr}")
}

fn feed_for(
    base: &str,
    capacity: usize,
) -> (
    crate::tastytrade_feed::TastytradeFeed,
    Arc<TastytradeTransport>,
) {
    let transport = Arc::new(TastytradeTransport::new(
        TastytradeConfig::new().base_url(base.to_owned()).build(),
    ));
    let net = Arc::new(NetCapability::new());
    let feed = crate::tastytrade_feed::TastytradeFeed::new(transport.clone(), net, capacity);
    (feed, transport)
}

async fn drain_to(session: &mut crate::feed::FeedSession, count: usize) -> Vec<MarketEvent> {
    let mut events = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while events.len() < count {
            events.extend(session.try_drain());
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("streamed events arrive");
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_stream_publishes_quotes_and_prints_and_updates_cache() {
    let now_ms = Utc::now().timestamp_millis();
    let base = serve_scripted(vec![Scripted {
        chunks: vec![
            quote_line("SPY", now_ms) + "\n",
            print_line("SPY", now_ms, 4) + "\n",
        ],
        clean_end: true,
    }])
    .await;
    scoped(async {
        let (feed, transport) = feed_for(&base, 16);
        let mut session = feed
            .subscribe(&[
                FeedSubscription::of("SPY", MarketEventKind::Quote),
                FeedSubscription::of("SPY", MarketEventKind::TradePrint),
            ])
            .await
            .expect("quote and print subscribe");
        let abort = fluent_concurrency::stream::StreamAbort::new();
        let run_abort = abort.clone();
        // Task-locals do not cross `spawn`: re-enter the network capability
        // scope inside the spawned pump task.
        let caps = net_caps();
        let run = tokio::spawn(
            CURRENT_CAPS.scope(caps, async move { feed.run_supervised(&stream_config(), &run_abort).await }),
        );
        let events = drain_to(&mut session, 2).await;
        abort.cancel();
        let outcome = run.await.expect("run ends");
        assert!(
            matches!(outcome, crate::stream::StreamOutcome::Aborted),
            "cancel ends the supervised run, got {outcome:?}"
        );
        assert!(
            matches!(
                &events[0],
                MarketEvent::Quote { underlying, .. } if underlying == "SPY"
            ),
            "first event is the quote, got {:?}",
            events[0]
        );
        assert!(
            matches!(
                &events[1],
                MarketEvent::TradePrint { underlying, print } if underlying == "SPY" && print.size == 4
            ),
            "second event is the print, got {:?}",
            events[1]
        );
        let cached = transport.latest("SPY").expect("streamed quote is cached");
        assert_eq!(
            cached.ts.timestamp_millis(),
            now_ms,
            "the cache update rides the stream, not a poll"
        );
        assert_eq!(session.stats.snapshot().malformed, 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_stream_reconnects_after_a_dropped_generation() {
    let now_ms = Utc::now().timestamp_millis();
    let base = serve_scripted(vec![
        Scripted {
            chunks: vec![quote_line("SPY", now_ms) + "\n"],
            clean_end: false,
        },
        Scripted {
            chunks: vec![quote_line("SPY", now_ms + 1) + "\n"],
            clean_end: true,
        },
    ])
    .await;
    scoped(async {
        let (feed, _) = feed_for(&base, 16);
        let mut session = feed
            .subscribe(&[FeedSubscription::of("SPY", MarketEventKind::Quote)])
            .await
            .expect("quote subscribes");
        let abort = fluent_concurrency::stream::StreamAbort::new();
        let run_abort = abort.clone();
        // Task-locals do not cross `spawn`: re-enter the network capability
        // scope inside the spawned pump task.
        let caps = net_caps();
        let run = tokio::spawn(CURRENT_CAPS.scope(caps, async move {
            feed.run_supervised(&stream_config(), &run_abort).await
        }));
        let events = drain_to(&mut session, 2).await;
        abort.cancel();
        let outcome = run.await.expect("run ends");
        assert!(
            matches!(outcome, crate::stream::StreamOutcome::Aborted),
            "cancel ends the resumed run, got {outcome:?}"
        );
        let stamps: Vec<i64> = events
            .iter()
            .map(|event| match event {
                MarketEvent::Quote { quote, .. } => quote.ts.timestamp_millis(),
                other => panic!("quotes only, got {other:?}"),
            })
            .collect();
        assert_eq!(
            stamps,
            vec![now_ms, now_ms + 1],
            "both generations deliver: the drop reconnected, nothing replayed"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_stream_counts_malformed_and_skips_without_stalling() {
    let now_ms = Utc::now().timestamp_millis();
    let base = serve_scripted(vec![Scripted {
        chunks: vec![
            "this is not json\n".to_owned(),
            "{\"type\":\"halting\",\"symbol\":\"SPY\"}\n".to_owned(),
            quote_line("SPY", now_ms) + "\n",
        ],
        clean_end: true,
    }])
    .await;
    scoped(async {
        let (feed, _) = feed_for(&base, 1);
        let mut session = feed
            .subscribe(&[FeedSubscription::of("SPY", MarketEventKind::Quote)])
            .await
            .expect("quote subscribes");
        let abort = fluent_concurrency::stream::StreamAbort::new();
        let run_abort = abort.clone();
        // Task-locals do not cross `spawn`: re-enter the network capability
        // scope inside the spawned pump task.
        let caps = net_caps();
        let run = tokio::spawn(CURRENT_CAPS.scope(caps, async move {
            feed.run_supervised(&stream_config(), &run_abort).await
        }));
        let events = drain_to(&mut session, 1).await;
        // Let the pump finish the script, then stop the reconnect loop.
        tokio::time::sleep(Duration::from_millis(300)).await;
        abort.cancel();
        let _ = run.await.expect("run ends");
        assert_eq!(events.len(), 1, "the one good quote lands");
        let snapshot = session.stats.snapshot();
        assert_eq!(
            snapshot.malformed, 2,
            "both bad lines count, got {snapshot:?}"
        );
        assert_eq!(snapshot.received, 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_stream_synthesizes_closed_bars_without_forming_candles() {
    let base = serve_scripted(vec![Scripted {
        chunks: vec![
            print_line("SPY", 0, 1) + "\n",
            print_line("SPY", 10_000, 1) + "\n",
            print_line("SPY", 50_000, 1) + "\n",
            print_line("SPY", 60_000, 2) + "\n",
            print_line("SPY", 70_000, 3) + "\n",
        ],
        clean_end: true,
    }])
    .await;
    scoped(async {
        use protopyne_types::Timeframe;
        let (feed, _) = feed_for(&base, 64);
        let mut session = feed
            .subscribe(&[
                FeedSubscription::bars("SPY", Timeframe::M1),
                FeedSubscription {
                    symbol: "SPY".to_owned(),
                    kind: MarketEventKind::BarUpdate,
                    timeframe: Some(Timeframe::M1),
                },
            ])
            .await
            .expect("bar subscriptions subscribe");
        let abort = fluent_concurrency::stream::StreamAbort::new();
        let run_abort = abort.clone();
        // Task-locals do not cross `spawn`: re-enter the network capability
        // scope inside the spawned pump task.
        let caps = net_caps();
        let run = tokio::spawn(CURRENT_CAPS.scope(caps, async move {
            feed.run_supervised(&stream_config(), &run_abort).await
        }));
        // One closed bar plus its forming updates: 5 forming prints, one
        // boundary close, one post-boundary forming print.
        let events = drain_to(&mut session, 7).await;
        abort.cancel();
        let _ = run.await.expect("run ends");
        let mut candles = 0;
        let mut closed_updates = 0;
        let mut forming_updates = 0;
        for event in &events {
            match event {
                MarketEvent::Candle { underlying, bar } => {
                    assert_eq!(underlying, "SPY");
                    assert_eq!(
                        bar.ts.timestamp_millis(),
                        0,
                        "the closed bar is bucket [0,60)"
                    );
                    assert_eq!(bar.volume, 3, "closed volume counts its own prints only");
                    candles += 1;
                }
                MarketEvent::BarUpdate { update } => {
                    if update.closed {
                        closed_updates += 1;
                    } else {
                        forming_updates += 1;
                    }
                }
                other => panic!("bars only, got {other:?}"),
            }
        }
        assert_eq!(candles, 1, "exactly one closed bar, from the one boundary");
        assert_eq!(closed_updates, 1, "the close paints once");
        assert_eq!(forming_updates, 5, "every print moves the forming bar");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_stream_refuses_unsupported_subscriptions() {
    use protopyne_types::Timeframe;
    scoped(async {
        let (feed, _) = feed_for("http://127.0.0.1:9", 8);
        let subscribed = feed
            .subscribe(&[FeedSubscription::bars("SPY", Timeframe::D1)])
            .await;
        assert!(subscribed.is_ok(), "synthesized bars subscribe");
        let refused = feed
            .subscribe(&[FeedSubscription::of("SPY", MarketEventKind::BookDelta)])
            .await;
        assert!(
            refused.is_err(),
            "depth deltas refuse: the feed publishes none"
        );
        let refused = feed
            .subscribe(&[FeedSubscription::of("  ", MarketEventKind::Quote)])
            .await;
        assert!(refused.is_err(), "blank symbols refuse");
        let refused = feed
            .subscribe(&[FeedSubscription::of("SPY", MarketEventKind::BarUpdate)])
            .await;
        assert!(refused.is_err(), "a bar update without a timeframe refuses");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_stream_without_consumers_ends_consumer_gone() {
    scoped(async {
        let (feed, _) = feed_for("http://127.0.0.1:9", 8);
        let abort = fluent_concurrency::stream::StreamAbort::new();
        let outcome = feed.run_supervised(&stream_config(), &abort).await;
        assert!(
            matches!(outcome, crate::stream::StreamOutcome::ConsumerGone),
            "nothing subscribed means nothing to serve, got {outcome:?}"
        );
    })
    .await;
}
