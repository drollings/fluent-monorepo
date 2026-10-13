//! Per-tick serve cost at the resident-ring seam
//! (`BarRing::serve_fetch_tick`): throughput numbers only, never
//! optimization targets; benches assert nothing.
//!
//! Workload honesty: both benches drive the exact seam the leased `fetch`
//! path runs on a hit (accumulator fold + tail-slot audit-sync + cursor),
//! over rings built with the public `reconcile` the `bars` suites already
//! prove correct — what fails there never reaches `cargo bench`. The field
//! table carries full-capacity candle arrays (`ch/cl/cc/cv/cb/ct` plus the
//! cursor scalars), mirroring the Pine table shape post-`init`, so the
//! per-serve table clone costs what a lease hit pays. Ticks cycle an
//! 8-print steady bar (all `is_new = false`): pure steady-state cost;
//! bar-advance appends ride the `mtf_streaming` bench, and commit-vs-
//! fallback shares ride the engine counters the lease suite observes.
//!
//! Bound documented for acceptance: per-tick serve touches the accumulator
//! fold, at most one tail slot, and one bounded table clone — no history
//! walk, no hydrate. Cost is O(1) in tape length by construction (the ring
//! never exceeds `max_candles`); the small/large pair quantifies the
//! capacity constant, not a scaling law. The large ring's ~20x reflects the
//! full-capacity table clone (6 x 1445 cells) the lease hit pays per serve
//! — a config-fixed constant, flat across iterations (note the tight
//! confidence intervals: streamed state never grows the per-tick work).
//!
//! Baselines (bench profile, same machine, 2026-10-13):
//!
//! | Bench | mean |
//! |-------|------|
//! | `fetch_tick/small_ring` | 3.79 µs/iter |
//! | `fetch_tick/large_ring` | 76.3 µs/iter |

use criterion::{Criterion, criterion_group, criterion_main};
use protopyne_kernels::{
    BarRing, BarsConfig, FieldTable, LtfRow, NativeCall, NativeFetchTick, PineValue,
    RingCursor, indicator::BarTick,
};

const MIN: i64 = 60_000;
const DAY: i64 = 86_400_000;
const T0: i64 = 1_767_344_400_000;
const ARRAY_KEYS: [&str; 6] = ["ch", "cl", "cc", "cv", "cb", "ct"];

fn rows(count: usize) -> Vec<LtfRow> {
    (0..count)
        .map(|index| {
            let base = 100.0 + index as f64 * 0.01;
            LtfRow {
                ts_ms: T0 + index as i64 * MIN,
                o: base,
                h: base + 0.05,
                l: base - 0.05,
                c: base + 0.02,
                volume: 1_000.0,
            }
        })
        .collect()
}

struct Rig {
    ring: BarRing,
    fields: FieldTable,
    cursor: RingCursor,
    ticks: Vec<NativeFetchTick>,
    next: usize,
}

fn rig(config: &BarsConfig, row_count: usize) -> Rig {
    let mut ring = BarRing::new(config).expect("config valid");
    let built = rows(row_count);
    let reference: f64 = built.iter().map(|row| row.volume).sum();
    ring.reconcile(&built, reference, built[0].o);
    // Full-capacity table shape, mirroring post-`init`: per-serve clone
    // costs what the lease hit pays.
    let max = config.max_candles();
    let mut fields = FieldTable::new();
    for key in ARRAY_KEYS {
        fields.insert(key.to_owned(), Some(PineValue::Array(vec![None; max])));
    }
    fields.insert("max_candles".to_owned(), Some(PineValue::Int(max as i64)));
    let cursor = RingCursor { head: 0, count: ring.len() };
    let last = built.last().expect("rows non-empty");
    let ticks = (0..8u32)
        .map(|step| NativeFetchTick {
            legs: BarTick {
                open: last.o,
                high: last.h + f64::from(step) * 0.01,
                low: last.l,
                close: last.c + f64::from(step) * 0.01,
                volume: last.volume + f64::from(step) * 10.0,
            },
            tick_ms: last.ts_ms + 5_000 + i64::from(step) * 1_000,
            bar_open_ms: last.ts_ms,
            is_new: false,
            is_confirmed: false,
            confirm: None,
        })
        .collect();
    Rig { ring, fields, cursor, ticks, next: 0 }
}

fn steady_serve(criterion: &mut Criterion, name: &str, config: &BarsConfig, row_count: usize) {
    criterion.bench_function(name, |bencher| {
        let mut rig = rig(config, row_count);
        bencher.iter(|| {
            let tick = &rig.ticks[rig.next % rig.ticks.len()];
            rig.next += 1;
            let (outcome, next) = rig
                .ring
                .serve_fetch_tick(&mut rig.fields, rig.cursor, "Bars", tick)
                .expect("steady tick serves");
            let NativeCall::Served(_) = outcome else {
                panic!("steady tick commits, got {outcome:?}");
            };
            rig.cursor = next;
        });
    });
}

fn small_ring(criterion: &mut Criterion) {
    let config = BarsConfig::new(MIN, MIN, 5 * MIN, DAY);
    steady_serve(criterion, "fetch_tick/small_ring", &config, 8);
}

fn large_ring(criterion: &mut Criterion) {
    let config = BarsConfig::new(MIN, MIN, DAY, 7 * DAY);
    steady_serve(criterion, "fetch_tick/large_ring", &config, 1_000);
}

criterion_group!(benches, small_ring, large_ring);
criterion_main!(benches);
