//! Tastytrade transport skeleton calibration: transformer reasons, loopback
//! REST refresh with retry shape, stale-primary fallback, stream
//! forwarding/drops, and scope-owned shutdown.
//!
//! The loopback speaks `200`-with-envelope because `NetCapability` surfaces
//! response bodies only (no status line): the envelope `error.code` is the
//! status until a later milestone teaches the capability to surface HTTP
//! status. Nothing here dials beyond `127.0.0.1`.

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

use crate::order::{OrderIntent, OrderRecord, OrderSide, OrderStatus};
use crate::quote::Quote;
use crate::venue::{AccountEvent, QuoteSource};
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

fn net_caps() -> CapabilitySet {
    CapabilitySet::new().with(NetCapability::new())
}

fn transport_for(base: &str) -> Arc<TastytradeTransport> {
    Arc::new(TastytradeTransport::new(
        TastytradeConfig::new().base_url(base.to_owned()).build(),
    ))
}

#[test]
fn config_defaults_and_json_round_trip() {
    let config = TastytradeConfig::default();
    assert_eq!(config.quote_max_age_secs, 60);
    assert_eq!(config.max_attempts, 3);

    let missing = std::path::Path::new("/nonexistent-tastytrade-config.json");
    assert_eq!(
        TastytradeConfig::load(missing),
        TastytradeConfig::default(),
        "missing file reads as default, never error"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tasty.json");
    std::fs::write(&path, r#"{"base_url":"http://127.0.0.1:9","quote_max_age_secs":30}"#)
        .expect("write");
    let loaded = TastytradeConfig::load(&path);
    assert_eq!(loaded.base_url, "http://127.0.0.1:9");
    assert_eq!(loaded.quote_max_age_secs, 30);
    assert_eq!(loaded.max_attempts, 3, "absent keys keep defaults");
}

#[test]
fn transformer_parses_quote_wire() {
    let quote = transform_quote(&quote_body()).expect("valid wire parses");
    assert_eq!(quote.bid, money("2.50"));
    assert_eq!(quote.ask, money("2.70"));
    assert_eq!((quote.bid_size, quote.ask_size), (50, 50));
}

#[test]
fn transformer_rejects_malformed_with_reasons() {
    let missing_ask = r#"{"bid":"2.50","bid_size":50,"ask_size":50,"ts":"2026-06-01T12:00:00Z"}"#;
    let reason = transform_quote(missing_ask).expect_err("ask is required");
    assert!(reason.contains("ask"), "names the missing field, got: {reason}");

    let negative_bid = r#"{"bid":"-1.00","ask":"2.70","bid_size":50,"ask_size":50,"ts":"2026-06-01T12:00:00Z"}"#;
    let reason = transform_quote(negative_bid).expect_err("bid must be non-negative");
    assert!(reason.contains("bid"), "got: {reason}");

    let crossed = r#"{"bid":"2.70","ask":"2.50","bid_size":50,"ask_size":50,"ts":"2026-06-01T12:00:00Z"}"#;
    let reason = transform_quote(crossed).expect_err("crossed refuses");
    assert!(reason.contains("crossed"), "got: {reason}");

    let bad_ts = r#"{"bid":"2.50","ask":"2.70","bid_size":50,"ask_size":50,"ts":"not-a-time"}"#;
    let reason = transform_quote(bad_ts).expect_err("ts must parse");
    assert!(reason.contains("ts"), "got: {reason}");

    assert!(transform_quote("not json").is_err(), "non-JSON never panics");
}

#[test]
fn validator_rejects_bad_subscriptions() {
    assert!(validate_subscription("SPY").is_ok());
    let reason = validate_subscription("").expect_err("empty refuses");
    assert!(reason.contains("empty"), "got: {reason}");
    let reason = validate_subscription("spy!").expect_err("punctuation refuses");
    assert!(reason.contains("symbol"), "got: {reason}");
    let long = "A".repeat(25);
    validate_subscription(&long).expect_err("overlong refuses");
}

#[test]
fn chain_transformer_parses_leg_quote() {
    let body = r#"{"underlying":"SPY","expiry":"2026-01-16","strike":"600","kind":"call","bid":"2.50","ask":"2.70","bid_size":10,"ask_size":12,"iv":0.25,"delta":0.6}"#;
    let quote = transform_chain_quote(body).expect("valid chain wire parses");
    assert_eq!(quote.contract.underlying, "SPY");
    assert_eq!(quote.bid, money("2.50"));
    assert_eq!((quote.bid_size, quote.ask_size), (10, 12));
    assert_eq!(quote.iv, Some(0.25));
    assert!((quote.greeks.expect("greeks").delta - 0.6).abs() < 1e-12);

    let bare = r#"{"underlying":"SPY","expiry":"2026-01-16","strike":"600","kind":"put","bid":"1.20","ask":"1.40","bid_size":10,"ask_size":12}"#;
    let quote = transform_chain_quote(bare).expect("greeks optional");
    assert!(quote.iv.is_none() && quote.greeks.is_none());

    let bad_kind = r#"{"underlying":"SPY","expiry":"2026-01-16","strike":"600","kind":"diagonal","bid":"1.20","ask":"1.40","bid_size":10,"ask_size":12}"#;
    let reason = transform_chain_quote(bad_kind).expect_err("kind must name call/put");
    assert!(reason.contains("kind"), "got: {reason}");
}

#[test]
fn account_event_transformer_routes_kinds() {
    let balance = r#"{"kind":"balance","asset":"USD","amount":"1000.00"}"#;
    match transform_account_event(balance).expect("balance routes") {
        AccountEvent::BalanceUpdated(entry) => {
            assert_eq!(entry.asset, "USD");
            assert_eq!(entry.amount, money("1000.00"));
        }
        other => panic!("wrong variant: {other:?}"),
    }

    let trade = r#"{"kind":"trade","quantity":2,"price":"600.50","fee":"1.30"}"#;
    match transform_account_event(trade).expect("trade routes") {
        AccountEvent::Trade(fill) => {
            assert_eq!(fill.quantity, 2);
            assert_eq!(fill.price, money("600.50"));
        }
        other => panic!("wrong variant: {other:?}"),
    }

    let intent = OrderIntent::new()
        .client_id("tt-1".to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build();
    let mut record = OrderRecord::pending(intent, now());
    record.status = OrderStatus::Acknowledged;
    let wire = serde_json::to_value(&record).expect("record serializes");
    let body = format!(r#"{{"kind":"order","order":{wire}}}"#);
    match transform_account_event(&body).expect("order routes") {
        AccountEvent::OrderUpdated(back) => assert_eq!(back, record),
        other => panic!("wrong variant: {other:?}"),
    }

    let unknown = r#"{"kind":"dividend"}"#;
    let reason = transform_account_event(unknown).expect_err("unknown kind refuses");
    assert!(reason.contains("kind"), "got: {reason}");
}

#[tokio::test]
async fn rest_refresh_serves_loopback_quote() {
    let (base, count) = serve_script(vec![quote_body()]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let quote = CURRENT_CAPS
        .scope(net_caps(), transport.refresh_quote(&net, "SPY"))
        .await
        .expect("loopback refresh serves");
    assert_eq!(quote.bid, money("2.50"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        transport.latest("SPY").expect("cached").bid,
        money("2.50"),
        "refresh populates the cache the ladder reads"
    );
}

#[tokio::test]
async fn transient_error_retries_then_succeeds() {
    let (base, count) = serve_script(vec![quote_body(), error_body("unavailable")]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let quote = CURRENT_CAPS
        .scope(net_caps(), transport.refresh_quote(&net, "SPY"))
        .await
        .expect("transient resolves on retry");
    assert_eq!(quote.ask, money("2.70"));
    assert_eq!(count.load(Ordering::SeqCst), 2, "exactly one retry");
}

#[tokio::test]
async fn terminal_error_stops_without_retry() {
    let (base, count) = serve_script(vec![error_body("unauthorized")]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let outcome = CURRENT_CAPS.scope(net_caps(), transport.refresh_quote(&net, "SPY")).await;
    assert!(
        matches!(outcome, Err(TransportError::Terminal(_))),
        "terminal is first-class Err, got: {outcome:?}"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1, "terminal never retries");
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

#[tokio::test]
async fn stale_primary_falls_back() {
    let stale = Quote::new()
        .bid(money("2.50"))
        .ask(money("2.70"))
        .bid_size(50)
        .ask_size(50)
        .ts(now() - chrono::Duration::seconds(120))
        .build();
    let primary = transport_for("http://127.0.0.1:9");
    primary.put_cached("SPY", stale);
    assert!(primary.latest("SPY").is_none(), "stale primary serves nothing");

    let fallback = FreshSource { quote: transform_quote(&quote_body()).expect("wire") };
    let served = crate::session::TradingSession::resolve_quote(
        vec![
            primary as Arc<dyn QuoteSource>,
            Arc::new(fallback) as Arc<dyn QuoteSource>,
        ],
        "SPY",
    )
    .await;
    let (quote, name) = served.expect("fallback serves");
    assert_eq!(name, "fallback");
    assert_eq!(quote.bid, money("2.50"));

    let primary_only: Vec<Arc<dyn QuoteSource>> =
        vec![transport_for("http://127.0.0.1:9") as Arc<dyn QuoteSource>];
    assert!(
        crate::session::TradingSession::resolve_quote(primary_only, "SPY").await.is_none(),
        "all-stale reads None, never blind"
    );
}

#[tokio::test]
async fn stream_forwards_valid_drops_malformed() {
    let transport = transport_for("http://127.0.0.1:9");
    let (inbox_tx, inbox_rx) = tokio::sync::mpsc::channel(8);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let mut scope = fluent_concurrency::scope::Scope::new();
    transport.run_stream(&mut scope, inbox_rx, out_tx);
    inbox_tx
        .send(r#"{"kind":"balance","asset":"USD","amount":"1000.00"}"#.to_owned())
        .await
        .expect("send");
    inbox_tx.send("not json".to_owned()).await.expect("send");
    inbox_tx
        .send(r#"{"kind":"trade","quantity":2,"price":"600.50","fee":"1.30"}"#.to_owned())
        .await
        .expect("send");
    drop(inbox_tx);

    let first = out_rx.recv().await.expect("first event");
    assert!(matches!(first, AccountEvent::BalanceUpdated(_)));
    let second = out_rx.recv().await.expect("second event");
    assert!(matches!(second, AccountEvent::Trade(_)));
    assert!(out_rx.recv().await.is_none(), "stream ends with the inbox");

    scope.close().await;
    assert_eq!(transport.dropped_messages(), 1, "malformed counted, not forwarded");
    assert_eq!(transport.rejections().len(), 1, "reason banked for review");
    assert_eq!(transport.active_streams(), 0, "no task leaks past close");
}
