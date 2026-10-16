//! Shared registry-test setup: capability scope plus default registration.
//!
//! Included once from `registry.rs`; consumer suites `use` it, never include
//! it again. Registration gates on the task-local capability set, so seeding
//! runs inside an installed `StoreStorage` bound to the ephemeral registry —
//! the same shape production sessions use, minus the file.
//!
//! `dead_code` is allowed here so the single home stays single without
//! per-consumer shims.

#![allow(dead_code)]

use std::future::Future;

use crate::key::SymbolId;
use crate::registry::MarketRegistry;

/// The venue every seeded market prints on.
pub const VENUE: &str = "test-venue";

/// Run `f` with an ephemeral-registry capability installed. Registration and
/// every other gated effect succeed inside; outside, they are denied.
pub async fn with_registry_caps<F, T>(f: F) -> T
where
    F: Future<Output = T>,
{
    let caps = fluent_wvr::CapabilitySet::new()
        .with(crate::capability::StoreStorage::new(":memory:".into()));
    fluent_wvr::CURRENT_CAPS.scope(caps, f).await
}

/// Register `symbol` at a two-digit scale on the one-minute grid.
pub fn register_defaults(
    registry: &MarketRegistry,
    symbol: &str,
    venue: &str,
) -> Result<SymbolId, crate::error::StoreError> {
    registry.register(
        symbol,
        venue,
        crate::catalog::AssetClass::Stock,
        crate::catalog::PriceScale::of(2).expect("valid scale"),
        "UTC",
        protopyne_types::Timeframe::M1,
    )
}
