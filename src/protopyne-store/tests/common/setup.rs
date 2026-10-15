//! Shared store-test setup: an in-memory tape with one registered market.
//!
//! Included once from `sqlite_tape.rs`; consumer suites `use` it, never
//! include it again. Registration gates on the task-local capability set,
//! so seeding runs inside an installed `StoreStorage` bound to the
//! ephemeral store — the same shape production sessions use, minus the
//! file.
//!
//! `dead_code` is allowed here so the single home stays single without
//! per-consumer shims.

#![allow(dead_code)]

use std::future::Future;

use protopyne_types::{Bar, Timeframe};

use crate::catalog::AssetClass;
use crate::key::{SymbolId, TapeKey};
use crate::sqlite_tape::SqliteTape;
use crate::store::{CandleStore, WritePolicy};

/// The venue every seeded market prints on.
pub const VENUE: &str = "test-venue";

/// Run `f` with an ephemeral-store capability installed. Registration and
/// every other gated effect succeed inside; outside, they are denied.
pub async fn with_memory_caps<F, T>(f: F) -> T
where
    F: Future<Output = T>,
{
    let caps = fluent_wvr::CapabilitySet::new()
        .with(crate::capability::StoreStorage::new(":memory:".into()));
    fluent_wvr::CURRENT_CAPS.scope(caps, f).await
}

/// An empty in-memory tape. Registration and writes still require
/// [`with_memory_caps`].
#[must_use]
pub fn memory_tape() -> SqliteTape {
    SqliteTape::open_in_memory().expect("in-memory tape opens")
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
pub async fn seeded(symbol: &str, bars: &[Bar]) -> (SqliteTape, TapeKey) {
    with_memory_caps(async {
        let tape = memory_tape();
        let symbol_id = tape
            .register(
                symbol,
                VENUE,
                AssetClass::Stock,
                crate::catalog::PriceScale::of(2).expect("valid scale"),
                "UTC",
                protopyne_types::Timeframe::M1,
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
