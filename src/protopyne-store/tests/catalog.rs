//! The catalog contract: resolve reads, register creates, bounds refuse.
//!
//! Registration is the only creator, and it refuses three ways — blank
//! symbol, duplicate `(symbol, venue)`, full table — each with its own
//! reason, so a caller can tell "already there, resolve it" from "the
//! catalog is full, intervene". Resolution never creates: an unknown
//! symbol is `Ok(None)`, which is what keeps a missing market from
//! reading as a broken store.

use super::*;
use crate::sqlite_tape::test_setup::{VENUE, memory_tape, with_memory_caps};

fn register_defaults(
    catalog: &MarketCatalog,
    symbol: &str,
    venue: &str,
) -> Result<SymbolId, StoreError> {
    catalog.register(
        symbol,
        venue,
        AssetClass::Stock,
        crate::catalog::PriceScale::of(2).expect("valid scale"),
        "UTC",
        protopyne_types::Timeframe::M1,
    )
}

#[tokio::test]
async fn register_then_resolve_round_trip() {
    with_memory_caps(async {
        let tape = memory_tape();
        let catalog = tape.catalog();
        let id = register_defaults(catalog, "SPY", VENUE).expect("register");
        assert_eq!(catalog.resolve("SPY", VENUE).expect("resolve"), Some(id));
        let markets = catalog.markets().expect("markets");
        assert_eq!(markets.len(), 1);
        assert_eq!(markets[0].symbol_id, id);
        assert_eq!(markets[0].symbol, "SPY");
        assert_eq!(markets[0].venue, VENUE);
        assert_eq!(markets[0].asset_class, AssetClass::Stock);
        assert_eq!(
            markets[0].price_scale,
            crate::catalog::PriceScale::of(2).expect("valid scale")
        );
        assert_eq!(
            markets[0].timeframe,
            protopyne_types::Timeframe::M1,
            "the registered spacing reads back"
        );
        assert_eq!(
            catalog
                .tape_of("SPY", VENUE)
                .map(|(_, timeframe)| timeframe),
            Some(protopyne_types::Timeframe::M1),
            "the planner's lookup carries the spacing"
        );
    })
    .await;
}

#[tokio::test]
async fn same_symbol_on_two_venues_registers_twice() {
    with_memory_caps(async {
        let tape = memory_tape();
        let catalog = tape.catalog();
        let first = register_defaults(catalog, "SPY", "venue-a").expect("first venue");
        let second = register_defaults(catalog, "SPY", "venue-b").expect("second venue");
        assert_ne!(
            first, second,
            "identity is per (symbol, venue), not per spelling"
        );
        assert_eq!(
            catalog.resolve("SPY", "venue-a").expect("resolve a"),
            Some(first)
        );
        assert_eq!(
            catalog.resolve("SPY", "venue-b").expect("resolve b"),
            Some(second)
        );
    })
    .await;
}

#[tokio::test]
async fn duplicate_symbol_venue_refuses() {
    with_memory_caps(async {
        let tape = memory_tape();
        let catalog = tape.catalog();
        let id = register_defaults(catalog, "SPY", VENUE).expect("first");
        let err = register_defaults(catalog, "SPY", VENUE).expect_err("duplicate refuses");
        assert!(
            matches!(err, StoreError::Duplicate(_)),
            "a duplicate is its own refusal, not a database failure: {err:?}"
        );
        assert_eq!(
            catalog.resolve("SPY", VENUE).expect("resolve"),
            Some(id),
            "the refused registration changes nothing"
        );
    })
    .await;
}

#[tokio::test]
async fn blank_symbol_refuses() {
    with_memory_caps(async {
        let tape = memory_tape();
        let catalog = tape.catalog();
        let err = register_defaults(catalog, "", VENUE).expect_err("blank refuses");
        assert!(
            matches!(err, StoreError::Plan(PlanError::BlankSymbol)),
            "a blank symbol is a plan refusal: {err:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn resolve_unknown_is_none_not_error() {
    with_memory_caps(async {
        let tape = memory_tape();
        let catalog = tape.catalog();
        assert_eq!(catalog.resolve("NOPE", VENUE).expect("resolve"), None);
    })
    .await;
}

#[tokio::test]
async fn full_table_refuses_rather_than_evicting() {
    with_memory_caps(async {
        let catalog = MarketCatalog::open_in_memory_with_max(2).expect("bounded catalog opens");
        register_defaults(&catalog, "SPY", VENUE).expect("first");
        register_defaults(&catalog, "QQQ", VENUE).expect("second");
        let err = register_defaults(&catalog, "IWM", VENUE).expect_err("full refuses");
        assert!(
            matches!(err, StoreError::CatalogFull(_)),
            "a full table refuses with its own reason: {err:?}"
        );
        assert!(
            catalog.resolve("SPY", VENUE).expect("resolve").is_some(),
            "refusing never evicts: the first market survives"
        );
        assert!(
            catalog.resolve("QQQ", VENUE).expect("resolve").is_some(),
            "refusing never evicts: the second market survives"
        );
    })
    .await;
}

#[tokio::test]
async fn a_non_minute_spacing_registers_and_reads_back() {
    with_memory_caps(async {
        let tape = memory_tape();
        let catalog = tape.catalog();
        let id = catalog
            .register(
                "SPY",
                VENUE,
                AssetClass::Stock,
                crate::catalog::PriceScale::of(2).expect("valid scale"),
                "UTC",
                protopyne_types::Timeframe::H1,
            )
            .expect("hourly market registers");
        let markets = catalog.markets().expect("markets");
        assert_eq!(markets.len(), 1);
        assert_eq!(markets[0].symbol_id, id);
        assert_eq!(
            markets[0].timeframe,
            protopyne_types::Timeframe::H1,
            "the tape's spacing is durable, not a caller-side claim"
        );
        assert_eq!(
            catalog.tape_of("SPY", VENUE),
            Some((id, protopyne_types::Timeframe::H1))
        );
    })
    .await;
}

#[tokio::test]
async fn registration_outside_the_capability_is_denied() {
    // No installed capability set: the catalog's own gate fires before any
    // database touch.
    let tape = memory_tape();
    let err = register_defaults(tape.catalog(), "SPY", VENUE).expect_err("no capability");
    assert!(
        matches!(err, StoreError::Denied(_)),
        "an ungranted task is denied, not defaulted: {err:?}"
    );
}
