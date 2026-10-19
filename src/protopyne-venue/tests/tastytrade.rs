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
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use chrono::{TimeZone, Utc};
use fluent_concurrency::io::net::NetCapability;
use fluent_wvr::{CapabilitySet, CURRENT_CAPS};
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
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
    std::fs::write(
        &path,
        r#"{"base_url":"http://127.0.0.1:9","quote_max_age_secs":30}"#,
    )
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
    assert!(
        reason.contains("ask"),
        "names the missing field, got: {reason}"
    );

    let negative_bid =
        r#"{"bid":"-1.00","ask":"2.70","bid_size":50,"ask_size":50,"ts":"2026-06-01T12:00:00Z"}"#;
    let reason = transform_quote(negative_bid).expect_err("bid must be non-negative");
    assert!(reason.contains("bid"), "got: {reason}");

    let crossed =
        r#"{"bid":"2.70","ask":"2.50","bid_size":50,"ask_size":50,"ts":"2026-06-01T12:00:00Z"}"#;
    let reason = transform_quote(crossed).expect_err("crossed refuses");
    assert!(reason.contains("crossed"), "got: {reason}");

    let bad_ts = r#"{"bid":"2.50","ask":"2.70","bid_size":50,"ask_size":50,"ts":"not-a-time"}"#;
    let reason = transform_quote(bad_ts).expect_err("ts must parse");
    assert!(reason.contains("ts"), "got: {reason}");

    assert!(
        transform_quote("not json").is_err(),
        "non-JSON never panics"
    );
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
    let outcome = CURRENT_CAPS
        .scope(net_caps(), transport.refresh_quote(&net, "SPY"))
        .await;
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
    assert!(
        primary.latest("SPY").is_none(),
        "stale primary serves nothing"
    );

    let fallback = FreshSource {
        quote: transform_quote(&quote_body()).expect("wire"),
    };
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
        crate::session::TradingSession::resolve_quote(primary_only, "SPY")
            .await
            .is_none(),
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
    assert_eq!(
        transport.dropped_messages(),
        1,
        "malformed counted, not forwarded"
    );
    assert_eq!(transport.rejections().len(), 1, "reason banked for review");
    assert_eq!(transport.active_streams(), 0, "no task leaks past close");
}

/// Chain-snapshot fetch over the documented envelope: success is
/// `{"quotes":[<chain-wire>, ...]}` with each entry in the
/// `transform_chain_quote` shape; errors ride the same `error.code`
/// envelope as the quote path (terminal codes stop, the rest retry).
fn chain_wire(ts: &str, strike: &str, kind: &str, bid: &str, ask: &str) -> String {
    format!(
        r#"{{"underlying":"SPY","expiry":"2026-07-17","strike":"{strike}","kind":"{kind}","bid":"{bid}","ask":"{ask}","bid_size":50,"ask_size":50,"ts":"{ts}"}}"#
    )
}

fn snapshot_body(wires: &[String]) -> String {
    format!(r#"{{"quotes":[{}]}}"#, wires.join(","))
}

/// Loopback chain script that also banks the `Authorization` header it saw,
/// so the fetch proves it authenticates without the test ever reading the
/// token back out of an error.
async fn serve_chain_script(
    script: Vec<String>,
) -> (String, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let count = Arc::new(AtomicUsize::new(0));
    let seen_auth = Arc::new(std::sync::Mutex::new(Vec::new()));
    let bodies = Arc::new(std::sync::Mutex::new(script));
    let counted = Arc::clone(&count);
    let seen = Arc::clone(&seen_auth);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0_u8; 8192];
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
            let text = String::from_utf8_lossy(&head).into_owned();
            let auth = text
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
                .map(|line| {
                    line.split_once(':')
                        .map_or("", |part| part.1)
                        .trim()
                        .to_owned()
                })
                .unwrap_or_default();
            seen.lock().expect("auth log locks").push(auth);
            let body = bodies
                .lock()
                .ok()
                .and_then(|mut guard| guard.pop())
                .unwrap_or_else(|| error_body("unavailable"));
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = stream.write_all(http_response(&body).as_bytes()).await;
        }
    });
    (base, count, seen_auth)
}

