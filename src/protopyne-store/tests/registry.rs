//! The file-backed registry contract: round-trip, refusals, crash safety,
//! and the durable JSON shape.
//!
//! Registration refuses the same four ways as the SQL catalog — blank
//! symbol, bad scale, duplicate `(symbol, venue)`, full table — each with
//! its own reason. A crash before the rename leaves the old file intact;
//! a crash after it leaves the new file intact. The `catalog.json` shape
//! is pinned so an operator can read it.

use super::*;
use crate::error::PlanError;
use crate::registry::test_setup::{register_defaults, with_registry_caps, VENUE};

#[tokio::test]
async fn register_then_resolve_round_trip() {
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let id = register_defaults(&registry, "SPY", VENUE).expect("register");
        assert_eq!(registry.resolve("SPY", VENUE).expect("resolve"), Some(id));
        assert_eq!(
            registry
                .tape_of("SPY", VENUE)
                .map(|(_, timeframe)| timeframe),
            Some(protopyne_types::Timeframe::M1),
            "the planner's lookup carries the spacing"
        );
        let markets = registry.markets().expect("markets");
        assert_eq!(markets.len(), 1);
        assert_eq!(markets[0].symbol_id, id);
        assert_eq!(markets[0].symbol, "SPY");
        assert_eq!(markets[0].venue, VENUE);
        assert_eq!(markets[0].asset_class, AssetClass::Stock);
        assert_eq!(
            markets[0].timeframe,
            protopyne_types::Timeframe::M1,
            "the registered spacing reads back"
        );
    })
    .await;
}

#[tokio::test]
async fn same_symbol_on_two_venues_registers_twice() {
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let first = register_defaults(&registry, "SPY", "venue-a").expect("first venue");
        let second = register_defaults(&registry, "SPY", "venue-b").expect("second venue");
        assert_ne!(
            first, second,
            "identity is per (symbol, venue), not per spelling"
        );
        assert_eq!(
            registry.resolve("SPY", "venue-a").expect("resolve a"),
            Some(first)
        );
        assert_eq!(
            registry.resolve("SPY", "venue-b").expect("resolve b"),
            Some(second)
        );
    })
    .await;
}

#[tokio::test]
async fn duplicate_symbol_venue_refuses() {
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let id = register_defaults(&registry, "SPY", VENUE).expect("first");
        let err = register_defaults(&registry, "SPY", VENUE).expect_err("duplicate refuses");
        assert!(
            matches!(err, StoreError::Duplicate(_)),
            "a duplicate is its own refusal: {err:?}"
        );
        assert_eq!(
            registry.resolve("SPY", VENUE).expect("resolve"),
            Some(id),
            "the refused registration changes nothing"
        );
    })
    .await;
}

#[tokio::test]
async fn blank_symbol_refuses() {
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let err = register_defaults(&registry, "", VENUE).expect_err("blank refuses");
        assert!(
            matches!(err, StoreError::Plan(PlanError::BlankSymbol)),
            "a blank symbol is a plan refusal: {err:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn resolve_unknown_is_none_not_error() {
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        assert_eq!(registry.resolve("NOPE", VENUE).expect("resolve"), None);
    })
    .await;
}

#[tokio::test]
async fn full_table_refuses_rather_than_evicting() {
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory_with_max(2).expect("bounded registry opens");
        register_defaults(&registry, "SPY", VENUE).expect("first");
        register_defaults(&registry, "QQQ", VENUE).expect("second");
        let err = register_defaults(&registry, "IWM", VENUE).expect_err("full refuses");
        assert!(
            matches!(err, StoreError::CatalogFull(_)),
            "a full table refuses with its own reason: {err:?}"
        );
        assert!(
            registry.resolve("SPY", VENUE).expect("resolve").is_some(),
            "refusing never evicts"
        );
    })
    .await;
}

#[tokio::test]
async fn registration_outside_the_capability_is_denied() {
    let registry = MarketRegistry::open_in_memory().expect("registry opens");
    let err = register_defaults(&registry, "SPY", VENUE).expect_err("no capability");
    assert!(
        matches!(err, StoreError::Denied(_)),
        "an ungranted task is denied, not defaulted: {err:?}"
    );
}

