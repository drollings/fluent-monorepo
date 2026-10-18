//! Read-path scale: one-day reads, torn-month pagination, and boot replay
//! across tape sizes.
//!
//! Numbers only, never optimization: the enforced gate is the structural
//! budget suite (`tests/tape_budget.rs`), since absolute timings are
//! machine-dependent. The control group that must not regress is the
//! contiguous-tape read; the treatment is torn-tape pagination, which must
//! stay bounded by the window — flat per-page cost as the tape grows —
//! because planning now walks windowed stamps instead of the whole tape.
//! Boot replay is O(tape) by allowance; the replay group records what that
//! allowance costs at each size so a regression in the log tail is visible.

use chrono::DateTime;
use criterion::{criterion_group, criterion_main, Criterion};
use protopyne_store::{
    AssetClass, CandleStore, FoldPolicy, MarketTape, PriceScale, ReadRequest, StoreStorage,
    TapeKey, TimeRange, WritePolicy,
};
use protopyne_types::{Bar, Timeframe};

const STEP_MS: i64 = 60_000;
const DAY_MS: i64 = 1_440 * STEP_MS;
const BASE_MS: i64 = 1_700_000_000_000;

fn flat_bar(ts_ms: i64) -> Bar {
    let price: rust_decimal::Decimal = "100".parse().expect("valid decimal");
    Bar::new()
        .ts(DateTime::from_timestamp_millis(ts_ms).expect("representable stamp"))
        .o(price)
        .h(price)
        .l(price)
        .c(price)
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn caps_for(root: &std::path::Path) -> fluent_wvr::CapabilitySet {
    fluent_wvr::CapabilitySet::new().with(StoreStorage::new(root.to_path_buf()))
}

/// Seed `months` of minute bars on a file-backed tape, dropping every sixth
/// bar over days 15..19 when `torn` — a holey zone the measured pages never
/// touch, proving interior holes elsewhere do not move a distant page's
/// cost. Seeding runs outside the measured loop; each day appends as one
/// chunk.
fn seed_tape(
    months: i64,
    torn: bool,
) -> (MarketTape, tempfile::TempDir, fluent_wvr::CapabilitySet) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("bench-store");
    let caps = caps_for(&root);
    fluent_wvr::CURRENT_CAPS.sync_scope(caps.clone(), || {
        let tape = MarketTape::open(&root).expect("bench tape opens");
        tape.register(
            "BENCH",
            "bench-venue",
            AssetClass::Stock,
            PriceScale::of(2).expect("valid scale"),
            "UTC",
            Timeframe::M1,
        )
        .expect("register");
        let symbol_id = tape
            .resolve("BENCH", "bench-venue")
            .expect("resolve")
            .expect("known");
        let key = TapeKey {
            symbol_id,
            timeframe: Timeframe::M1,
        };
        for day in 0..months * 30 {
            let mut chunk = Vec::with_capacity(1_440);
            for minute in 0..1_440_i64 {
                if torn && (15..20).contains(&day) && (day * 1_440 + minute) % 6 == 0 {
                    continue;
                }
                chunk.push(flat_bar(BASE_MS + day * DAY_MS + minute * STEP_MS));
            }
            tape.append(key, &chunk, WritePolicy::InsertIfAbsent)
                .expect("append");
        }
        (tape, dir, caps)
    })
}

fn day_request(day: i64) -> ReadRequest {
    let start = BASE_MS + day * DAY_MS;
    ReadRequest {
        symbol: "BENCH".to_owned(),
        venue: "bench-venue".to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange {
            start_ms: Some(start),
            end_ms: Some(start + DAY_MS),
        },
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    }
}

fn scoped_read(caps: &fluent_wvr::CapabilitySet, tape: &MarketTape, req: &ReadRequest) -> usize {
    fluent_wvr::CURRENT_CAPS.sync_scope(caps.clone(), || {
        tape.read(req).expect("bench read serves").bars.len()
    })
}

fn read_one_day(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_one_day");
    for months in [1, 3, 12] {
        for torn in [false, true] {
            let name = format!("{months}mo_{}", if torn { "torn" } else { "contiguous" });
            let (tape, _dir, caps) = seed_tape(months, torn);
            let req = day_request(10);
            group.bench_function(name, |b| {
                b.iter(|| scoped_read(&caps, &tape, &req));
            });
        }
    }
    group.finish();
}

fn paginate_torn_month(c: &mut Criterion) {
    let mut group = c.benchmark_group("paginate_torn_month");
    for months in [1, 3, 12] {
        let (tape, _dir, caps) = seed_tape(months, true);
        // Fourteen serving day reads over the contiguous prefix: per-page
        // cost must not grow with the tape — holey or whole — behind it.
        let reqs: Vec<ReadRequest> = (0..14).map(day_request).collect();
        group.bench_function(format!("{months}mo"), |b| {
            b.iter(|| {
                let mut bars = 0_usize;
                for req in &reqs {
                    bars += scoped_read(&caps, &tape, req);
                }
                bars
            });
        });
    }
    group.finish();
}

fn boot_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("boot_replay");
    for months in [1, 3, 12] {
        let (_tape, dir, _caps) = seed_tape(months, false);
        let root = dir.path().join("bench-store");
        group.bench_function(format!("{months}mo"), |b| {
            b.iter(|| {
                let caps = caps_for(&root);
                fluent_wvr::CURRENT_CAPS
                    .sync_scope(caps, || MarketTape::open(&root).expect("replay opens"))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, read_one_day, paginate_torn_month, boot_replay);
criterion_main!(benches);
