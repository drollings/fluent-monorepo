//! Differential contract for the unified tick fold: one frozen tape run
//! through both entries tick-for-tick — rehearsal accumulation
//! (`on_bar_open` + `ingest_tick`, side-channel stamps) and native `fetch`
//! (`fetch_tick_native`, bar-open stamps) — must agree on every fold outcome
//! except the two documented clock fields.
//!
//! The tape covers all four fold arms (seed, accumulate, flat, rollback).
//! Snapshots ride `write_pine_fields` so the accumulator and every counter
//! compare in one table; only `last_time`/`last_time_diff` may differ, and
//! they must follow their documented sources exactly (side-channel stamps
//! vs bar-open time with its always-zero diffs).
//!
//! `#[allow(clippy::float_cmp)]` covers the exact decimal literals, as in
//! the sibling bar-path goldens.
#![allow(clippy::float_cmp)]

use std::collections::HashMap;

use super::*;
use crate::indicator::BarTick;
use crate::value::PineValue;

const T0: i64 = 1_699_999_980_000;

fn config() -> BarsConfig {
    // Hour/day grids: no LTF crossing and no fetch branch inside the tape,
    // so the fold compares without boundary interference.
    BarsConfig::new(3_600_000, 7_200_000, 86_400_000, 86_400_000)
}

/// (stamp offset, price, cumulative volume): seed, accumulate, flat,
/// rollback, accumulate, flat — every fold arm fires.
fn tape() -> Vec<(i64, f64, f64)> {
    vec![
        (5_000, 100.0, 0.0),
        (12_000, 100.5, 100.0),
        (20_000, 100.3, 200.0),
        (33_000, 100.3, 200.0),
        (40_000, 101.0, 150.0),
        (47_000, 101.5, 250.0),
        (52_000, 101.5, 250.0),
    ]
}

fn legs_for(price: f64, cumvol: f64, high: f64, low: f64) -> BarTick {
    BarTick {
        open: 100.0,
        high,
        low,
        close: price,
        volume: cumvol,
    }
}

fn template_fields() -> FieldTable {
    let mut fields = HashMap::new();
    fields.insert("max_candles".to_owned(), Some(PineValue::Int(64)));
    fields.into()
}

fn snapshot(ring: &BarRing) -> FieldTable {
    let mut fields = template_fields();
    ring.write_pine_fields(&mut fields, 0, 0, 0)
        .expect("empty ring snapshots onto the template");
    fields
}

fn int_field(fields: &FieldTable, name: &str) -> Option<i64> {
    match fields.get(name).and_then(|slot| slot.as_ref()) {
        Some(PineValue::Int(value)) => Some(*value),
        None => None,
        Some(other) => panic!("expected int-or-na for {name}, got {other:?}"),
    }
}

/// Snapshot minus the two clock fields: everything the fold must agree on.
fn masked(fields: &FieldTable) -> FieldTable {
    fields
        .iter()
        .filter(|(name, _)| name.as_str() != "last_time" && name.as_str() != "last_time_diff")
        .map(|(name, slot)| (name.clone(), slot.clone()))
        .collect()
}

