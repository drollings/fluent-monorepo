//! Chain history ingest: CSV shape, fail-closed rows, point-in-time reads.
//!
//! Every assertion is headless: parse the documented shape, refuse the
//! documented corruptions with line numbers, and prove a bar never sees a
//! quote stamped after it.

use chrono::{TimeZone, Utc};
use fluent_wvr::validate::Validate;
use rust_decimal::Decimal;

use protopyne_types::{Money, OptionContract, OptionKind};
use protopyne_venue::LegQuoteSource;

use crate::chain_history::{
    load_chain_history, parse_chain_csv, ChainFeedConfig, HistoricalChainSource,
};

fn contract() -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid expiry"))
        .strike(Money::from(600))
        .kind(OptionKind::Call)
        .build()
}

fn row(ts: i64, bid: i64, ask: i64) -> String {
    format!("{ts},SPY,2026-07-17,600,call,{bid},{ask}")
}

fn headed(rows: &[String]) -> String {
    let mut content = "ts,underlying,expiry,strike,kind,bid,ask\n".to_owned();
    for row in rows {
        content.push_str(row);
        content.push('\n');
    }
    content
}

#[test]
fn minimal_rows_parse_with_sizes_defaulted() {
    let quotes = parse_chain_csv(&headed(&[
        row(1_750_000_000, 3, 4),
        row(1_750_000_900, 3, 5),
    ]))
    .expect("minimal rows parse");
    assert_eq!(quotes.len(), 2, "one quote per row");
    assert_eq!(quotes[0].bid, Money::from(3));
    assert_eq!(quotes[0].ask, Money::from(4));
    assert_eq!(quotes[0].bid_size, 0, "absent sizes default to zero");
    assert_eq!(quotes[0].open_interest, 0);
    assert!(quotes[0].iv.is_none(), "absent IV reads as none");
    assert!(quotes[0].greeks.is_none(), "absent greeks read as none");
    assert_eq!(quotes[0].contract, contract());
}

#[test]
fn full_rows_carry_sizes_interest_iv_and_greeks() {
    let content = "ts,underlying,expiry,strike,kind,bid,ask,bid_size,ask_size,open_interest,iv,delta,gamma,theta,vega\n\
         1750000000,SPY,2026-07-17,600,call,3,4,10,12,500,0.25,0.6,0.05,-0.02,0.1\n";
    let quotes = parse_chain_csv(content).expect("full rows parse");
    assert_eq!(quotes.len(), 1);
    let quote = &quotes[0];
    assert_eq!(
        (quote.bid_size, quote.ask_size, quote.open_interest),
        (10, 12, 500)
    );
    assert_eq!(quote.iv, Some(0.25));
    let greeks = quote.greeks.expect("greeks ride along");
    assert_eq!(
        (greeks.delta, greeks.gamma, greeks.theta, greeks.vega),
        (0.6, 0.05, -0.02, 0.1)
    );
}

#[test]
fn corruptions_refuse_with_line_numbers() {
    // Missing column.
    assert!(
        parse_chain_csv(
            "ts,underlying,expiry,strike,kind,bid\n1750000000,SPY,2026-07-17,600,call,3\n"
        )
        .is_err(),
        "a missing ask column refuses"
    );
    // Bad strike on the first data row (line 2).
    let bad_strike = headed(&["1750000000,SPY,2026-07-17,lotsofmoney,call,3,4".to_owned()]);
    let Err(crate::error::ServerError::BadRow { line, .. }) = parse_chain_csv(&bad_strike) else {
        panic!("a bad strike refuses as BadRow");
    };
    assert_eq!(line, 2);
    // Bad kind on the second data row (line 3).
    let bad_kind = headed(&[
        row(1_750_000_000, 3, 4),
        "1750000900,SPY,2026-07-17,600,sideways,3,4".to_owned(),
    ]);
    let Err(crate::error::ServerError::BadRow { line, .. }) = parse_chain_csv(&bad_kind) else {
        panic!("a bad kind refuses as BadRow");
    };
    assert_eq!(line, 3);
    // Bad expiry shape.
    let bad_expiry = headed(&["1750000000,SPY,07-17-2026,600,call,3,4".to_owned()]);
    assert!(
        parse_chain_csv(&bad_expiry).is_err(),
        "a non-YYYY-MM-DD expiry refuses"
    );
    // Negative bid: the vocabulary refuses what it would never quote.
    let negative_bid = headed(&["1750000000,SPY,2026-07-17,600,call,-1,4".to_owned()]);
    assert!(
        parse_chain_csv(&negative_bid).is_err(),
        "a negative bid refuses through the type's own validator"
    );
}

