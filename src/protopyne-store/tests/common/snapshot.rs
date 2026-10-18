//! Shared snapshot-test setup: an in-memory registry with one seeded market,
//! plus the capability scope every gated effect needs.
//!
//! Included once from `snapshot.rs`; consumer suites `use` it, never include
//! it again. `dead_code` is allowed here so the single home stays single
//! without per-consumer shims.

#![allow(dead_code)]

use protopyne_types::{Bar, Timeframe};

use crate::key::TapeKey;
use crate::registry::test_setup::{with_registry_caps, VENUE};
use crate::registry::MarketRegistry;

/// The stamp every snapshot test seals at, so a written record is
/// deterministic across runs. A real seal passes the wall clock.
pub const SEAL_NOW_MS: i64 = 1_767_225_600_000;

/// Run `f` inside an ephemeral-registry capability scope, against `symbol`
/// registered at `scale`. `f` receives the registry and its key and may do
/// synchronous work (seal, reconcile, read) while the capability is still
/// installed. Bars ride the closure's own capture: the seal reads them from
/// its caller, never from a tape.
pub async fn with_seeded_tape<F, T>(symbol: &str, scale: u32, bars: &[Bar], f: F) -> T
where
    F: FnOnce(&MarketRegistry, TapeKey) -> T,
{
    let _ = bars;
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let symbol_id = registry
            .register(
                symbol,
                VENUE,
                crate::catalog::AssetClass::Stock,
                crate::catalog::PriceScale::of(scale).expect("valid scale"),
                "UTC",
                Timeframe::M1,
            )
            .expect("registration succeeds");
        let key = TapeKey {
            symbol_id,
            timeframe: Timeframe::M1,
        };
        f(&registry, key)
    })
    .await
}

/// `n` ascending M1 bars from step 0, each closing a cent above its opening,
/// with the range varying so price content is distinct per bar. The OHLC legs
/// stay ordered (`h >= o, l <= o`) by construction.
#[must_use]
pub fn varied_bars(n: usize) -> Vec<Bar> {
    (0..n as i64)
        .map(|step| {
            let base = 100 + (step % 97);
            crate::commit::test_tapes::ohlc(
                step,
                &format!("{base}.00"),
                &format!("{}.05", base + 1),
                &format!("{}.95", base - 1),
                &format!("{base}.01"),
            )
        })
        .collect()
}
