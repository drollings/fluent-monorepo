//! Tastytrade transaction/balance transform fixtures: the neutral deal and
//! summary shapes over recorded wire envelopes, plus malformed refusal.
//!
//! The network edge (`get`, envelope-error mapping) is pinned in the
//! transport suite; this pins what the transforms do with a decoded body.

use super::*;
use super::{deals_from_transactions, summary_from_balances};

use chrono::TimeZone as _;
use protopyne_types::Money;

use crate::account::{AccountError, AccountRange};
use crate::order::OrderSide;

fn stamp(hour: u32, min: u32) -> chrono::DateTime<Utc> {
    chrono::Utc
        .with_ymd_and_hms(2026, 1, 5, hour, min, 0)
        .single()
        .expect("valid stamp")
}

fn buy_txn(id: &str, at: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "action": "Buy_To_Open",
        "symbol": "SPY  260616C00650000",
        "underlying-symbol": "SPY",
        "price": "2.50",
        "quantity": "3",
        "commission": "1.05",
        "executed-at": at,
        "client-order-id": "manual-1",
        "order-id": "ord-9",
    })
}

fn sell_txn(id: &str, at: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "action": "Sell_To_Close",
        "symbol": "SPY  260616C00650000",
        "underlying-symbol": "SPY",
        "price": "3.10",
        "quantity": 3,
        "commission": 1.05,
        "executed-at": at,
    })
}

/// The deals transform maps recorded transactions to neutral deals: sides
/// fold case-insensitively, quantities read absolute, string and numeric
/// prices both decode, ids and counterparties carry over, and the window
/// applies.
#[test]
fn deals_transform_maps_a_recorded_envelope() {
    let wire = serde_json::json!({
        "data": {
            "items": [
                buy_txn("t-1", "2026-01-05T14:00:00Z"),
                sell_txn("t-2", "2026-01-05T15:30:00Z"),
            ]
        }
    });
    let range = AccountRange {
        start: Some(stamp(13, 0)),
        end: Some(stamp(16, 0)),
        limit: None,
    };
    let deals = deals_from_transactions(&wire, "ACC-1", range).expect("deals decode");
    assert_eq!(deals.len(), 2, "both windowed transactions land");
    let buy = &deals[0];
    assert_eq!(buy.deal_id, "t-1");
    assert_eq!(buy.side, OrderSide::Buy);
    assert_eq!(buy.fill.quantity, 3);
    assert_eq!(buy.fill.price, Money::new("2.50".parse().unwrap()));
    assert_eq!(buy.fill.fee, Money::new("1.05".parse().unwrap()));
    assert_eq!(buy.underlying, "SPY");
    assert_eq!(buy.client_id.as_deref(), Some("manual-1"));
    assert_eq!(buy.order_id.as_deref(), Some("ord-9"));
    assert_eq!(buy.executed_at, stamp(14, 0));
    let sell = &deals[1];
    assert_eq!(sell.side, OrderSide::Sell);
    assert_eq!(sell.fill.price, Money::new("3.10".parse().unwrap()));
    assert_eq!(sell.executed_at, stamp(15, 30));

    // The bare-items envelope reads identically; the window excludes.
    let bare = serde_json::json!({ "items": [buy_txn("t-1", "2026-01-05T14:00:00Z")] });
    let out = deals_from_transactions(
        &bare,
        "ACC-1",
        AccountRange {
            end: Some(stamp(13, 0)),
            ..AccountRange::all()
        },
    )
    .expect("bare envelope decodes");
    assert!(out.is_empty(), "the window excludes the early deal");

    // The limit truncates the oldest-first rows.
    let wire = serde_json::json!({ "items": [
        buy_txn("t-1", "2026-01-05T14:00:00Z"),
        sell_txn("t-2", "2026-01-05T15:30:00Z"),
    ] });
    let capped = deals_from_transactions(
        &wire,
        "ACC-1",
        AccountRange {
            limit: Some(1),
            ..AccountRange::all()
        },
    )
    .expect("limit applies");
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].deal_id, "t-1");
}

