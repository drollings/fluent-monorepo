//! Shared multi-timeframe proof harness for the headless Pine suites.
//!
//! One home for the fixture paths, engine construction, injection datasets,
//! resampling, and closure linkage the IntegrationLite suite and the
//! task-indicator suite both run. Both suites call these helpers; neither
//! keeps a private copy (no second resampler, no second linker tail).
//!
//! Injection contract (mirrors the suite docstrings): `request.security`
//! is served from per-timeframe bar datasets registered under `(symbol,
//! abbrev)` with confirmed bars only (`lookahead_off`, `gaps_off` carry);
//! `request.security_lower_tf` is served from the registered minute window.
//! Anything unregistered refuses naming the live fetch.

//! Different suites use different subsets; dead code is shared harness, not drift.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use protopyne_kernels::manifest_files::load_pine_sources;
use protopyne_kernels::{
    closure_data_root, closure_inputs, discover_closures, resolve_imports, ClosureOutcome,
    LinkedModule, LtfDataset, ModuleGraph, ModuleInput, PineEngine, PineRuntimeConfig, PineValue,
    RunOutcome, UserInstance,
};
use protopyne_types::{Bar, Timeframe};
use rust_decimal::Decimal;

/// Warmup bars skipped before the first evaluated bar. Zero: Pine
/// evaluates from bar 0 with `na` kernel warmth, and `barstate.isfirst`
/// init blocks require the true first bar (under a nonzero warmup
/// `isfirst` never fires, so init-gated scripts could not initialize).
/// Early-bar `na` values are genuine Pine warmth, deterministic and
/// pinned like everything else.
pub const WARMUP: u64 = 0;

pub fn data_root() -> PathBuf {
    closure_data_root(env!("CARGO_MANIFEST_DIR"))
}

pub fn history_path(file: &str) -> PathBuf {
    data_root().join("historical").join(file)
}

pub fn load_history(file: &str) -> Vec<Bar> {
    crate::csv::load_bars_csv(&history_path(file)).expect("history loads")
}

pub fn engine_for(period: &str) -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period(period.to_owned())
            .warmup_bars(WARMUP)
            .build(),
    )
}

/// One injection dataset from bars of one timeframe (bucket opens, per-row
/// session-aware closes, volumes as exact decimals where integral).
/// Shared constructor: the kernels harness builds the same datasets from
/// the same bars through `LtfDataset :: of_bars`, so the two suites cannot
/// drift into incompatible conversions.
pub fn dataset_of(bars: &[Bar], timeframe: Timeframe) -> LtfDataset {
    LtfDataset::of_bars(bars, timeframe)
}

