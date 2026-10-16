//! Shared file-tape test setup: an in-memory `MarketTape` plus seeding.
//!
//! Included once from `tape.rs`; consumer suites `use` it, never include it
//! again. Registration gates on the task-local capability set, so seeding
//! runs inside an installed `StoreStorage` bound to the ephemeral store —
//! the same shape production sessions use, minus the file.
//!
//! `dead_code` is allowed here so the single home stays single without
//! per-consumer shims.

#![allow(dead_code)]

use protopyne_types::{Bar, Timeframe};

use crate::key::{SymbolId, TapeKey};
pub use crate::registry::test_setup::{VENUE, with_registry_caps};
use crate::store::{CandleStore, WritePolicy};
use crate::tape::MarketTape;

/// An empty in-memory tape. Registration and writes still require the
/// capability scope from [`with_registry_caps`].
#[must_use]
pub fn memory_tape() -> MarketTape {
    MarketTape::open_in_memory().expect("in-memory tape opens")
}

/// The one-minute key for `symbol_id`.
#[must_use]
pub fn m1_key(symbol_id: SymbolId) -> TapeKey {
    TapeKey {
        symbol_id,
        timeframe: Timeframe::M1,
    }
}

/// Register `symbol` at a two-digit scale and append `bars` under
/// `InsertIfAbsent`. Returns the tape and the tape's key.
pub async fn seeded(symbol: &str, bars: &[Bar]) -> (MarketTape, TapeKey) {
    with_registry_caps(async {
        let tape = memory_tape();
        let symbol_id = tape
            .register(
                symbol,
                VENUE,
                crate::catalog::AssetClass::Stock,
                crate::catalog::PriceScale::of(2).expect("valid scale"),
                "UTC",
                Timeframe::M1,
            )
            .expect("registration succeeds");
        let key = m1_key(symbol_id);
        if !bars.is_empty() {
            tape.append(key, bars, WritePolicy::InsertIfAbsent)
                .expect("seed append succeeds");
        }
        (tape, key)
    })
    .await
}
