//! History-load economics: Parquet, CSV, and the SQLite tape over the
//! committed SPY month, plus the load-time tally into coarser frames.
//!
//! Numbers only, never optimization: the three arms are the real ingest
//! paths (`load_bars_parquet`, `load_bars_csv`, `SqliteTape::read`), all
//! over the committed one-month fixture. Parquet and CSV serve the vendor
//! series verbatim (the bench asserts equal bar counts before timing). The
//! SQLite tape is seeded with the standard gap-fill convention — a missing
//! minute is a flat bar at the previous close with zero volume — because
//! the store's read refuses torn windows by design, and a month of
//! committed bars is what a tape holds in practice. The CSV and tape
//! fixtures are prepared from the Parquet file outside the measured loop.
//!
//! Run: `cargo bench -p protopyne-server --bench history_load`.

use std::path::PathBuf;
use std::sync::OnceLock;

use criterion::{criterion_group, Criterion};

use protopyne_server::{fold_frames, load_bars_csv, load_bars_parquet};
use protopyne_store::{
    AssetClass, CandleStore as _, FoldPolicy, PriceScale, ReadRequest as TapeReadRequest,
    SqliteTape, StoreStorage, TapeKey, TimeRange, WritePolicy,
};
use protopyne_types::{Bar, Timeframe};

const VENUE: &str = "bench-venue";

fn month_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs/historical/SPY-2026-03-1M.parquet")
}

/// The committed series with every missing minute filled flat (previous
/// close, zero volume): the tape a month of committed 1M bars actually
/// holds, accepted by the store's hole-refusing read planner.
fn gap_filled(bars: &[Bar]) -> Vec<Bar> {
    let step = 60_000_i64;
    let mut out: Vec<Bar> = Vec::new();
    let mut cursor = bars.first().map_or(0, |bar| bar.ts.timestamp_millis());
    for bar in bars {
        let ts = bar.ts.timestamp_millis();
        while cursor < ts {
            let previous = out.last().expect("the first bar seeds the grid").c;
            out.push(
                Bar::new()
                    .ts(chrono::DateTime::from_timestamp_millis(cursor).expect("in range"))
                    .o(previous)
                    .h(previous)
                    .l(previous)
                    .c(previous)
                    .volume(0)
                    .maybe_adj(None)
                    .build(),
            );
            cursor += step;
        }
        out.push(bar.clone());
        cursor = ts + step;
    }
    out
}

/// Everything the timed closures need, built once: the committed bars, a
/// CSV rendering of them, and a seeded SQLite tape.
struct Fixtures {
    parquet: PathBuf,
    csv: PathBuf,
    tape: SqliteTape,
    caps: fluent_wvr::CapabilitySet,
    bars: Vec<Bar>,
    tape_bars: usize,
    workdir: tempfile::TempDir,
}

fn caps_for(path: &std::path::Path) -> fluent_wvr::CapabilitySet {
    fluent_wvr::CapabilitySet::new().with(StoreStorage::new(path.to_path_buf()))
}

fn render_csv(bars: &[Bar]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bars.len() * 48);
    out.push_str("time,open,high,low,close,volume\n");
    for bar in bars {
        writeln!(
            out,
            "{},{},{},{},{},{}",
            bar.ts.timestamp(),
            bar.o.normalize(),
            bar.h.normalize(),
            bar.l.normalize(),
            bar.c.normalize(),
            bar.volume,
        )
        .expect("writing to a String cannot fail");
    }
    out
}

fn fixtures() -> &'static Fixtures {
    static FIXTURES: OnceLock<Fixtures> = OnceLock::new();
    FIXTURES.get_or_init(|| {
        let parquet = month_path();
        let bars = load_bars_parquet(&parquet).expect("committed month loads");
        assert!(!bars.is_empty(), "the fixture has bars");

        let dir = tempfile::tempdir().expect("bench tempdir");
        let csv = dir.path().join("SPY-2026-03-1M.csv");
        std::fs::write(&csv, render_csv(&bars)).expect("csv fixture writes");

        let tape_path = dir.path().join("tape.sqlite");
        let tape = SqliteTape::open(&tape_path).expect("bench tape opens");
        let caps = caps_for(&tape_path);
        let filled = gap_filled(&bars);
        let tape_bars = filled.len();
        fluent_wvr::CURRENT_CAPS.sync_scope(caps.clone(), || {
            let symbol_id = tape
                .register(
                    "SPY",
                    VENUE,
                    AssetClass::Stock,
                    PriceScale::of(3).expect("scale"),
                    "UTC",
                    Timeframe::M1,
                )
                .expect("bench market registers");
            let key = TapeKey {
                symbol_id,
                timeframe: Timeframe::M1,
            };
            let outcome = tape
                .append(key, &filled, WritePolicy::InsertIfAbsent)
                .expect("bench append");
            assert_eq!(
                outcome.rejected, 0,
                "the fixture scale represents every price"
            );
            assert_eq!(outcome.inserted, filled.len(), "every bench bar lands");
        });

        Fixtures {
            parquet,
            csv,
            tape,
            caps,
            bars,
            tape_bars,
            workdir: dir,
        }
    })
}