#[test]
fn unified_fold_agrees_tick_for_tick() {
    let tape = tape();
    let mut high = 100.0f64;
    let mut low = 100.0f64;

    let mut rehearsal = BarRing::new(&config()).expect("config validates");
    let mut native = BarRing::new(&config()).expect("config validates");

    // Tick 0 is the bar open on both paths (no fold yet).
    rehearsal.on_bar_open(100.0, 100.0, 100.0, 100.0, T0);
    let first = NativeFetchTick {
        legs: legs_for(100.0, 0.0, 100.0, 100.0),
        tick_ms: T0 + 5_000,
        bar_open_ms: T0,
        is_new: true,
        is_confirmed: false,
        confirm: None,
    };
    let out = native
        .fetch_tick_native(&first)
        .expect("open tick commits");
    assert_eq!(out.signal, FetchSignal::Commit);
    assert_eq!(out.appended, 0);

    let expected_kinds = [
        TickIngest::Seeded,
        TickIngest::Accumulated {
            volume_delta: 100.0,
            buy_volume: 0.0,
        },
        TickIngest::Unchanged,
        TickIngest::RolledBack,
        TickIngest::Accumulated {
            volume_delta: 100.0,
            buy_volume: 100.0,
        },
        TickIngest::Unchanged,
    ];

    // Rehearsal stamps: seed/rollback/accumulate move `last_time` to the
    // tick stamp; flats leave it (the fold only tracks moving volume).
    let expected_rehearsal_times: [i64; 6] = [
        T0 + 12_000,
        T0 + 20_000,
        T0 + 20_000,
        T0 + 40_000,
        T0 + 47_000,
        T0 + 47_000,
    ];
    // accumulation stamps the tick-to-tick delta, flats and rollbacks keep
    // the prior delta. Native diffs under the bar-open clock: absence after
    // the seed, zero ever after.
    let expected_rehearsal_diffs: [Option<i64>; 6] =
        [None, Some(8_000), Some(8_000), Some(8_000), Some(7_000), Some(7_000)];
    let expected_native_diffs: [Option<i64>; 6] =
        [None, Some(0), Some(0), Some(0), Some(0), Some(0)];

    for (index, (offset, price, cumvol)) in tape.iter().skip(1).enumerate() {
        let stamp = T0 + offset;
        high = high.max(*price);
        low = low.min(*price);
        let legs = legs_for(*price, *cumvol, high, low);

        let kind = rehearsal.ingest_tick(legs, stamp);
        assert_eq!(kind, expected_kinds[index], "fold arm on tick {}", index + 1);

        let tick = NativeFetchTick {
            legs,
            tick_ms: stamp,
            bar_open_ms: T0,
            is_new: false,
            is_confirmed: false,
            confirm: None,
        };
        let out = native.fetch_tick_native(&tick).expect("tick commits");
        assert_eq!(out.signal, FetchSignal::Commit);
        assert_eq!(out.appended, 0);

        let left = snapshot(&rehearsal);
        let right = snapshot(&native);
        assert_eq!(
            masked(&left),
            masked(&right),
            "fold state agrees on tick {}",
            index + 1
        );

        // Clock fields follow their documented sources, never each other.
        assert_eq!(
            int_field(&left, "last_time"),
            Some(expected_rehearsal_times[index]),
            "rehearsal stamp on tick {}",
            index + 1
        );
        assert_eq!(int_field(&right, "last_time"), Some(T0));
        assert_eq!(
            int_field(&left, "last_time_diff"),
            expected_rehearsal_diffs[index],
            "rehearsal diff on tick {}",
            index + 1
        );
        assert_eq!(
            int_field(&right, "last_time_diff"),
            expected_native_diffs[index],
            "native diff on tick {}",
            index + 1
        );
    }

    // Native diffs stay zero under the bar-open clock: bar-open minus
    // bar-open on every accumulation, untouched elsewhere.
    let native_fields = snapshot(&native);
    assert_eq!(
        int_field(&native_fields, "last_time_diff"),
        Some(0),
        "bar-open clock implies always-zero diffs"
    );
    let rehearsal_fields = snapshot(&rehearsal);
    assert_eq!(
        int_field(&rehearsal_fields, "last_time_diff"),
        Some(7_000),
        "final flat tick leaves the prior stamp delta"
    );

    // Counters agree end to end.
    assert_eq!(rehearsal.tick_count(), native.tick_count());
    assert_eq!(rehearsal.tick_count(), 2, "two accumulating ticks");
    assert_eq!(
        rehearsal.tick_volume_total(),
        native.tick_volume_total()
    );
    assert_eq!(rehearsal.tick_volume_total(), 200.0);
    assert_eq!(rehearsal.tick_buy_volume(), native.tick_buy_volume());
    assert_eq!(rehearsal.tick_buy_volume(), 100.0);
    assert_eq!(rehearsal.rollback_count(), native.rollback_count());
    assert_eq!(rehearsal.rollback_count(), 1);
}
