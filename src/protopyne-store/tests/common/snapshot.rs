//! Shared snapshot-test setup: an in-memory tape with one seeded market,
//! plus the capability scope every gated effect needs.
//!
//! Included once from `snapshot.rs`; consumer suites `use` it, never include
//! it again. `dead_code` is allowed here so the single home stays single
//! without per-consumer shims.

#![allow(dead_code)]

use protopyne_types::{Bar, Timeframe};

use crate::catalog::AssetClass;
use crate::key::TapeKey;
use crate::sqlite_tape::SqliteTape;
use crate::sqlite_tape::test_setup::{
    VENUE, m1_key, memory_tape, with_memory_caps,
};
use crate::store::{CandleStore, WritePolicy};

/// The stamp every snapshot test seals at, so a written record is
/// deterministic across runs. A real seal passes the wall clock.
pub const SEAL_NOW_MS: i64 = 1_767_225_600_000;

/// Run `f` inside an ephemeral-store capability scope, against `symbol`
/// registered at `scale` and seeded with `bars`. `f` receives the tape and
/// its key and may do synchronous work (seal, reconcile, read) while the
/// capability is still installed.
pub async fn with_seeded_tape<F, T>(symbol: &str, scale: u32, bars: &[Bar], f: F) -> T
where
    F: FnOnce(&SqliteTape, TapeKey) -> T,
{
    with_memory_caps(async {
        let tape = memory_tape();
        let symbol_id = tape
            .register(
                symbol,
                VENUE,
                AssetClass::Stock,
                crate::catalog::PriceScale::of(scale).expect("valid scale"),
                "UTC",
                Timeframe::M1,
            )
            .expect("registration succeeds");
        let key = m1_key(symbol_id);
        if !bars.is_empty() {
            tape.append(key, bars, WritePolicy::InsertIfAbsent)
                .expect("seed append succeeds");
        }
        f(&tape, key)
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