/// Resample bars into floor-aligned buckets of `bucket_ms` (first open,
/// high/low extremes, last close, summed volume), for timeframes with no
/// CSV of their own. Bucket floors carry the bucket-start timestamp.
pub fn resample(bars: &[Bar], bucket_ms: i64) -> Vec<Bar> {
    let mut buckets: HashMap<i64, Vec<&Bar>> = HashMap::new();
    for bar in bars {
        buckets
            .entry(bar.ts.timestamp_millis().div_euclid(bucket_ms))
            .or_default()
            .push(bar);
    }
    let mut keys: Vec<i64> = buckets.keys().copied().collect();
    keys.sort_unstable();
    keys.into_iter()
        .map(|key| {
            let mut rows = buckets.remove(&key).expect("bucket present");
            rows.sort_by_key(|b| b.ts);
            let first = rows.first().expect("nonempty bucket");
            let last = rows.last().expect("nonempty bucket");
            Bar::new()
                .ts(DateTime::from_timestamp_millis(key * bucket_ms)
                    .expect("bucket start in range"))
                .o(first.o)
                .h(rows.iter().map(|b| b.h).max().expect("nonempty bucket"))
                .l(rows.iter().map(|b| b.l).min().expect("nonempty bucket"))
                .c(last.c)
                .volume(rows.iter().map(|b| b.volume).sum())
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// Register every dataset the IntegrationLite indicator path may observe:
/// minute bars for the tick-engine pull path, plus one per-timeframe
/// dataset for the MTF market contexts (`H1`/`H4` resampled, see
/// [`resample`]).
pub fn inject_all(engine: &mut PineEngine, minute: &[Bar], m5: &[Bar], m15: &[Bar]) {
    engine
        .inject_lower_tf("SPY", "1", dataset_of(minute, Timeframe::M1))
        .expect("minute window injects");
    engine
        .inject_security("SPY", "5", dataset_of(m5, Timeframe::M5))
        .expect("M5 injects");
    engine
        .inject_security("SPY", "15", dataset_of(m15, Timeframe::M15))
        .expect("M15 injects");
    let h1 = resample(m15, 60 * 60 * 1000);
    let h4 = resample(m15, 4 * 60 * 60 * 1000);
    assert!(
        !h1.is_empty() && !h4.is_empty(),
        "resampling covers the tape"
    );
    engine
        .inject_security("SPY", "60", dataset_of(&h1, Timeframe::H1))
        .expect("H1 injects");
    engine
        .inject_security("SPY", "240", dataset_of(&h4, Timeframe::H4))
        .expect("H4 injects");
}

/// Resolve linked inputs into a dependency-first module graph.
pub fn resolve_inputs(inputs: Vec<ModuleInput>) -> ModuleGraph {
    resolve_imports(inputs).expect("modules resolve")
}

/// Map an ordered module graph to the alias tables linked execution runs.
pub fn linked_from_graph(graph: &ModuleGraph) -> Vec<LinkedModule> {
    graph
        .order()
        .iter()
        .map(|id| {
            let entry = graph.get(id).expect("ordered module resolves");
            let aliases = graph
                .alias_table(id)
                .unwrap_or_default()
                .into_iter()
                .map(|(alias, target)| (alias, target.to_string()))
                .collect::<HashMap<String, String>>();
            LinkedModule {
                key: id.to_string(),
                program: entry.program.clone(),
                aliases,
            }
        })
        .collect()
}

/// Checked-in fixture root for Pine closures (`tests/data/pine_closure`,
/// same layout as the live data root). Fixture bytes are hermetic; the
/// volatile live root is never read for Pine sources here.
pub fn fixture_closure_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/pine_closure")
}

/// One fixture indicator closure linked dependency-first: the same loader,
/// parser, and linker the closure suites use, mapped to alias tables for
/// runs. `stem` is the fixture indicator stem. Versions ride each file's
/// own header (metadata only, never enforced): there is exactly one
/// on-disk source per publisher/name, and cross-version member drift
/// refuses fail-closed instead of becoming a shim.
pub fn link_fixture_closure(stem: &str) -> Vec<LinkedModule> {
    let (indicators, libraries) =
        load_pine_sources(&fixture_closure_root()).expect("fixtures load");
    let closure = discover_closures(&indicators, &libraries)
        .into_iter()
        .find_map(|outcome| match outcome {
            ClosureOutcome::Linked(closure) if closure.stem == stem => Some(closure),
            _ => None,
        })
        .unwrap_or_else(|| panic!("fixture closure {stem} discovered"));
    let inputs = closure_inputs(&closure)
        .unwrap_or_else(|err| panic!("fixture closure {stem} parses: {err}"));
    linked_from_graph(&resolve_inputs(inputs))
}

/// One discovered fixture closure's parsed inputs, for suites that link
/// their own driver root against the fixture libraries.
pub fn fixture_closure_inputs(stem: &str) -> Vec<(String, protopyne_kernels::PineSource)> {
    let (indicators, libraries) =
        load_pine_sources(&fixture_closure_root()).expect("fixtures load");
    discover_closures(&indicators, &libraries)
        .into_iter()
        .find_map(|outcome| match outcome {
            ClosureOutcome::Linked(closure) if closure.stem == stem => Some(closure.sources),
            _ => None,
        })
        .unwrap_or_else(|| panic!("fixture closure {stem} discovered"))
}

pub fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> f64 {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Float(value))) => value,
        Some(Some(PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name} on bar {at_bar}, got {other:?}"),
    }
}

pub fn present(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    outcome.value_at(at_bar, name).is_some()
}

/// One tick update: legs track bar-so-far OHLC plus the rounded cumulative
/// volume over the constant bar-open time; the tick stamp rides the side
/// channel. Cumulative rounding keeps the total error under one share.
pub fn tick_update(
    bar_open: DateTime<Utc>,
    tick_ms: i64,
    open: f64,
    high: f64,
    low: f64,
    price: f64,
    cumvol: u64,
) -> (Bar, i64) {
    let fmt = |v: f64| format!("{v:.2}");
    let decimal = |text: String| text.parse::<Decimal>().expect("decimal");
    (
        Bar::new()
            .ts(bar_open)
            .o(decimal(fmt(open)))
            .h(decimal(fmt(high)))
            .l(decimal(fmt(low)))
            .c(decimal(fmt(price)))
            .volume(cumvol)
            .maybe_adj(None)
            .build(),
        tick_ms,
    )
}

