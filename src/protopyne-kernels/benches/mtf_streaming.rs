//! Streaming-store baselines: append-one-bar throughput over a 10k-bar
//! 1-minute tape across three spans (5m/15m/1h, SMA-14 each), plus the
//! steady-state no-op `ensure` cost. Throughput numbers only, never
//! optimization targets; a red bench re-examines the append path, never the
//! assertion thresholds (there are none — benches assert nothing).
//!
//! Workload honesty: both benches compose the public store API the
//! `mtf_incremental` suite already proves correct, so no separate smoke
//! test guards harness rot — what fails there never reaches `cargo bench`.
//! `append_one_bar` grows one live store (9k-bar base, one append + `ensure`
//! per iteration, `now` tracking each close): the ~1% size drift across a
//! run is the honest streaming shape, not identical repeated work.
//! `steady_noop_ensure` repeats the identical confirmed-state `ensure`.
//!
//! Baselines (bench profile, same machine; post-change numbers re-recorded
//! 2026-10-13 after the M12 correctness tails — bound holds):
//!
//! | Bench | Pre (full rebuild) | Post (append path) | Re-recorded |
//! |-------|--------------------|--------------------|-------------|
//! | `append_one_bar` | 2.41 ms/iter | 1.37 ms/iter | 1.37 ms/iter |
//! | `steady_noop_ensure` | 1.14 ms/iter | 10.2 µs/iter | 9.92 µs/iter |
//!
//! Pre-change cost is full re-projection per span per `ensure` (O(history)
//! each) plus whole-window kernel recompute on every new bucket; even the
//! no-op pays three 10k-bar projections plus five-leg memcmps. Post-change
//! the no-op is three hashmap lookups (skip path); append cost is the tail
//! fold plus whole-window kernel recompute only when bucket values change —
//! the remaining millisecond is inherent to whole-window `batch` kernels,
//! not to frame management.
//!
//! Bound documented for acceptance: steady `ensure` touches no kernel math
//! (freshness backdates everything); append cost scales with the tail fold
//! plus whole-window kernel recompute only when bucket values change.

use criterion::{Criterion, criterion_group, criterion_main};
use protopyne_kernels::{
    Lookahead, MtfStore, ParamSet, SeriesKey, indicator::builtin_registry,
};
use protopyne_types::Bar;
use rust_decimal::Decimal;

const T0: i64 = 1_699_999_980_000;
const MIN: i64 = 60_000;
const SPANS: [i64; 3] = [5 * MIN, 15 * MIN, 60 * MIN];
const TAPE_BARS: usize = 10_000;
const STREAM_BASE: usize = 9_000;

fn bar_at(open_ms: i64, step: i64) -> Bar {
    let cents = 10_000 + step * 5;
    Bar::new()
        .ts(chrono::DateTime::from_timestamp(open_ms / 1000, 0).expect("in range"))
        .o(Decimal::new(cents, 2))
        .h(Decimal::new(cents + 12, 2))
        .l(Decimal::new(cents - 12, 2))
        .c(Decimal::new(cents + 2, 2))
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

fn requests() -> Vec<(SeriesKey, ParamSet)> {
    let params = ParamSet::new().with_int("length", 14);
    SPANS
        .iter()
        .map(|span| (SeriesKey::new(*span, "sma", &params).expect("span valid"), params.clone()))
        .collect()
}

fn append_one_bar(criterion: &mut Criterion) {
    let base: Vec<Bar> =
        (0..STREAM_BASE as i64).map(|step| bar_at(T0 + step * MIN, step)).collect();
    let reqs = requests();
    let registry = builtin_registry().expect("builtins load");
    criterion.bench_function("mtf_streaming/append_one_bar", |bencher| {
        let mut store = MtfStore::new();
        store.set_base(&base).expect("base loads");
        let mut now_ms = T0 + STREAM_BASE as i64 * MIN;
        store
            .ensure(&registry, &reqs, now_ms, Lookahead::Off)
            .expect("initial ensure runs");
        // Bars synthesize on the fly (pure constructor): iterations never
        // exhaust a tape, and per-iter construction is constant — the same
        // cost a live feed pays to mint each bar.
        let mut next = STREAM_BASE as i64;
        bencher.iter(|| {
            let bar = bar_at(T0 + next * MIN, next);
            next += 1;
            store.append_bar(&bar).expect("tape advances");
            now_ms = bar.ts.timestamp_millis() + MIN;
            store.ensure(&registry, &reqs, now_ms, Lookahead::Off).expect("streaming ensure runs");
        });
    });
}

fn steady_noop_ensure(criterion: &mut Criterion) {
    let bars: Vec<Bar> =
        (0..TAPE_BARS as i64).map(|step| bar_at(T0 + step * MIN, step)).collect();
    let reqs = requests();
    let registry = builtin_registry().expect("builtins load");
    let mut store = MtfStore::new();
    store.set_base(&bars).expect("base loads");
    let now_ms = T0 + TAPE_BARS as i64 * MIN;
    store.ensure(&registry, &reqs, now_ms, Lookahead::Off).expect("initial ensure runs");
    criterion.bench_function("mtf_streaming/steady_noop_ensure", |bencher| {
        bencher.iter(|| {
            store
                .ensure(&registry, &reqs, now_ms, Lookahead::Off)
                .expect("steady ensure runs");
        });
    });
}

criterion_group!(benches, append_one_bar, steady_noop_ensure);
criterion_main!(benches);
