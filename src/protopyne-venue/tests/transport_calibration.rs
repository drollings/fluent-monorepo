//! Reconnect/backoff + ladder policy calibration: the freeze behind the
//! retry predicate and the quote ladder.
//!
//! Measured calibration (this run, loopback only):
//!
//! * Terminal control group (`unauthorized`, `forbidden`, `bad_request`,
//!   `malformed_auth`): 0 false-retries — every terminal answers exactly
//!   one attempt as first-class `Err`.
//! * Transient group (`unavailable`, `rate_limited`, `timeout`, `reset`,
//!   unknown codes): full recall — every scripted transient resolves to a
//!   quote with exactly the scripted attempt count.
//! * Ladder precision/recall on stale/healthy mixes: 1.0 / 1.0.
//!
//! Two axes, labeled separately:
//!
//! * Producer-confidence (validity): attempt counts are exact (no hidden
//!   extra tries, no swallowed terminals); the `stop` predicate is the
//!   closed terminal set, everything else retries bounded by `max_attempts`.
//! * Task-value/correctness (economics): the ladder serves the first fresh
//!   rung and refuses (never guesses) when all rungs are dark; a gapped
//!   primary backfills through refresh before its quotes serve again; a
//!   slow subscriber skips lag instead of wedging the loop.
//!
//! No transport-state persistence anywhere in this module: the cache is
//! memory-only, and no test writes it anywhere.

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use chrono::{TimeZone, Utc};
use fluent_concurrency::io::net::NetCapability;
use fluent_wvr::{CURRENT_CAPS, CapabilitySet};
use rust_decimal::Decimal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::quote::Quote;
use crate::venue::QuoteSource;
use protopyne_types::Money;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn quote_body() -> String {
    format!(
        r#"{{"bid":"2.50","ask":"2.70","bid_size":50,"ask_size":50,"ts":"{}"}}"#,
        Utc::now().to_rfc3339()
    )
}

fn error_body(code: &str) -> String {
    format!(r#"{{"error":{{"code":"{code}","message":"scripted"}}}}"#)
}

fn http_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn read_head(stream: &mut tokio::net::TcpStream) {
    let mut buf = vec![0_u8; 4096];
    let mut head = Vec::new();
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                head.extend_from_slice(&buf[..n]);
                if head.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
}

/// Loopback REST script: each connection pops the next canned body.
/// Returns the base URL plus the served-request counter.
async fn serve_script(script: Vec<String>) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let count = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(std::sync::Mutex::new(script));
    let counted = Arc::clone(&count);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            read_head(&mut stream).await;
            let body = bodies
                .lock()
                .ok()
                .and_then(|mut guard| guard.pop())
                .unwrap_or_else(|| error_body("unavailable"));
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = stream.write_all(http_response(&body).as_bytes()).await;
        }
    });
    (base, count)
}

/// Loopback that resets the first connection (accept + drop, no bytes) and
/// then serves the script: the transport-level reset the envelope can never
/// name, so it must ride the retryable path by construction.
async fn serve_reset_once_then(script: Vec<String>) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let count = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(std::sync::Mutex::new(script));
    let counted = Arc::clone(&count);
    tokio::spawn(async move {
        let mut reset_done = false;
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            counted.fetch_add(1, Ordering::SeqCst);
            if !reset_done {
                reset_done = true;
                continue;
            }
            read_head(&mut stream).await;
            let body = bodies
                .lock()
                .ok()
                .and_then(|mut guard| guard.pop())
                .unwrap_or_else(|| error_body("unavailable"));
            let _ = stream.write_all(http_response(&body).as_bytes()).await;
        }
    });
    (base, count)
}

fn net_caps() -> CapabilitySet {
    CapabilitySet::new().with(NetCapability::new())
}

fn transport_for(base: &str) -> Arc<TastytradeTransport> {
    Arc::new(TastytradeTransport::new(
        TastytradeConfig::new().base_url(base.to_owned()).build(),
    ))
}

async fn refresh(
    transport: &TastytradeTransport,
    underlying: &str,
) -> Result<Quote, TransportError> {
    let net = NetCapability::new();
    CURRENT_CAPS
        .scope(net_caps(), transport.refresh_quote(&net, underlying))
        .await
}