/// One minute's tape legs: the bucket-open milliseconds, the rounded
/// cumulative leg per tick, and the tick index of the bucket's last tick
/// (the confirming tick of an isolated per-minute run).
pub struct TickBucket {
    pub open_ms: i64,
    pub legs: Vec<f64>,
    pub last_at: usize,
}

/// Group tape rows `(stamp_ms, price, volume)` into minute buckets with
/// running bar-so-far legs: minute-open/high/low track prices, legs carry
/// the rounded cumulative volume. Shared by the tick proofs so leg
/// geometry cannot drift between suites.
pub fn build_tick_legs(rows: &[(i64, f64, f64)]) -> (Vec<Bar>, Vec<i64>, Vec<TickBucket>) {
    let mut ticks: Vec<Bar> = Vec::with_capacity(rows.len());
    let mut stamps: Vec<i64> = Vec::with_capacity(rows.len());
    let mut buckets: Vec<TickBucket> = Vec::new();
    let mut minute_open = 0.0;
    let mut minute_high = f64::NEG_INFINITY;
    let mut minute_low = f64::INFINITY;
    let mut minute_cum = 0.0;
    let mut minute_open_ms = 0i64;
    for (stamp, price, volume) in rows {
        let open_ms = stamp.div_euclid(60_000) * 60_000;
        if open_ms != minute_open_ms {
            minute_open_ms = open_ms;
            minute_open = *price;
            minute_high = f64::NEG_INFINITY;
            minute_low = f64::INFINITY;
            minute_cum = 0.0;
        }
        minute_high = minute_high.max(*price);
        minute_low = minute_low.min(*price);
        minute_cum += *volume;
        let bar_open = DateTime::from_timestamp_millis(open_ms).expect("bar-open stamp in range");
        let leg = minute_cum.round();
        let (bar, tick_ms) = tick_update(
            bar_open,
            *stamp,
            minute_open,
            minute_high,
            minute_low,
            *price,
            leg as u64,
        );
        let at = ticks.len();
        ticks.push(bar);
        stamps.push(tick_ms);
        match buckets.last_mut() {
            Some(bucket) if bucket.open_ms == open_ms => {
                bucket.legs.push(leg);
                bucket.last_at = at;
            }
            _ => buckets.push(TickBucket {
                open_ms,
                legs: vec![leg],
                last_at: at,
            }),
        }
    }
    (ticks, stamps, buckets)
}

/// The `bars` object snapshot at one evaluated tick: the accumulator under
/// proof, read off the snapshot (never driver-addressable fields).
pub fn bars_at(outcome: &RunOutcome, bar: usize) -> UserInstance {
    match outcome.value_at(bar, "bars") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected Bars object for bars on tick {bar}, got {other:?}"),
    }
}

pub fn field_num(instance: &UserInstance, name: &str) -> f64 {
    match instance.fields.get(name) {
        Some(Some(PineValue::Float(value))) => *value,
        Some(Some(PineValue::Int(value))) => *value as f64,
        other => panic!("expected number field {name}, got {other:?}"),
    }
}

/// Tick-tape reader over an explicit path (same column pin as the shared
/// tape): trimmed windows live beside their suite, not under the shared
/// historical root, so callers pass a manifest-relative path and pin the
/// row count at the call site.
pub fn load_tick_rows_from(path: &Path) -> Vec<(i64, f64, f64)> {
    let content = std::fs::read_to_string(path).expect("ticks load");
    let mut lines = content.lines();
    let header = lines.next().expect("ticks header");
    assert_eq!(
        header, "timestamp_ms,price,volume,is_out_of_order,original_timestamp_ms",
        "tick columns pinned"
    );
    lines
        .map(|line| {
            let mut cells = line.split(',');
            let stamp = cells
                .next()
                .expect("stamp")
                .parse::<i64>()
                .expect("int stamp");
            let price = cells.next().expect("price").parse::<f64>().expect("price");
            let volume = cells
                .next()
                .expect("volume")
                .parse::<f64>()
                .expect("volume");
            (stamp, price, volume)
        })
        .collect()
}