#[test]
fn point_in_time_reads_hold_no_lookahead() {
    let quotes = parse_chain_csv(&headed(&[
        row(1_000, 3, 4),
        row(2_000, 5, 6),
        row(3_000, 7, 8),
    ]))
    .expect("three bars of chain parse");
    let source = HistoricalChainSource::new(quotes);
    let at = |secs: i64| Utc.timestamp_opt(secs, 0).unwrap();
    // Before the first quote: nothing held.
    assert!(
        source.leg_quote_at(&contract(), at(999)).is_none(),
        "a bar before any quote reads no chain"
    );
    // At-or-before: the newest held quote, never a later one.
    let first = source
        .leg_quote_at(&contract(), at(1_500))
        .expect("first window reads");
    assert_eq!((first.bid, first.ask), (Money::from(3), Money::from(4)));
    let second = source
        .leg_quote_at(&contract(), at(2_000))
        .expect("exact stamp reads");
    assert_eq!((second.bid, second.ask), (Money::from(5), Money::from(6)));
    let third = source
        .leg_quote_at(&contract(), at(9_999))
        .expect("tail reads newest");
    assert_eq!((third.bid, third.ask), (Money::from(7), Money::from(8)));
    // The timeless read answers newest, like every live source.
    let newest = source.leg_quote(&contract()).expect("newest reads");
    assert_eq!((newest.bid, newest.ask), (Money::from(7), Money::from(8)));
    // Unknown contracts read as absent, never as another contract's quote.
    let mut other = contract();
    other.kind = OptionKind::Put;
    assert!(
        source.leg_quote_at(&other, at(9_999)).is_none(),
        "a put never reads a call's quote"
    );
}

#[test]
fn window_narrows_to_the_replay_range() {
    let quotes = parse_chain_csv(&headed(&[
        row(1_000, 3, 4),
        row(2_000, 5, 6),
        row(3_000, 7, 8),
    ]))
    .expect("three bars of chain parse");
    let source = HistoricalChainSource::new(quotes);
    let at = |secs: i64| Utc.timestamp_opt(secs, 0).unwrap();
    let narrowed = source.window(at(1_500), at(2_500));
    assert_eq!(
        narrowed.quotes().len(),
        1,
        "only the in-window quote survives"
    );
    assert!(
        narrowed.leg_quote_at(&contract(), at(9_999)).is_some(),
        "the windowed quote still serves"
    );
    assert!(
        narrowed.leg_quote_at(&contract(), at(1_400)).is_none(),
        "narrowing drops out-of-window history too"
    );
}

#[test]
fn config_defaults_absent_and_validates_fail_closed() {
    let config = ChainFeedConfig::default();
    assert!(config.chain_data_path.is_none(), "chains default to absent");
    assert!(config.credentials_path.is_none());
    assert!(config.validate().is_ok(), "absence validates");
    let blank = ChainFeedConfig::new()
        .maybe_chain_data_path(Some("  ".to_owned()))
        .build();
    assert!(blank.validate().is_err(), "a blank chain path refuses");
    let ghost = ChainFeedConfig::new()
        .maybe_credentials_path(Some("/nonexistent/tasty-creds.json".to_owned()))
        .build();
    assert!(
        ghost.validate().is_err(),
        "a named-but-missing credentials file refuses at load time"
    );
    // Money precision: fractional strikes survive the CSV round trip.
    let fractional = headed(&["1750000000,SPY,2026-07-17,600.50,call,3,4".to_owned()]);
    let quotes = parse_chain_csv(&fractional).expect("fractional strikes parse");
    assert_eq!(
        quotes[0].contract.strike,
        Money::new(Decimal::new(60050, 2)),
        "strikes keep exact decimal precision"
    );
}

#[test]
fn missing_chain_file_refuses_without_reading() {
    let missing = std::path::Path::new("/nonexistent/chain-history.csv");
    assert!(
        load_chain_history(missing).is_err(),
        "a missing chain file refuses instead of reading empty"
    );
}

