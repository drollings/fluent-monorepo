//! Authenticated live-venue adapter: login, query-first order entry,
//! dry-run, cancel, and the production boot bundle — all against a stateful
//! loopback fake. Nothing here dials beyond `127.0.0.1`; the cert-sandbox
//! soak lives in `tests/live/` (`#[ignore]` + `live-ai`).

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use chrono::{TimeZone, Utc};
use fluent_concurrency::io::net::NetCapability;
use fluent_wvr::{CURRENT_CAPS, CapabilitySet};
use rust_decimal::Decimal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::journal::VenueStorage;
use crate::live::{LiveBoot, TastytradeVenue, boot_live, positions_of};
use crate::order::{OrderIntent, OrderRecord, OrderSide, OrderStatus};
use crate::session::SessionConfig;
use crate::tastytrade::{TastytradeConfig, TastytradeTransport};
use crate::venue::Venue;
use protopyne_types::Money;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn intent(id: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

fn record_for(intent: OrderIntent, status: OrderStatus) -> OrderRecord {
    OrderRecord::new()
        .intent(intent)
        .status(status)
        .filled_quantity(0)
        .maybe_avg_price(None)
        .fees_paid(Money::zero())
        .updated_at(now())
        .build()
}

/// One request the fake observed.
struct Seen {
    method: String,
    path: String,
    authorized: bool,
    body: String,
}

/// Stateful fake venue: sessions, dry-run, orders keyed by client ID, and
/// cancel — with per-route counters so tests prove call counts.
struct Fake {
    seen: std::sync::Mutex<Vec<Seen>>,
    orders: std::sync::Mutex<HashMap<String, OrderRecord>>,
    token: String,
    drop_next_order_post: AtomicUsize,
    posts_to_orders: AtomicUsize,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: std::sync::Mutex::new(Vec::new()),
            orders: std::sync::Mutex::new(HashMap::new()),
            token: "tok-live-1".to_owned(),
            drop_next_order_post: AtomicUsize::new(0),
            posts_to_orders: AtomicUsize::new(0),
        })
    }

    fn reply(&self, head: &str, body: &str) -> String {
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();
        let authorized = lines.any(|line| {
            line.to_ascii_lowercase() == format!("authorization: {}", self.token).to_lowercase()
        });
        self.seen.lock().expect("seen").push(Seen {
            method: method.clone(),
            path: path.clone(),
            authorized,
            body: body.to_owned(),
        });
        let ok = |payload: &str| http_response(payload);
        if path == "/sessions" {
            if body.contains("\"password\":\"wrong\"") {
                return http_response(r#"{"error":{"code":"unauthorized","message":"bad login"}}"#);
            }
            return ok(&format!(r#"{{"token":"{}"}}"#, self.token));
        }
        if !authorized {
            return ok(r#"{"error":{"code":"unauthorized","message":"no token"}}"#);
        }
        if path.ends_with("/orders/dry-run") {
            return ok(r#"{"allowed":true,"reserved_notional":"1201.00"}"#);
        }
        if method == "POST" && path.ends_with("/orders") {
            self.posts_to_orders.fetch_add(1, Ordering::SeqCst);
            let wire: serde_json::Value = serde_json::from_str(body).expect("intent json");
            let client_id = wire
                .get("client_id")
                .and_then(|v| v.as_str())
                .expect("client_id")
                .to_owned();
            let intent: OrderIntent = serde_json::from_str(body).expect("intent");
            let record = record_for(intent, OrderStatus::Acknowledged);
            self.orders.lock().expect("orders").insert(client_id, record.clone());
            // Ack loss is accept-then-silence: the order stands, only the
            // reply is dropped — so the retry must resolve by query.
            if self.drop_next_order_post.fetch_sub(1, Ordering::SeqCst) > 0 {
                return "drop".to_owned();
            }
            return ok(&serde_json::to_string(&serde_json::json!({ "order": record })).expect("json"));
        }
        if method == "GET" && path.contains("/orders/") {
            let id = decode_segment(path.rsplit('/').next().unwrap_or_default());
            if let Some(record) = self.orders.lock().expect("orders").get(id.as_str()).cloned() {
                return ok(&serde_json::to_string(&serde_json::json!({ "order": record })).expect("json"));
            }
            return ok(r#"{"error":{"code":"not_found","message":"unknown order"}}"#);
        }
        if method == "POST" && path.ends_with("/cancel") {
            let id = decode_segment(path.split('/').rev().nth(1).unwrap_or_default());
            let mut orders = self.orders.lock().expect("orders");
            match orders.get_mut(id.as_str()) {
                None => return ok(r#"{"error":{"code":"not_found","message":"unknown order"}}"#),
                Some(record) if !record.status.is_active() => {
                    return ok(r#"{"error":{"code":"terminal","message":"already terminal"}}"#);
                }
                Some(record) => {
                    record.status = OrderStatus::Cancelled;
                    record.updated_at = now();
                    let record = record.clone();
                    return ok(&serde_json::to_string(&serde_json::json!({ "order": record })).expect("json"));
                }
            }
        }
        ok(r#"{"error":{"code":"bad_request","message":"no route"}}"#)
    }

    fn paths(&self, method: &str, suffix: &str) -> usize {
        self.seen
            .lock()
            .expect("seen")
            .iter()
            .filter(|s| s.method == method && s.path.ends_with(suffix))
            .count()
    }
}

/// One-decoding of `%XX` path segments (the fake speaks raw TCP, so no
/// server framework decodes for it).
fn decode_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() + 1 {
            if let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    index += 3;
                    continue;
                }
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn http_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn serve(fake: Arc<Fake>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let fake = Arc::clone(&fake);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0_u8; 4096];
                let header_end = loop {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            head.extend_from_slice(&buf[..n]);
                            if let Some(at) = head
                                .windows(4)
                                .position(|w| w == b"\r\n\r\n")
                            {
                                break at + 4;
                            }
                        }
                    }
                };
                let head_text = String::from_utf8_lossy(&head[..header_end]).into_owned();
                if head_text.to_ascii_lowercase().contains("expect: 100-continue") {
                    let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await;
                }
                let content_len = head_text
                    .to_ascii_lowercase()
                    .split("content-length:")
                    .nth(1)
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                // The first read may already hold the whole body: keep what
                // arrived past the header boundary and read only the rest.
                let mut body = head[header_end..].to_vec();
                body.resize(content_len.max(body.len()), 0);
                let mut read = head[header_end..].len().min(content_len);
                while read < content_len {
                    match stream.read(&mut body[read..content_len]).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => read += n,
                    }
                }
                body.truncate(content_len);
                let reply = fake.reply(&head_text, &String::from_utf8_lossy(&body));
                if reply == "drop" {
                    return;
                }
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    base
}

fn net_caps() -> CapabilitySet {
    CapabilitySet::new().with(NetCapability::new())
}

async fn scoped<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    CURRENT_CAPS.scope(net_caps(), f).await
}

fn venue_for(base: &str) -> (Arc<TastytradeTransport>, Arc<NetCapability>, TastytradeVenue) {
    let transport = Arc::new(TastytradeTransport::new(
        TastytradeConfig::new().base_url(base.to_owned()).build(),
    ));
    let net = Arc::new(NetCapability::new());
    let venue = TastytradeVenue::new(Arc::clone(&transport), Arc::clone(&net), "ACC-1".to_owned())
        .expect("configured");
    (transport, net, venue)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_stores_token_and_bad_creds_stop_without_retry() {
    let fake = Fake::new();
    let base = serve(Arc::clone(&fake)).await;
    let (_transport, _net, venue) = venue_for(&base);
    scoped(async {
        assert!(!venue.is_authenticated());
        venue.login("user", "wrong").await.expect_err("bad creds refuse");
        assert!(!venue.is_authenticated(), "no token banked on refusal");
        assert_eq!(fake.paths("POST", "/sessions"), 1, "terminal: exactly one login attempt");
        venue.login("user", "right").await.expect("good creds");
        assert!(venue.is_authenticated());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthenticated_order_calls_refuse_without_dialing() {
    let fake = Fake::new();
    let base = serve(Arc::clone(&fake)).await;
    let (_transport, _net, venue) = venue_for(&base);
    scoped(async {
        let quote = crate::quote::Quote::new()
            .bid(money("600.00"))
            .ask(money("600.10"))
            .bid_size(50)
            .ask_size(50)
            .ts(now())
            .build();
        venue.dry_run(&intent("a#0"), &quote).expect_err("no token refuses");
        venue.submit(intent("a#0"), &quote, now()).await.expect_err("no token refuses");
        venue.cancel("a#0", now()).await.expect_err("no token refuses");
        assert!(venue.query("a#0").await.is_none(), "query fails closed");
        assert_eq!(fake.seen.lock().expect("seen").len(), 0, "nothing dialed");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_submit_and_query_round_trip() {
    let fake = Fake::new();
    let base = serve(Arc::clone(&fake)).await;
    let (_transport, _net, venue) = venue_for(&base);
    scoped(async {
        venue.login("user", "right").await.expect("login");
        let quote = crate::quote::Quote::new()
            .bid(money("600.00"))
            .ask(money("600.10"))
            .bid_size(50)
            .ask_size(50)
            .ts(now())
            .build();
        let dry = venue.dry_run(&intent("b#0"), &quote).expect("dry run");
        assert!(dry.allowed);
        assert_eq!(dry.reserved_notional, money("1201.00"));
        {
            let seen = fake.seen.lock().expect("seen");
            let dry_call = seen.iter().find(|s| s.path.ends_with("/dry-run")).expect("dry call");
            assert!(dry_call.authorized, "the token rides the header");
            assert!(dry_call.body.contains("\"client_id\":\"b#0\""), "the intent rides the body");
        }
        let record = venue.submit(intent("b#0"), &quote, now()).await.expect("submit");
        assert_eq!(record.status, OrderStatus::Acknowledged);
        let found = venue.query("b#0").await.expect("query finds");
        assert_eq!(found.intent.client_id, "b#0");
        assert!(venue.query("missing#0").await.is_none(), "unknown is None, not error");
        assert_eq!(venue.orders().len(), 1, "mirror carries the order");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resubmit_returns_the_record_with_no_second_post() {
    let fake = Fake::new();
    let base = serve(Arc::clone(&fake)).await;
    let (_transport, _net, venue) = venue_for(&base);
    scoped(async {
        venue.login("user", "right").await.expect("login");
        let quote = crate::quote::Quote::new()
            .bid(money("600.00"))
            .ask(money("600.10"))
            .bid_size(50)
            .ask_size(50)
            .ts(now())
            .build();
        venue.submit(intent("c#0"), &quote, now()).await.expect("first");
        let again = venue.submit(intent("c#0"), &quote, now()).await.expect("resubmit");
        assert_eq!(again.status, OrderStatus::Acknowledged);
        assert_eq!(fake.posts_to_orders.load(Ordering::SeqCst), 1, "one order POST total");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_ack_resolves_through_query_with_no_second_order() {
    let fake = Fake::new();
    let base = serve(Arc::clone(&fake)).await;
    let (_transport, _net, venue) = venue_for(&base);
    scoped(async {
        use crate::journal::RunIntentLog;
        use crate::submit::submit_guarded;
        use protopyne_strategy::{RiskApproved, RiskEngine, RiskLimits, RiskState};
        venue.login("user", "right").await.expect("login");
        fake.drop_next_order_post.store(1, Ordering::SeqCst);
        let quote = crate::quote::Quote::new()
            .bid(money("600.00"))
            .ask(money("600.10"))
            .bid_size(50)
            .ask_size(50)
            .ts(now())
            .build();
        let engine = RiskEngine::new(RiskLimits::default());
        let proposal = protopyne_strategy::Proposal::new()
            .id("prop-1".to_owned())
            .strategy("ribbon".to_owned())
            .underlying("SPY".to_owned())
            .direction(protopyne_strategy::Direction::Long)
            .side(protopyne_strategy::ProposalSide::Entry)
            .structure(protopyne_strategy::StructureKind::Vertical)
            .quantity(protopyne_types::Qty::new(Decimal::from(2)))
            .limit_price(money("600.50"))
            .max_loss(money("300"))
            .notional(money("1_000"))
            .dte_days(30)
            .conviction(0.8)
            .activity_score(0.7)
            .state(protopyne_strategy::ProposalState::Proposed)
            .created_at(now())
            .build();
        let state = RiskState {
            quote_age_secs: 5,
            clock_skew_secs: 1,
            reconciliation_healthy: true,
            is_limit_order: true,
            is_spread: true,
        };
        let approved: RiskApproved = engine.check(&proposal, &state).expect("healthy");
        let log = RunIntentLog::new();
        let record = submit_guarded(&log, &venue, &approved, intent("d#0"), &quote, now())
            .await
            .expect("ack loss still resolves");
        assert_eq!(record.intent.client_id, "d#0");
        assert_eq!(
            fake.posts_to_orders.load(Ordering::SeqCst),
            1,
            "the retry was a lookup, never a second order"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_unknown_and_terminal_refuse() {
    let fake = Fake::new();
    let base = serve(Arc::clone(&fake)).await;
    let (_transport, _net, venue) = venue_for(&base);
    scoped(async {
        venue.login("user", "right").await.expect("login");
        let quote = crate::quote::Quote::new()
            .bid(money("600.00"))
            .ask(money("600.10"))
            .bid_size(50)
            .ask_size(50)
            .ts(now())
            .build();
        venue.cancel("ghost#0", now()).await.expect_err("unknown refuses");
        venue.submit(intent("e#0"), &quote, now()).await.expect("submit");
        let cancelled = venue.cancel("e#0", now()).await.expect("cancel");
        assert_eq!(cancelled.status, OrderStatus::Cancelled);
        venue.cancel("e#0", now()).await.expect_err("terminal refuses");
    })
    .await;
}

#[test]
fn order_record_wire_refuses_without_panicking() {
    let reason = TastytradeVenue::parse_order_record("not json").expect_err("non-JSON refuses");
    assert!(reason.contains("JSON"), "got: {reason}");
    let reason = TastytradeVenue::parse_order_record(r#"{"order":{"nope":1}}"#)
        .expect_err("non-record refuses");
    assert!(reason.contains("order"), "got: {reason}");
    let record = TastytradeVenue::parse_order_record(
        &serde_json::to_string(&serde_json::json!({ "order": record_for(intent("f#0"), OrderStatus::Acknowledged) }))
            .expect("json"),
    )
    .expect("envelope parses");
    assert_eq!(record.intent.client_id, "f#0");
}

#[test]
fn positions_fold_books_into_underlying_totals() {
    let mut buy = intent("g#0");
    buy.quantity = 2;
    let mut buy_record = record_for(buy, OrderStatus::Filled);
    buy_record.filled_quantity = 2;
    buy_record.avg_price = Some(money("600.50"));
    let mut sell = intent("g#1");
    sell.side = OrderSide::Sell;
    sell.quantity = 1;
    let mut sell_record = record_for(sell, OrderStatus::Filled);
    sell_record.filled_quantity = 1;
    sell_record.avg_price = Some(money("601.00"));
    let positions = positions_of([buy_record, sell_record]);
    let position = positions.get("SPY").expect("SPY nets");
    assert_eq!(position.quantity, 1, "2 bought minus 1 sold");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_live_refuses_without_config_or_credentials() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal_path = dir.path().join("live.db");
    let boot = LiveBoot::new()
        .session(SessionConfig::default())
        .transport(TastytradeConfig::new().build())
        .account(String::new())
        .journal_path(journal_path)
        .build();
    let net = Arc::new(NetCapability::new());
    scoped(async {
        let refused = boot_live(&boot, Arc::clone(&net), "", "").await;
        assert!(refused.is_err(), "empty everything refuses");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_live_constructs_session_registry_and_journal() {
    let fake = Fake::new();
    let base = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let journal_path = dir.path().join("live.db");
    let boot = LiveBoot::new()
        .session(SessionConfig::default())
        .transport(TastytradeConfig::new().base_url(base.clone()).build())
        .account("ACC-1".to_owned())
        .journal_path(journal_path.clone())
        .build();
    let net = Arc::new(NetCapability::new());
    scoped(async {
        let live = boot_live(&boot, Arc::clone(&net), "user", "right")
            .await
            .expect("boot");
        assert!(live.venue.is_authenticated(), "logged in during boot");
        assert!(journal_path.exists(), "durable journal file exists");
        let quote = crate::quote::Quote::new()
            .bid(money("600.00"))
            .ask(money("600.10"))
            .bid_size(50)
            .ask_size(50)
            .ts(now())
            .build();
        let record = live
            .venue
            .submit(intent("h#0"), &quote, now())
            .await
            .expect("venue serves through the bundle");
        assert_eq!(record.status, OrderStatus::Acknowledged);
        let replayed: crate::journal::ReplayedState = CURRENT_CAPS
            .scope(
                CapabilitySet::new().with(VenueStorage::new(journal_path.clone())),
                async {
                    live.journal
                        .replay(&VenueStorage::new(journal_path))
                        .expect("replay")
                },
            )
            .await;
        let _ = replayed;
        live.session.shutdown().await;
    })
    .await;
}
