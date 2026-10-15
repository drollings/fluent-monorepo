//! Shared bar constructors for the store's lifecycle suites.
//!
//! One home for "a bar at this stamp with these prices", so the suites below
//! differ only in the fact under test and never in how a fixture bar is
//! spelled. Every constructor lays bars out on a fixed minute grid from one
//! base stamp, which makes "strictly later", "the same instant", and
//! "strictly earlier" expressible as small integer steps.
//!
//! Included once from `commit.rs`; consumer suites `use` it, never include it
//! again. `dead_code` is allowed here so the single home stays single without
//! per-consumer shims.

#![allow(dead_code)]

use chrono::{DateTime, Duration, TimeZone as _, Utc};
use protopyne_types::Bar;

/// The base every fixture tape is laid out from: `2026-01-05 20:15:00 UTC`, a
/// Monday well clear of any session boundary, so a stamp's meaning never
/// depends on which suite is reading it.
#[must_use]
pub fn base() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 5, 20, 15, 0)
        .single()
        .expect("fixture base stamp is a valid instant")
}

/// The stamp `step` minutes after [`base`].
#[must_use]
pub fn stamp(step: i64) -> DateTime<Utc> {
    base() + Duration::minutes(step)
}

/// The bar primitive: one stamp, four prices, one volume. Everything else in
/// this file is a spelling of this call.
#[must_use]
pub fn ohlc(step: i64, o: &str, h: &str, l: &str, c: &str) -> Bar {
    Bar::new()
        .ts(stamp(step))
        .o(o.parse().expect("valid decimal"))
        .h(h.parse().expect("valid decimal"))
        .l(l.parse().expect("valid decimal"))
        .c(c.parse().expect("valid decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

/// A flat bar `step` minutes after [`base`]: open, high, low, and close all
/// carry `close`, which satisfies every OHLC invariant by construction.
#[must_use]
pub fn bar(step: i64, close: &str) -> Bar {
    ohlc(step, close, close, close, close)
}

/// A republish of the forming bar at `step`: the same instant and the same
/// open, with the close moved and the high extended to cover it — the venue
/// restating a bar that has not finished, which is ordinary streaming and not
/// a late arrival.
#[must_use]
pub fn revised(step: i64, open: &str, close: &str) -> Bar {
    ohlc(step, open, close, open, close)
}

/// A flat bar stamped at an absolute millisecond instant. The coverage suites
/// lay out tapes on the clock rather than on a step grid, so every timestamp
/// here is stated exactly.
#[must_use]
pub fn at_ms(ts_ms: i64) -> Bar {
    Bar::new()
        .ts(chrono::DateTime::from_timestamp_millis(ts_ms).expect("representable stamp"))
        .o("100".parse().expect("valid decimal"))
        .h("100".parse().expect("valid decimal"))
        .l("100".parse().expect("valid decimal"))
        .c("100".parse().expect("valid decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}