fn read_request(fixtures: &Fixtures) -> TapeReadRequest {
    TapeReadRequest {
        symbol: "SPY".to_owned(),
        venue: VENUE.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange {
            start_ms: None,
            end_ms: None,
        },
        limit: Some(fixtures.tape_bars),
        policy: FoldPolicy::ExtendToFullBuckets,
    }
}

fn load_parquet(c: &mut Criterion) {
    let fixtures = fixtures();
    let mut group = c.benchmark_group("load");
    group.sample_size(20);
    group.bench_function("parquet", |b| {
        b.iter(|| {
            let bars = load_bars_parquet(&fixtures.parquet).expect("loads");
            assert_eq!(bars.len(), fixtures.bars.len(), "same series, every arm");
        });
    });
    group.finish();
}

fn load_csv(c: &mut Criterion) {
    let fixtures = fixtures();
    let mut group = c.benchmark_group("load");
    group.sample_size(20);
    group.bench_function("csv", |b| {
        b.iter(|| {
            let bars = load_bars_csv(&fixtures.csv).expect("loads");
            assert_eq!(bars.len(), fixtures.bars.len(), "same series, every arm");
        });
    });
    group.finish();
}

fn load_sqlite(c: &mut Criterion) {
    let fixtures = fixtures();
    let request = read_request(fixtures);
    let mut group = c.benchmark_group("load");
    group.sample_size(20);
    group.bench_function("sqlite_tape", |b| {
        b.iter(|| {
            let series = fluent_wvr::CURRENT_CAPS.sync_scope(fixtures.caps.clone(), || {
                fixtures.tape.read(&request).expect("tape reads")
            });
            assert_eq!(
                series.bars.len(),
                fixtures.tape_bars,
                "the tape serves its gap-filled month"
            );
        });
    });
    group.finish();
}

const TARGETS: [Timeframe; 5] = [
    Timeframe::M5,
    Timeframe::M15,
    Timeframe::H1,
    Timeframe::H4,
    Timeframe::D1,
];

fn tally_parquet(c: &mut Criterion) {
    let fixtures = fixtures();
    let mut group = c.benchmark_group("tally");
    group.sample_size(20);
    group.bench_function("parquet_frames", |b| {
        b.iter(|| {
            let bars = load_bars_parquet(&fixtures.parquet).expect("loads");
            let frames = fold_frames(&bars, Timeframe::M1, &TARGETS).expect("folds");
            assert_eq!(frames.len(), TARGETS.len());
        });
    });
    group.finish();
}

fn tally_csv(c: &mut Criterion) {
    let fixtures = fixtures();
    let mut group = c.benchmark_group("tally");
    group.sample_size(20);
    group.bench_function("csv_frames", |b| {
        b.iter(|| {
            let bars = load_bars_csv(&fixtures.csv).expect("loads");
            let frames = fold_frames(&bars, Timeframe::M1, &TARGETS).expect("folds");
            assert_eq!(frames.len(), TARGETS.len());
        });
    });
    group.finish();
}

/// The storage-economy facts the timings are read against: committed bytes
/// per arm for the identical series.
fn report_bytes(fixtures: &Fixtures) {
    let bars = fixtures.bars.len() as f64;
    let parquet = std::fs::metadata(&fixtures.parquet)
        .expect("metadata")
        .len() as f64;
    let csv = std::fs::metadata(&fixtures.csv).expect("metadata").len() as f64;
    // WAL mode keeps fresh writes in the `-wal` sibling until a checkpoint:
    // the committed bytes are the pair.
    let db = fixtures.workdir.path().join("tape.sqlite");
    let sqlite = std::fs::metadata(&db).expect("metadata").len() as f64
        + std::fs::metadata(db.with_extension("sqlite-wal"))
            .map_or(0, |m| m.len()) as f64;
    println!(
        "history-load fixtures: {bars} vendor bars | parquet {parquet:.0} B ({:.1} B/bar) | \
         csv {csv:.0} B ({:.1} B/bar) | sqlite {sqlite:.0} B ({:.1} B/tape-bar over \
         {} gap-filled bars)",
        parquet / bars,
        csv / bars,
        sqlite / fixtures.tape_bars as f64,
        fixtures.tape_bars,
    );
}

criterion_group!(
    benches,
    load_parquet,
    load_csv,
    load_sqlite,
    tally_parquet,
    tally_csv
);

fn main() {
    report_bytes(fixtures());
    benches();
    Criterion::default().configure_from_args().final_summary();
}