/// Fetch milestone: credentials file → loopback chain fetch → the landed CSV
/// shape. Secrets never reach an error string; partial windows never reach
/// the filesystem.
use fluent_concurrency::io::net::NetCapability;
use fluent_wvr::{CapabilitySet, CURRENT_CAPS};
use protopyne_venue::{TastytradeConfig, TastytradeTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn net_caps() -> CapabilitySet {
    CapabilitySet::new().with(NetCapability::new())
}

fn write_creds(dir: &tempfile::TempDir, token: &str) -> std::path::PathBuf {
    let path = dir.path().join("tasty-creds.json");
    std::fs::write(&path, format!(r#"{{"token":"{token}"}}"#)).expect("creds write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    path
}

fn chain_wire(ts: &str, strike: &str, bid: &str, ask: &str) -> String {
    format!(
        r#"{{"underlying":"SPY","expiry":"2026-07-17","strike":"{strike}","kind":"call","bid":"{bid}","ask":"{ask}","bid_size":50,"ask_size":50,"open_interest":500,"iv":0.25,"ts":"{ts}"}}"#
    )
}

/// Loopback chain endpoint serving one canned snapshot body per connection;
/// returns the base URL plus the served-request counter.
async fn serve_chain(
    script: Vec<String>,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let bodies = std::sync::Arc::new(std::sync::Mutex::new(script));
    let counted = std::sync::Arc::clone(&count);
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
            let body = bodies
                .lock()
                .ok()
                .and_then(|mut guard| guard.pop())
                .unwrap_or_else(|| {
                    r#"{"error":{"code":"unavailable","message":"scripted"}}"#.to_owned()
                });
            counted.fetch_add(1, Ordering::SeqCst);
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    (base, count)
}

fn transport_for(base: &str) -> TastytradeTransport {
    TastytradeTransport::new(TastytradeConfig::new().base_url(base.to_owned()).build())
}

#[test]
fn fetch_token_reads_the_opaque_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_creds(&dir, "tok-1");
    let token = crate::chain_history::read_fetch_token(path.to_str().expect("utf8"))
        .expect("owner-only creds read");
    assert_eq!(token, "tok-1");
}

#[test]
fn fetch_token_refuses_group_readable_without_echoing_the_secret() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_creds(&dir, "tok-super-secret");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let reason = crate::chain_history::read_fetch_token(path.to_str().expect("utf8"))
            .expect_err("group-readable creds refuse");
        assert!(
            !reason.to_string().contains("tok-super-secret"),
            "the secret never reaches the error, got: {reason}"
        );
    }
}

#[test]
fn fetch_token_missing_file_refuses() {
    assert!(
        crate::chain_history::read_fetch_token("/nonexistent/tasty-creds.json").is_err(),
        "missing creds refuse"
    );
}

#[test]
fn fetched_quotes_emit_the_landed_csv_shape_byte_exact() {
    let quotes = parse_chain_csv(
        "ts,underlying,expiry,strike,kind,bid,ask,bid_size,ask_size,open_interest,iv\n\
         1767996000,SPY,2026-07-17,690,call,3,4,50,50,500,0.25\n\
         1767996000,SPY,2026-07-17,695,call,2,3,50,50,600,0.26\n",
    )
    .expect("vendored rows parse");
    let csv = crate::chain_history::chain_quotes_to_csv(&quotes);
    assert_eq!(
        csv,
        "ts,underlying,expiry,strike,kind,bid,ask,bid_size,ask_size,open_interest,iv,delta,gamma,theta,vega\n\
         1767996000,SPY,2026-07-17,690,call,3,4,50,50,500,0.25,,,,\n\
         1767996000,SPY,2026-07-17,695,call,2,3,50,50,600,0.26,,,,\n",
        "fetch output and hand-written fixtures share one shape"
    );
    let back = parse_chain_csv(&csv).expect("emitted CSV re-parses");
    assert_eq!(back, quotes, "the shape round-trips without loss");
}