fn fresh_quote() -> Quote {
    Quote::new()
        .bid(money("2.50"))
        .ask(money("2.70"))
        .bid_size(50)
        .ask_size(50)
        .ts(Utc::now())
        .build()
}

fn stale_quote() -> Quote {
    Quote::new()
        .bid(money("2.50"))
        .ask(money("2.70"))
        .bid_size(50)
        .ask_size(50)
        .ts(now() - chrono::Duration::seconds(120))
        .build()
}

struct FreshSource {
    quote: Quote,
}

impl QuoteSource for FreshSource {
    fn latest(&self, _underlying: &str) -> Option<Quote> {
        Some(self.quote)
    }

    fn name(&self) -> &str {
        "fallback"
    }
}

struct DarkSource;

impl QuoteSource for DarkSource {
    fn latest(&self, _underlying: &str) -> Option<Quote> {
        None
    }

    fn name(&self) -> &str {
        "dark"
    }
}

#[test]
fn backoff_params_are_frozen() {
    // The retry schedule this calibration measures. Any production move of
    // these constants must first re-earn the terminal-zero/transient-recall
    // tallies below.
    assert_eq!(RETRY_MAX_ATTEMPTS, 3);
    assert_eq!(RETRY_BACKOFF_BASE_MS, 100);
    assert_eq!(RETRY_JITTER_PCT, 50);
    let config = TastytradeConfig::default();
    assert_eq!(config.max_attempts, RETRY_MAX_ATTEMPTS);
    assert_eq!(config.backoff_base_ms, RETRY_BACKOFF_BASE_MS);
}

#[test]
fn stop_predicate_table() {
    assert!(!TransportError::Terminal("401".to_owned()).is_retryable());
    assert!(TransportError::Transient("unavailable".to_owned()).is_retryable());
    assert!(TransportError::Rest("reset".to_owned()).is_retryable());
}

#[tokio::test]
async fn terminal_control_group_shows_zero_retries() {
    for code in ["unauthorized", "forbidden", "bad_request", "malformed_auth"] {
        let (base, count) = serve_script(vec![error_body(code)]).await;
        let outcome = refresh(&transport_for(&base), "SPY").await;
        assert!(
            matches!(outcome, Err(TransportError::Terminal(_))),
            "{code} stops first-class, got: {outcome:?}"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "{code} shows zero false-retries"
        );
    }
}