/// A transaction without a price is not a deal and never fails the pass;
/// one without any stamp reads its observed instant.
#[test]
fn deals_transform_skips_priceless_rows_and_defaults_unstamped_ones() {
    let wire = serde_json::json!({ "items": [
        { "id": "t-free", "action": "Buy_To_Open", "quantity": 1 },
        buy_txn("t-1", "2026-01-05T14:00:00Z"),
    ] });
    let deals = deals_from_transactions(&wire, "ACC-1", AccountRange::all()).expect("decode");
    assert_eq!(deals.len(), 1, "the priceless row is skipped");
    assert_eq!(deals[0].deal_id, "t-1");

    let wire = serde_json::json!({ "items": [
        { "action": "sell", "price": "1.00", "quantity": 1 },
    ] });
    let deals = deals_from_transactions(&wire, "ACC-1", AccountRange::all()).expect("decode");
    assert_eq!(deals.len(), 1);
    assert_eq!(
        deals[0].executed_at, deals[0].observed_at,
        "an unstamped deal reads its observed instant"
    );

    // Missing ids synthesize deterministically from the account and row.
    assert_eq!(deals[0].deal_id, "ACC-1-0");
}

/// A wire with no items is malformed, never an empty success masquerading
/// as "no deals".
#[test]
fn deals_transform_without_items_is_malformed() {
    for wire in [
        serde_json::json!({}),
        serde_json::json!({"data": {}}),
        serde_json::json!({"data": {"items": {}}}),
    ] {
        assert!(
            matches!(
                deals_from_transactions(&wire, "ACC-1", AccountRange::all()),
                Err(AccountError::Malformed(_))
            ),
            "no items refuses: {wire}"
        );
    }
    let empty = deals_from_transactions(
        &serde_json::json!({"items": []}),
        "ACC-1",
        AccountRange::all(),
    )
    .expect("an empty book decodes");
    assert!(
        empty.is_empty(),
        "an empty book is an empty vec, not a refusal"
    );
}

/// The balance transform reads the matching currency line, prefers the
/// hyphenated field spellings, and falls back line-first when the currency
/// is absent.
#[test]
fn summary_transform_reads_the_matching_currency_line() {
    let wire = serde_json::json!({ "data": { "items": [
        {
            "currency": "EUR",
            "net-liquid-balance": "10.00",
            "derivative-buying-power": "5.00",
            "maintenance-requirement": "1.00",
        },
        {
            "currency": "USD",
            "net-liquid-balance": "100.00",
            "derivative-buying-power": "50.00",
            "maintenance-requirement": "7.50",
        },
    ] } });
    let (net, buying, margin) = summary_from_balances(&wire, "USD").expect("balances decode");
    assert_eq!(net, Money::new("100.00".parse().unwrap()));
    assert_eq!(buying, Money::new("50.00".parse().unwrap()));
    assert_eq!(margin, Money::new("7.50".parse().unwrap()));

    // Legacy spellings decode when the hyphenated ones are absent.
    let legacy = serde_json::json!({ "items": [
        { "currency": "USD", "net-liquid": "40.00", "buying-power": "20.00" },
    ] });
    let (net, buying, margin) = summary_from_balances(&legacy, "USD").expect("legacy decodes");
    assert_eq!(net, Money::new("40.00".parse().unwrap()));
    assert_eq!(buying, Money::new("20.00".parse().unwrap()));
    assert_eq!(margin, Money::zero(), "no margin line reads zero");

    // No currency match falls back to the first line.
    let (net, _, _) = summary_from_balances(&legacy, "JPY").expect("fallback decodes");
    assert_eq!(net, Money::new("40.00".parse().unwrap()));
}

/// Balance wires with no usable lines are malformed, never zero balances
/// masquerading as an empty account.
#[test]
fn summary_transform_without_lines_is_malformed() {
    for wire in [
        serde_json::json!({}),
        serde_json::json!({"data": {}}),
        serde_json::json!({"data": {"items": []}}),
    ] {
        assert!(
            matches!(
                summary_from_balances(&wire, "USD"),
                Err(AccountError::Malformed(_))
            ),
            "no lines refuses: {wire}"
        );
    }
}