#[tokio::test]
async fn fetch_without_credentials_refuses_before_any_dial() {
    let (base, count) = serve_chain(vec![r#"{"quotes":[]}"#.to_owned()]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let out = tempfile::tempdir()
        .expect("tempdir")
        .path()
        .join("chain.csv");
    let outcome = CURRENT_CAPS
        .scope(
            net_caps(),
            crate::chain_history::fetch_and_write_chain_history(
                &net,
                &transport,
                "/nonexistent/tasty-creds.json",
                "SPY",
                &out,
            ),
        )
        .await;
    assert!(outcome.is_err(), "no creds means no fetch");
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing dialed"
    );
    assert!(!out.exists(), "a refused fetch writes no file");
}

#[tokio::test]
async fn fetch_partial_window_refuses_and_writes_no_file() {
    let (base, _) = serve_chain(vec![r#"{"quotes":[]}"#.to_owned()]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_creds(&dir, "tok-1");
    let out = dir.path().join("chain.csv");
    let outcome = CURRENT_CAPS
        .scope(
            net_caps(),
            crate::chain_history::fetch_and_write_chain_history(
                &net,
                &transport,
                creds.to_str().expect("utf8"),
                "SPY",
                &out,
            ),
        )
        .await;
    assert!(outcome.is_err(), "an empty snapshot refuses");
    assert!(!out.exists(), "a partial window never lands half a file");
}

#[tokio::test]
async fn fetch_snapshot_lands_csv_the_reader_accepts() {
    let ts = "2026-01-09T22:00:00Z";
    let body = format!(
        r#"{{"quotes":[{},{}]}}"#,
        chain_wire(ts, "690", "3", "4"),
        chain_wire(ts, "695", "2", "3"),
    );
    let (base, _) = serve_chain(vec![body]).await;
    let transport = transport_for(&base);
    let net = NetCapability::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_creds(&dir, "tok-1");
    let out = dir.path().join("chain.csv");
    let fetched = CURRENT_CAPS
        .scope(
            net_caps(),
            crate::chain_history::fetch_and_write_chain_history(
                &net,
                &transport,
                creds.to_str().expect("utf8"),
                "SPY",
                &out,
            ),
        )
        .await
        .expect("loopback fetch lands");
    assert_eq!(fetched, 2, "one row per wire entry");
    let back = load_chain_history(&out).expect("the landed file reads");
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].contract.strike, Money::from(690));
}

#[test]
fn greeks_parse_as_a_unit_never_a_partial_set() {
    // A header naming only three of the four greek columns carries no greeks
    // at all: a missing column is absence, never a silently dropped set.
    let content = "ts,underlying,expiry,strike,kind,bid,ask,delta,gamma,theta\n\
         1750000000,SPY,2026-07-17,600,call,3,4,0.6,0.05,-0.02\n";
    let quotes = parse_chain_csv(content).expect("partial greek headers parse");
    assert_eq!(quotes.len(), 1);
    assert!(
        quotes[0].greeks.is_none(),
        "three of four greeks is no greeks, not a partial set"
    );
}

#[test]
fn a_blank_greek_cell_refuses_instead_of_inventing_zero() {
    // All four columns present, one cell blank: the row refuses as `BadRow`
    // on its own line rather than inventing 0.0 for the missing greek.
    let content = "ts,underlying,expiry,strike,kind,bid,ask,delta,gamma,theta,vega\n\
         1750000000,SPY,2026-07-17,600,call,3,4,0.6,,-0.02,0.1\n";
    let Err(crate::error::ServerError::BadRow { line, reason }) = parse_chain_csv(content) else {
        panic!("a blank greek cell refuses as BadRow");
    };
    assert_eq!(line, 2, "the refusal names the row, got {reason}");
}

#[test]
fn a_malformed_greek_cell_refuses() {
    let content = "ts,underlying,expiry,strike,kind,bid,ask,delta,gamma,theta,vega\n\
         1750000000,SPY,2026-07-17,600,call,3,4,0.6,notanumber,-0.02,0.1\n";
    let Err(crate::error::ServerError::BadRow { line, .. }) = parse_chain_csv(content) else {
        panic!("a malformed greek cell refuses as BadRow");
    };
    assert_eq!(line, 2);
}

#[test]
fn greeks_survive_the_landed_csv_shape() {
    use crate::chain_history::chain_quotes_to_csv;

    // A quote carrying greeks round-trips through the landed shape with the
    // set intact; a quote without greeks round-trips as absent.
    let with = crate::chain_history::parse_chain_csv(
        "ts,underlying,expiry,strike,kind,bid,ask,delta,gamma,theta,vega\n\
         1750000000,SPY,2026-07-17,600,call,3,4,0.6,0.05,-0.02,0.1\n",
    )
    .expect("greek rows parse")[0]
        .clone();
    let without = parse_chain_csv(&headed(&[row(1_750_000_900, 3, 5)]))
        .expect("minimal rows parse")[0]
        .clone();
    let back = parse_chain_csv(&chain_quotes_to_csv(&[with.clone(), without]))
        .expect("landed shape parses");
    assert_eq!(back.len(), 2);
    assert_eq!(
        back[0].greeks, with.greeks,
        "a present set is never dropped"
    );
    assert!(back[1].greeks.is_none(), "absence round-trips as absence");
}

#[test]
fn covers_agrees_with_the_snapshot_on_a_fixture() {
    // The cheap coverage predicate answers the screen's question without
    // cloning the window: covered, uncovered, and wrong-underlying instants
    // agree with `!snapshot.is_empty()` on the same source.
    let quotes = parse_chain_csv(&headed(&[
        row(1_750_000_000, 3, 4),
        row(1_750_000_900, 3, 5),
    ]))
    .expect("fixture parses");
    let source = HistoricalChainSource::new(quotes);
    let at = |secs: i64| Utc.timestamp_opt(secs, 0).unwrap();
    for (instant, underlying, covered) in [
        (at(1_750_000_000), "SPY", true),
        (at(1_750_000_900), "SPY", true),
        (at(9_999), "SPY", false),
        (at(1_750_000_900), "QQQ", false),
    ] {
        assert_eq!(
            source.covers(underlying, instant),
            covered,
            "covers names the fixture's coverage at {instant} for {underlying}"
        );
        assert_eq!(
            source.covers(underlying, instant),
            !source.chain_snapshot(underlying, instant).is_empty(),
            "the predicate and the snapshot never disagree"
        );
    }
}