#[tokio::test]
async fn transient_group_resolves_with_exact_retries() {
    // Two transients then success: exactly two retries, then the quote.
    let (base, count) =
        serve_script(vec![quote_body(), error_body("unavailable"), error_body("unavailable")])
            .await;
    let quote = refresh(&transport_for(&base), "SPY")
        .await
        .expect("transients resolve");
    assert_eq!(quote.ask, money("2.70"));
    assert_eq!(count.load(Ordering::SeqCst), 3, "exactly two retries");

    // One rate-limit then success.
    let (base, count) = serve_script(vec![quote_body(), error_body("rate_limited")]).await;
    refresh(&transport_for(&base), "SPY").await.expect("rate limit resolves");
    assert_eq!(count.load(Ordering::SeqCst), 2);

    // Named timeout envelope is transient, not terminal.
    let (base, count) = serve_script(vec![quote_body(), error_body("timeout")]).await;
    refresh(&transport_for(&base), "SPY").await.expect("timeout resolves");
    assert_eq!(count.load(Ordering::SeqCst), 2);

    // Unknown codes retry bounded and resolve: the terminal set is closed,
    // everything else leans transient until max attempts fail it closed.
    let (base, count) = serve_script(vec![quote_body(), error_body("future_code")]).await;
    refresh(&transport_for(&base), "SPY").await.expect("unknown resolves");
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn transport_reset_retries() {
    let (base, count) = serve_reset_once_then(vec![quote_body()]).await;
    let quote = refresh(&transport_for(&base), "SPY")
        .await
        .expect("reset resolves on retry");
    assert_eq!(quote.bid, money("2.50"));
    assert_eq!(count.load(Ordering::SeqCst), 2, "reset costs one retry");
}

#[tokio::test]
async fn ladder_precision_and_recall_are_one() {
    // (primary state, fallback state, expected server).
    let fresh = fresh_quote();
    let cases: Vec<(Option<Quote>, Option<Quote>, Option<&str>)> = vec![
        (Some(fresh), Some(fresh), Some("tastytrade")),
        (None, Some(fresh), Some("fallback")),
        (Some(fresh), None, Some("tastytrade")),
        (None, None, None),
        (Some(fresh), Some(fresh), Some("tastytrade")),
    ];
    let mut served_correct = 0_usize;
    let mut served_total = 0_usize;
    let mut should_total = 0_usize;
    for (primary_quote, fallback_quote, expected) in cases {
        let primary = transport_for("http://127.0.0.1:9");
        if let Some(quote) = primary_quote {
            primary.put_cached("SPY", quote);
        } else {
            primary.put_cached("SPY", stale_quote());
        }
        let fallback: Arc<dyn QuoteSource> = match fallback_quote {
            Some(quote) => Arc::new(FreshSource { quote }),
            None => Arc::new(DarkSource),
        };
        let served = crate::session::TradingSession::resolve_quote(
            vec![primary as Arc<dyn QuoteSource>, fallback],
            "SPY",
        )
        .await
        .map(|(_, name)| name);
        let correct = served.as_deref() == expected;
        assert!(correct, "ladder rung diverged: served {served:?}, want {expected:?}");
        if served.is_some() {
            served_total += 1;
        }
        if expected.is_some() {
            should_total += 1;
        }
        if correct && served.is_some() {
            served_correct += 1;
        }
    }
    assert_eq!(served_correct, served_total, "precision: served implies should-serve");
    assert_eq!(served_correct, should_total, "recall: should-serve implies served");
    assert!(should_total > 0, "the mix must contain servable cases");
}

#[tokio::test]
async fn gapped_primary_backfills_before_resuming() {
    // t0: primary fresh, serves. t1: primary stale, fallback covers the gap
    // (no evaluation on the gapped series). t2: refresh backfills the cache
    // and primary serves again.
    let (base, _) = serve_script(vec![quote_body()]).await;
    let primary = transport_for(&base);
    let fallback: Arc<dyn QuoteSource> = Arc::new(FreshSource { quote: fresh_quote() });

    primary.put_cached("SPY", fresh_quote());
    let (_, first) = crate::session::TradingSession::resolve_quote(
        vec![primary.clone() as Arc<dyn QuoteSource>, Arc::clone(&fallback)],
        "SPY",
    )
    .await
    .expect("t0 serves");
    assert_eq!(first, "tastytrade");

    primary.put_cached("SPY", stale_quote());
    let (_, gap) = crate::session::TradingSession::resolve_quote(
        vec![primary.clone() as Arc<dyn QuoteSource>, Arc::clone(&fallback)],
        "SPY",
    )
    .await
    .expect("gap still serves");
    assert_eq!(gap, "fallback", "gapped primary evaluates nothing");

    refresh(&primary, "SPY").await.expect("backfill refreshes");
    let (_, resumed) = crate::session::TradingSession::resolve_quote(
        vec![primary as Arc<dyn QuoteSource>, fallback],
        "SPY",
    )
    .await
    .expect("t2 serves");
    assert_eq!(resumed, "tastytrade", "backfilled primary resumes");
}

#[tokio::test]
async fn slow_subscriber_lag_skips() {
    let transport = transport_for("http://127.0.0.1:9");
    let (inbox_tx, inbox_rx) = tokio::sync::mpsc::channel(8);
    // Outbox holds one: the loop must skip lag, never wedge on it.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(1);
    let mut scope = fluent_concurrency::scope::Scope::new();
    transport.run_stream(&mut scope, inbox_rx, out_tx);
    for _ in 0..3 {
        inbox_tx
            .send(r#"{"kind":"balance","asset":"USD","amount":"1000.00"}"#.to_owned())
            .await
            .expect("send");
    }
    drop(inbox_tx);

    let first = out_rx.recv().await.expect("first event lands");
    assert!(matches!(first, crate::venue::AccountEvent::BalanceUpdated(_)));
    scope.close().await;
    assert_eq!(
        transport.skipped_messages(),
        2,
        "slow subscriber skips lag, keeps the first"
    );
    assert_eq!(transport.dropped_messages(), 0, "skips are lag, not malformed");
    assert_eq!(transport.active_streams(), 0);
}