#[tokio::test]
async fn chain_snapshot_fetch_serves_loopback_wires() {
    let ts = "2026-01-09T22:00:00Z";
    let wires = vec![
        chain_wire(ts, "690", "call", "3", "4"),
        chain_wire(ts, "695", "call", "2", "3"),
    ];
    let (base, count, seen_auth) = serve_chain_script(vec![snapshot_body(&wires)]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let quotes = CURRENT_CAPS
        .scope(
            net_caps(),
            transport.fetch_chain_snapshot(&net, "SPY", "tok-1"),
        )
        .await
        .expect("loopback snapshot serves");
    assert_eq!(quotes.len(), 2, "one quote per wire entry");
    assert_eq!(quotes[0].contract.strike, money("690"));
    assert_eq!(quotes[1].contract.strike, money("695"));
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "one request serves one snapshot"
    );
    assert_eq!(
        seen_auth.lock().expect("auth log locks").as_slice(),
        ["Bearer tok-1"],
        "the fetch authenticates on the wire"
    );
}

#[tokio::test]
async fn chain_snapshot_terminal_error_stops_without_retry() {
    let (base, count, _) = serve_chain_script(vec![error_body("unauthorized")]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let outcome = CURRENT_CAPS
        .scope(
            net_caps(),
            transport.fetch_chain_snapshot(&net, "SPY", "tok-1"),
        )
        .await;
    assert!(
        matches!(outcome, Err(TransportError::Terminal(_))),
        "terminal is first-class Err, got: {outcome:?}"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1, "terminal never retries");
}

#[tokio::test]
async fn chain_snapshot_transient_error_retries_then_succeeds() {
    let ts = "2026-01-09T22:00:00Z";
    let good = snapshot_body(&[chain_wire(ts, "690", "call", "3", "4")]);
    let (base, count, _) = serve_chain_script(vec![good, error_body("unavailable")]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let quotes = CURRENT_CAPS
        .scope(
            net_caps(),
            transport.fetch_chain_snapshot(&net, "SPY", "tok-1"),
        )
        .await
        .expect("transient resolves on retry");
    assert_eq!(quotes.len(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 2, "exactly one retry");
}

#[tokio::test]
async fn chain_snapshot_malformed_entry_refuses_the_whole_snapshot() {
    let ts = "2026-01-09T22:00:00Z";
    let wires = vec![
        chain_wire(ts, "690", "call", "3", "4"),
        chain_wire(ts, "690", "sideways", "3", "4"),
    ];
    // Served once: a deterministic entry fault is terminal, so one body
    // suffices — no retry would ever meet a different entry.
    let bad = snapshot_body(&wires);
    let (base, count, _) = serve_chain_script(vec![bad]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    // The refusal is the entry itself, never a partial snapshot.
    let outcome = CURRENT_CAPS
        .scope(
            net_caps(),
            transport.fetch_chain_snapshot(&net, "SPY", "tok-1"),
        )
        .await;
    let reason = outcome.expect_err("a bad entry refuses the snapshot");
    assert!(
        matches!(reason, TransportError::Terminal(_)),
        "a deterministic fault is terminal, got: {reason:?}"
    );
    assert!(
        reason.to_string().contains("kind"),
        "names the entry fault, got: {reason}"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "a terminal fault attempts once"
    );
}

#[tokio::test]
async fn chain_snapshot_empty_quotes_refuse_as_partial_window() {
    let (base, count, _) = serve_chain_script(vec![r#"{"quotes":[]}"#.to_owned()]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let outcome = CURRENT_CAPS
        .scope(
            net_caps(),
            transport.fetch_chain_snapshot(&net, "SPY", "tok-1"),
        )
        .await;
    assert!(
        matches!(outcome, Err(TransportError::Terminal(_))),
        "an empty window is terminal, never an empty file, got: {outcome:?}"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "an empty snapshot attempts once"
    );
}

#[tokio::test]
async fn chain_snapshot_missing_auth_or_config_refuses_before_dial() {
    let (base, count, _) = serve_chain_script(vec![snapshot_body(&[chain_wire(
        "2026-01-09T22:00:00Z",
        "690",
        "call",
        "3",
        "4",
    )])])
    .await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let outcome = CURRENT_CAPS
        .scope(net_caps(), transport.fetch_chain_snapshot(&net, "SPY", ""))
        .await;
    assert!(outcome.is_err(), "an empty token refuses before any dial");
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "no request leaves without credentials"
    );
    let unconfigured = Arc::new(TastytradeTransport::new(TastytradeConfig::new().build()));
    let outcome = CURRENT_CAPS
        .scope(
            net_caps(),
            unconfigured.fetch_chain_snapshot(&net, "SPY", "tok-1"),
        )
        .await;
    assert!(
        outcome.is_err(),
        "an unconfigured base refuses without dialing"
    );
    assert_eq!(count.load(Ordering::SeqCst), 0, "still no request");
}

// ── Order-wire transform ────────────────────────────────────────────────

use chrono::NaiveDate;
use protopyne_types::{LegSide, OptionContract, OptionKind, SpreadLeg, SpreadLegs};

fn contract(strike: &str, kind: OptionKind) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(NaiveDate::from_ymd_opt(2026, 1, 16).unwrap())
        .strike(money(strike))
        .kind(kind)
        .build()
}

fn vertical() -> OrderIntent {
    let legs = SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(contract("600", OptionKind::Call))
            .side(LegSide::Long)
            .build(),
        SpreadLeg::new()
            .contract(contract("605", OptionKind::Call))
            .side(LegSide::Short)
            .build(),
    ])
    .expect("vertical legs");
    OrderIntent::new()
        .client_id("run-7-0#0".to_owned())
        .proposal_id("run-7-0".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("1.50"))
        .maybe_legs(Some(legs))
        .build()
}

fn condor() -> OrderIntent {
    let legs = SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(contract("590", OptionKind::Put))
            .side(LegSide::Long)
            .build(),
        SpreadLeg::new()
            .contract(contract("595", OptionKind::Put))
            .side(LegSide::Short)
            .build(),
        SpreadLeg::new()
            .contract(contract("605", OptionKind::Call))
            .side(LegSide::Short)
            .build(),
        SpreadLeg::new()
            .contract(contract("610", OptionKind::Call))
            .side(LegSide::Long)
            .build(),
    ])
    .expect("condor legs");
    OrderIntent::new()
        .client_id("run-7-1#0".to_owned())
        .proposal_id("run-7-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Sell)
        .quantity(1)
        .limit_price(money("-1.20"))
        .maybe_legs(Some(legs))
        .build()
}

#[test]
fn order_wire_round_trips_a_vertical_and_a_condor() {
    for intent in [vertical(), condor()] {
        let wire = order_wire_of(&intent).expect("wire builds");
        let back = order_intent_of(&wire).expect("wire rebuilds the intent");
        assert_eq!(back.client_id, intent.client_id);
        assert_eq!(back.side, intent.side);
        assert_eq!(back.quantity, intent.quantity);
        assert_eq!(back.limit_price, intent.limit_price);
        assert_eq!(back.time_in_force, intent.time_in_force);
        assert_eq!(back.underlying, intent.underlying);
        assert_eq!(back.legs, intent.legs, "legs round-trip by OCC symbol");
    }
}

#[test]
fn signed_package_limit_sets_the_price_effect_and_order_type() {
    let debit = order_wire_of(&vertical()).expect("debit wire");
    assert_eq!(debit["price-effect"], "Debit");
    assert_eq!(debit["price"], "1.50");
    assert_eq!(debit["order-type"], "Limit");
    assert_eq!(debit["instrument-type"], "Equity Option");
    assert_eq!(debit["legs"][0]["action"], "Buy to Open");
    assert_eq!(debit["legs"][1]["action"], "Sell to Open");
    assert_eq!(debit["legs"][0]["quantity"], 2);

    let credit = order_wire_of(&condor()).expect("credit wire");
    assert_eq!(credit["price-effect"], "Credit");
    assert_eq!(credit["price"], "1.20");
    assert_eq!(credit["legs"].as_array().expect("legs").len(), 4);
}

#[test]
fn a_side_that_disagrees_with_the_signed_limit_refuses() {
    let mut bad = vertical();
    bad.side = OrderSide::Sell;
    assert!(matches!(
        order_wire_of(&bad),
        Err(TransportError::Terminal(_))
    ));
}

#[test]
fn venue_statuses_map_to_the_lifecycle() {
    for (wire, expected) in [
        ("Live", OrderStatus::Acknowledged),
        ("Filled", OrderStatus::Filled),
        ("Cancelled", OrderStatus::Cancelled),
        ("Rejected", OrderStatus::Rejected),
        ("Expired", OrderStatus::Expired),
        ("Received", OrderStatus::Submitted),
    ] {
        let wire = serde_json::json!({
            "data": {"order": {
                "status": wire,
                "client-order-id": "run-7-0#0",
                "price": "1.50",
                "price-effect": "Debit",
                "time-in-force": "Day",
                "instrument-type": "Equity Option",
                "legs": [
                    {"action": "Buy to Open", "symbol": "SPY   260116C00600000", "quantity": 2},
                    {"action": "Sell to Open", "symbol": "SPY   260116C00605000", "quantity": 2}
                ]
            }}
        });
        let record = order_record_of(&wire, vertical()).expect("record maps");
        assert_eq!(record.status, expected);
    }
}

#[test]
fn venue_error_envelopes_keep_their_retry_class() {
    let terminal = envelope_error(&serde_json::json!({
        "error": {"code": "bad_request", "message": "no"}
    }))
    .expect("terminal envelope");
    assert!(matches!(terminal, TransportError::Terminal(_)));
    assert!(!terminal.is_retryable());

    let transient = envelope_error(&serde_json::json!({
        "error": {"code": "rate_limited", "message": "slow"}
    }))
    .expect("transient envelope");
    assert!(matches!(transient, TransportError::Transient(_)));
    assert!(transient.is_retryable());
}

#[test]
fn occ_symbols_parse_back_to_contracts() {
    let expected = contract("600", OptionKind::Call);
    let symbol = expected.occ_symbol();
    assert_eq!(
        OptionContract::parse_occ_symbol(&symbol).expect("padded root parses"),
        expected
    );
    assert_eq!(
        OptionContract::parse_occ_symbol(symbol.trim()).expect("trimmed root parses"),
        expected
    );
    assert!(OptionContract::parse_occ_symbol("not-an-occ").is_err());
}

/// A newest-only stub over fetched quotes: the fetch carries whatever the
/// wire printed (crossed included); the stub prices newest by declaration.
struct FetchedChain {
    quotes: Vec<protopyne_types::ChainQuote>,
}

impl crate::venue::LegQuoteSource for FetchedChain {
    fn leg_quote(&self, contract: &protopyne_types::OptionContract) -> Option<crate::quote::Quote> {
        self.quotes
            .iter()
            .rfind(|quote| quote.contract == *contract)
            .map(|quote| {
                crate::quote::Quote::new()
                    .bid(quote.bid)
                    .ask(quote.ask)
                    .bid_size(u64::from(quote.bid_size))
                    .ask_size(u64::from(quote.ask_size))
                    .ts(quote.timestamp)
                    .build()
            })
    }

    fn newest_ok(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn chain_snapshot_carries_a_crossed_print_to_the_fill_policy() {
    use crate::order::OrderSide;
    use crate::paper::{PaperConfig, PaperVenue};
    use crate::venue::Venue;
    use protopyne_types::{LegSide, SpreadLeg, SpreadLegs};

    // The 690 leg prints crossed (bid above ask); the 695 leg is liquid.
    let ts = "2026-01-09T22:00:00Z";
    let wires = vec![
        chain_wire(ts, "690", "call", "4", "3"),
        chain_wire(ts, "695", "call", "2", "3"),
    ];
    let (base, count, _) = serve_chain_script(vec![snapshot_body(&wires)]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let quotes = CURRENT_CAPS
        .scope(
            net_caps(),
            transport.fetch_chain_snapshot(&net, "SPY", "tok-1"),
        )
        .await
        .expect("a crossed print must not refuse the fetch");
    assert_eq!(quotes.len(), 2, "the snapshot carries every entry");
    assert!(
        quotes[0].is_crossed(),
        "the crossed print arrives crossed, not repaired"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // The carried print reaches the per-leg fill policy, which refuses the
    // crossed leg with its own reason instead of the fetch refusing it.
    let legs = SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(quotes[0].contract.clone())
            .side(LegSide::Long)
            .ratio(1)
            .build(),
        SpreadLeg::new()
            .contract(quotes[1].contract.clone())
            .side(LegSide::Short)
            .ratio(1)
            .build(),
    ])
    .expect("vertical builds");
    let intent = OrderIntent::new()
        .client_id("x#0".to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(1)
        .limit_price(money("5.00"))
        .maybe_legs(Some(legs))
        .build();
    let mut venue = PaperVenue::new(PaperConfig::new().build());
    venue.set_leg_quotes(std::sync::Arc::new(FetchedChain { quotes }));
    let single = crate::quote::Quote::new()
        .bid(money("600.00"))
        .ask(money("600.10"))
        .bid_size(50)
        .ask_size(50)
        .ts(now())
        .build();
    let err = venue
        .submit(intent, &single, now())
        .await
        .expect_err("the crossed leg refuses at the fill policy");
    assert!(
        err.to_string().contains("crossed"),
        "the refusal names the crossed leg, got {err:?}"
    );
}