#[tokio::test]
async fn file_round_trip_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let id = with_registry_caps(async {
        let registry = MarketRegistry::open(&root).expect("registry opens");
        let id = register_defaults(&registry, "SPY", VENUE).expect("register");
        register_defaults(&registry, "QQQ", VENUE).expect("second registers");
        id
    })
    .await;
    assert!(
        root.join("catalog.json").is_file(),
        "the catalog persists as JSON"
    );
    with_registry_caps(async {
        let reopened = MarketRegistry::open(&root).expect("registry reopens");
        assert_eq!(reopened.resolve("SPY", VENUE).expect("resolve"), Some(id));
        assert!(
            reopened.resolve("QQQ", VENUE).expect("resolve").is_some(),
            "both markets survive the reopen"
        );
        let next = register_defaults(&reopened, "IWM", VENUE).expect("third registers");
        assert!(
            next.0 > id.0,
            "ids keep allocating forward across the reopen: {next:?} after {id:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_stray_temp_sibling_never_becomes_the_catalog() {
    // Crash before the rename: a leftover temp file sits beside an intact
    // catalog. Reopening must read the intact catalog and ignore the stray.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    with_registry_caps(async {
        let registry = MarketRegistry::open(&root).expect("registry opens");
        register_defaults(&registry, "SPY", VENUE).expect("register");
    })
    .await;
    let catalog_path = root.join("catalog.json");
    let before = std::fs::read_to_string(&catalog_path).expect("catalog reads");
    std::fs::write(root.join("catalog.json.123.0.456.tmp"), b"{corrupt!!!")
        .expect("stray temp lands");
    with_registry_caps(async {
        let reopened = MarketRegistry::open(&root).expect("registry reopens past the stray");
        assert!(
            reopened.resolve("SPY", VENUE).expect("resolve").is_some(),
            "the intact catalog still resolves"
        );
    })
    .await;
    let after = std::fs::read_to_string(&catalog_path).expect("catalog re-reads");
    assert_eq!(after, before, "the stray never touched the catalog");
}

#[tokio::test]
async fn a_replaced_catalog_reads_the_new_file_whole() {
    // Crash after the rename: the new file is fully in place. Reopening
    // must see every market the new file names.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    with_registry_caps(async {
        let registry = MarketRegistry::open(&root).expect("registry opens");
        register_defaults(&registry, "SPY", VENUE).expect("first");
        register_defaults(&registry, "QQQ", VENUE).expect("second");
    })
    .await;
    with_registry_caps(async {
        let reopened = MarketRegistry::open(&root).expect("registry reopens");
        let markets = reopened.markets().expect("markets");
        assert_eq!(markets.len(), 2, "the replaced file reads whole");
    })
    .await;
}

#[tokio::test]
async fn a_corrupt_catalog_refuses_loudly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    std::fs::create_dir_all(&root).expect("root exists");
    std::fs::write(root.join("catalog.json"), b"{not valid json").expect("corrupt lands");
    with_registry_caps(async {
        let err = MarketRegistry::open(&root).expect_err("corrupt refuses");
        assert!(
            matches!(err, StoreError::InvalidBar(_)),
            "a corrupt catalog is a loud refusal, never a silent default: {err:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn the_catalog_json_shape_is_pinned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    with_registry_caps(async {
        let registry = MarketRegistry::open(&root).expect("registry opens");
        register_defaults(&registry, "SPY", VENUE).expect("register");
    })
    .await;
    let text = std::fs::read_to_string(root.join("catalog.json")).expect("catalog reads");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(
        value
            .get("next_symbol_id")
            .and_then(serde_json::Value::as_u64),
        Some(2),
        "one registration advances the allocator past id 1: {text}"
    );
    let markets = value
        .get("markets")
        .and_then(serde_json::Value::as_array)
        .expect("markets array");
    assert_eq!(markets.len(), 1);
    let market = &markets[0];
    assert_eq!(
        market.get("symbol").and_then(serde_json::Value::as_str),
        Some("SPY")
    );
    assert_eq!(
        market.get("venue").and_then(serde_json::Value::as_str),
        Some(VENUE)
    );
    assert_eq!(
        market
            .get("asset_class")
            .and_then(serde_json::Value::as_str),
        Some("stock")
    );
    assert_eq!(
        market.get("time_zone").and_then(serde_json::Value::as_str),
        Some("UTC")
    );
    assert_eq!(
        market.get("price_scale"),
        Some(&serde_json::json!({"digits": 2})),
        "the scale keeps its structured shape: {text}"
    );
    assert_eq!(
        market.get("symbol_id").and_then(serde_json::Value::as_u64),
        Some(1)
    );
}
