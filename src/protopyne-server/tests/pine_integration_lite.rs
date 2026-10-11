//! Headless end-to-end proof for the shipped `_IntegrationLite` indicator.
//!
//! The seven libraries plus the indicator link through `resolve_imports`
//! and run bar-by-bar through `PineEngine :: run_linked` over the real
//! histories, with every multi-timeframe number entering through explicit
//! injection — never a live fetch. Two passes compose the proof:
//!
//! * Closed-bar indicator runs over `SPY_1M.csv` (primary), `SPY_5M.csv`,
//!   and `SPY_15M.csv` (alternate chart timeframes). Each run pins the
//!   evaluated-bar count, the ten indicator snapshot series, the drawing
//!   captures, and rerun byte-identity.
//! * A full-tape tick run feeds all of `SPY_ticks.csv` into `Bars ::
//!   fetch` (a Bars-only driver over the same closure) and reconciles the
//!   reconstructed minute volumes against `SPY_1M.csv`.
//!
//! Headless injection contract (what `request.*` would do live vs what the
//! harness injects):
//!
//! * `request.security(sym, tf, exprs, gaps_off, lookahead_off,
//!   calc_bars_count)` is served from per-timeframe bar datasets
//!   registered under `(symbol, abbrev)`. Only confirmed bars count: rows
//!   with timestamps after the current chart bar are invisible
//!   (`lookahead_off`), and the last confirmed value carries forward
//!   (`gaps_off`). Any other merge mode, or an unregistered pair, refuses
//!   with a reason — the live-fetch refusal stays for everything the
//!   harness does not inject.
//! * `request.security_lower_tf` is served from the registered minute
//!   window (static, per the tick-engine design: the harness registers
//!   exactly the window a pull may observe).
//! * `M5`/`M15` contexts inject from their CSVs; `H1`/`H4` have no CSV, so
//!   the harness resamples the 15-minute bars into hour (4x15m) and
//!   four-hour (16x15m) floor buckets with first-open / high / low /
//!   last-close / summed-volume aggregation. The resampling is
//!   deterministic and documented here, not hidden in a fixture.
//!
//! Closed-bar assumption (from the platform vision): every evaluated bar is
//! a confirmed history bar, so `barstate.isnew/isconfirmed/ishistory` hold
//! on each bar and the confirmation-gated calculations run per bar.
//!
//! Tick-harness contract (the full-tape run below, and the trimmed windows
//! in the kernels `libbars` suite — same rules, smaller fixtures):
//!
//! * Legs: per tape row, bar-so-far OHLC with the rounded cumulative
//!   volume over a constant bar-open time; tick stamps ride the side
//!   channel (`run_linked_ticks`). Volumes are positive, so cumulative
//!   legs never decrease and the rollback path stays quiet on real tape.
//! * Flags: a bar-open change clears (`isnew`); the run's final tick
//!   confirms, so `onConfirm` always runs once per window with `ctf_vol`
//!   equal to the last leg. Seeding absorbs the first two ticks without
//!   accumulating, hence drift is exactly the seeding loss over the tape
//!   total and the accumulator telescopes to last-minus-second leg.
//! * Pulls: drift over the 4% gate rebuilds the ring oldest-first from
//!   the confirmed injection prefix (capacity wins) plus the remainder
//!   only when the ring has room; the remainder adds by magnitude (the
//!   abs-remainder rule), so pulled totals are window sums on deep
//!   windows. Unregistered pairs refuse like live fetch.
//! * Recorded baselines: trimmed windows plus the soak bound live in the
//!   kernels `tests/data/tick_harness_baselines.json` with their bounds;
//!   the full contract (per-minute recompute, both regimes non-empty) is
//!   asserted below, aggregates only in the soak.
//!
//! Goldens: `tests/data/pine_integration_lite_<tf>.json` holds the full
//! canonical dump of one recorded run (snapshots of the ten indicator
//! series plus every capture). Set `PROTOPYNE_RECORD=1` to regenerate
//! after an intentional change; otherwise the files compare within
//! `1e-9`, and any drift fails loudly instead of silently moving.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use protopyne_kernels::{
    BarSnapshot, EnumHandle, InputSet, LinkedModule, LtfDataset, ModuleInput, PineEngine,
    PineRuntimeConfig, PineValue, RunOutcome, closure_data_root, indicator_library_id,
    load_closure_sources, parse_program, resolve_imports,
};
use protopyne_types::Bar;
use rust_decimal::Decimal;

/// Warmup bars skipped before the first evaluated bar. Zero: Pine
/// evaluates from bar 0 with `na` kernel warmth, and the indicator's
/// `barstate.isfirst` init block requires the true first bar (under a
/// nonzero warmup `isfirst` never fires, so init-gated scripts could not
/// initialize). Early-bar `na` values are genuine Pine behavior —
/// deterministic and pinned like everything else.
const WARMUP: u64 = 0;

/// Snapshot series the proof pins on confirmed bars.
const PINNED_SERIES: &[&str] = &[
    "_eit_adaptive",
    "_ribbon_signal",
    "_ribbon_mean",
    "ribbon_health",
    "_osc1_fast",
    "_osc1_slow",
    "alpha",
    "color_candle_trend",
    "dvc_buy_top",
    "dvc_sell_bot",
];

fn data_root() -> PathBuf {
    closure_data_root(env!("CARGO_MANIFEST_DIR"))
}

fn history_path(file: &str) -> PathBuf {
    data_root().join("historical").join(file)
}

fn golden_path(timeframe: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(format!("pine_integration_lite_{timeframe}.json"))
}

fn load_history(file: &str) -> Vec<Bar> {
    crate::csv::load_bars_csv(&history_path(file)).expect("history loads")
}

/// The real eight-module closure (seven libraries plus the indicator root)
/// linked dependency-first, exactly like the loader hands the engine.
fn linked_closure() -> Vec<LinkedModule> {
    let inputs: Vec<ModuleInput> = load_closure_sources(&data_root())
        .expect("closure loads")
        .into_iter()
        .map(|(key, source)| {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("real file {key} parses: {err}"));
            let id = if key == "local/IntegrationLite" {
                indicator_library_id()
            } else {
                let (publisher, name) = key.split_once('/').expect("key shape");
                protopyne_kernels::LibraryId {
                    publisher: publisher.to_owned(),
                    name: name.to_owned(),
                    version: 1,
                }
            };
            ModuleInput::new(id, source, program)
        })
        .collect();
    let graph = resolve_imports(inputs).expect("closure resolves");
    assert!(graph.warnings().is_empty(), "clean v6 closure warns nothing");
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

/// Link the seven shipped libraries with one driver root calling
/// through the same aliases the indicator uses.
fn link_driver(driver: &str) -> Vec<LinkedModule> {
    let mut inputs: Vec<ModuleInput> = load_closure_sources(&data_root())
        .expect("closure loads")
        .into_iter()
        .filter(|(key, _)| key != "local/IntegrationLite")
        .map(|(key, source)| {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("real file {key} parses: {err}"));
            let (publisher, name) = key.split_once('/').expect("key shape");
            let id = protopyne_kernels::LibraryId {
                publisher: publisher.to_owned(),
                name: name.to_owned(),
                version: 1,
            };
            ModuleInput::new(id, source, program)
        })
        .collect();
    let source = protopyne_kernels::parse_pine_source("local/Driver", driver);
    let program = parse_program(driver).expect("driver parses");
    inputs.push(ModuleInput::new(
        protopyne_kernels::LibraryId {
            publisher: "local".to_owned(),
            name: "Driver".to_owned(),
            version: 1,
        },
        source,
        program,
    ));
    let graph = resolve_imports(inputs).expect("libraries plus driver resolve");
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

fn engine_for(period: &str) -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period(period.to_owned())
            .warmup_bars(WARMUP)
            .build(),
    )
}

/// One injection dataset from bars (millisecond timestamps, matching the
/// dataset convention; volumes ride as exact decimals where integral).
fn dataset_of(bars: &[Bar]) -> LtfDataset {
    let decimal = |d: Decimal| d.to_string().parse::<f64>().unwrap_or(f64::NAN);
    LtfDataset {
        times: bars.iter().map(|b| b.ts.timestamp_millis()).collect(),
        opens: bars.iter().map(|b| decimal(b.o)).collect(),
        highs: bars.iter().map(|b| decimal(b.h)).collect(),
        lows: bars.iter().map(|b| decimal(b.l)).collect(),
        closes: bars.iter().map(|b| decimal(b.c)).collect(),
        volumes: bars.iter().map(|b| decimal(Decimal::from(b.volume))).collect(),
    }
}

/// Resample bars into floor-aligned buckets of `bucket_ms` (first open,
/// high/low extremes, last close, summed volume), for timeframes with no
/// CSV of their own. Bucket floors carry the bucket-start timestamp.
fn resample(bars: &[Bar], bucket_ms: i64) -> Vec<Bar> {
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
                .ts(
                    DateTime::from_timestamp_millis(key * bucket_ms)
                        .expect("bucket start in range"),
                )
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

/// Register every dataset the indicator path may observe: minute bars for
/// the tick-engine pull path, plus one per-timeframe dataset for the MTF
/// market contexts (`H1`/`H4` resampled, see [`resample`]).
fn inject_all(engine: &mut PineEngine, minute: &[Bar], m5: &[Bar], m15: &[Bar]) {
    engine
        .inject_lower_tf("SPY", "1", dataset_of(minute))
        .expect("minute window injects");
    engine
        .inject_security("SPY", "5", dataset_of(m5))
        .expect("M5 injects");
    engine
        .inject_security("SPY", "15", dataset_of(m15))
        .expect("M15 injects");
    let h1 = resample(m15, 60 * 60 * 1000);
    let h4 = resample(m15, 4 * 60 * 60 * 1000);
    assert!(!h1.is_empty() && !h4.is_empty(), "resampling covers the tape");
    engine
        .inject_security("SPY", "60", dataset_of(&h1))
        .expect("H1 injects");
    engine
        .inject_security("SPY", "240", dataset_of(&h4))
        .expect("H4 injects");
}

fn run_chart(modules: &[LinkedModule], period: &str, bars: &[Bar]) -> RunOutcome {
    let minutes = load_history("SPY_1M.csv");
    let m5 = load_history("SPY_5M.csv");
    let m15 = load_history("SPY_15M.csv");
    let mut engine = engine_for(period);
    inject_all(&mut engine, &minutes, &m5, &m15);
    engine
        .run_linked(modules, bars, InputSet::new())
        .expect("linked indicator run evaluates with zero refusals")
}

fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> f64 {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Float(value))) => value,
        Some(Some(PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn present(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    outcome.value_at(at_bar, name).is_some()
}

/// Deterministic JSON for one script value (`na` and non-finite floats
/// both read `null`; maps and objects sort by key). Takes the reference
/// form because every call site holds a borrowed slot.
#[allow(clippy::ref_option)]
fn dump_value(value: &Option<PineValue>) -> serde_json::Value {
    match value {
        None => serde_json::Value::Null,
        Some(PineValue::Float(v)) => serde_json::Number::from_f64(*v)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Some(PineValue::Int(v)) => serde_json::Value::Number((*v).into()),
        Some(PineValue::Bool(v)) => serde_json::Value::Bool(*v),
        Some(PineValue::Str(v)) => serde_json::Value::String(v.clone()),
        Some(PineValue::ColorHandle(v)) => serde_json::Value::Number(v.raw().into()),
        Some(PineValue::EnumHandle(EnumHandle {
            enum_name,
            member,
        })) => serde_json::Value::String(format!("{enum_name}.{member}")),
        Some(PineValue::Array(items)) => {
            serde_json::Value::Array(items.iter().map(dump_value).collect())
        }
        Some(PineValue::IntMap(entries)) => {
            let mut keys: Vec<i64> = entries.keys().copied().collect();
            keys.sort_unstable();
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|k| {
                        (
                            k.to_string(),
                            serde_json::Value::Number(entries[&k].into()),
                        )
                    })
                    .collect(),
            )
        }
        Some(PineValue::Map(entries)) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, (_, value))| (key.clone(), dump_value(value)))
                .collect(),
        ),
        Some(PineValue::Object(instance)) => {
            let mut keys: Vec<&String> = instance.fields.keys().collect();
            keys.sort_unstable();
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), dump_value(&instance.fields[k])))
                    .collect(),
            )
        }
        Some(PineValue::Drawing(handle)) => {
            serde_json::Value::String(format!("{:?}#{}", handle.kind, handle.id))
        }
        Some(PineValue::Matrix(grid)) => serde_json::json!({
            "rows": grid.rows,
            "cols": grid.cols,
            "cells": grid.cells.iter().map(dump_value).collect::<Vec<_>>(),
        }),
    }
}

/// Canonical dump of snapshots plus every capture: key-sorted JSON whose
/// text form is byte-identical across reruns of identical inputs.
fn dump_canonical(outcome: &RunOutcome) -> String {
    let snapshots: Vec<serde_json::Value> = outcome
        .snapshots
        .iter()
        .map(|BarSnapshot { bar, values }| {
            let mut keys: Vec<&String> = values.keys().collect();
            keys.sort_unstable();
            serde_json::json!({
                "bar": bar,
                "values": keys
                    .into_iter()
                    .map(|k| (k.clone(), dump_value(&values[k])))
                    .collect::<serde_json::Map<_, _>>(),
            })
        })
        .collect();
    let plots: Vec<serde_json::Value> = outcome
        .plots
        .iter()
        .map(|plot| {
            serde_json::json!({
                "title": plot.title,
                "display": dump_value(&plot.display),
                "values": plot.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let candles: Vec<serde_json::Value> = outcome
        .plotcandles
        .iter()
        .map(|candle| {
            serde_json::json!({
                "title": candle.title,
                "display": dump_value(&candle.display),
                "color": dump_value(&candle.color),
                "wickcolor": dump_value(&candle.wickcolor),
                "bordercolor": dump_value(&candle.bordercolor),
                "opens": candle.opens.iter().map(dump_value).collect::<Vec<_>>(),
                "highs": candle.highs.iter().map(dump_value).collect::<Vec<_>>(),
                "lows": candle.lows.iter().map(dump_value).collect::<Vec<_>>(),
                "closes": candle.closes.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let barcolors: Vec<serde_json::Value> = outcome
        .barcolors
        .iter()
        .map(|paint| {
            serde_json::json!({
                "title": paint.title,
                "values": paint.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let fills: Vec<serde_json::Value> = outcome
        .fills
        .iter()
        .map(|fill| {
            serde_json::json!({
                "title": fill.title,
                "first": fill.first,
                "second": fill.second,
                "first_title": fill.first_title,
                "second_title": fill.second_title,
                "color": dump_value(&fill.color),
                "values": fill.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let events: Vec<serde_json::Value> = outcome
        .draw_events
        .iter()
        .map(|event| {
            serde_json::json!({
                "bar": event.bar,
                "kind": format!("{:?}", event.kind),
                "object": event.object,
                "key": event.key,
            })
        })
        .collect();
    serde_json::json!({
        "evaluated": outcome.evaluated,
        "snapshots": snapshots,
        "plots": plots,
        "plotcandles": candles,
        "barcolors": barcolors,
        "fills": fills,
        "draw_events": events,
    })
    .to_string()
}

/// Slim golden dump: the pinned series over the full run plus every
/// capture. Snapshots ride the in-memory determinism check instead —
/// they carry whole engine state (bars, contexts, scheduler maps) that
/// would bloat a checked-in file without adding proof value.
fn dump_slim(outcome: &RunOutcome) -> String {
    let series: serde_json::Map<String, serde_json::Value> = PINNED_SERIES
        .iter()
        .map(|name| {
            (
                name.to_string(),
                serde_json::Value::Array(
                    outcome
                        .snapshots
                        .iter()
                        .map(|snapshot| {
                            snapshot
                                .values
                                .get(*name)
                                .map_or(serde_json::Value::Null, dump_value)
                        })
                        .collect(),
                ),
            )
        })
        .collect();
    let plots: Vec<serde_json::Value> = outcome
        .plots
        .iter()
        .map(|plot| {
            serde_json::json!({
                "title": plot.title,
                "display": dump_value(&plot.display),
                "values": plot.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let candles: Vec<serde_json::Value> = outcome
        .plotcandles
        .iter()
        .map(|candle| {
            serde_json::json!({
                "title": candle.title,
                "display": dump_value(&candle.display),
                "color": dump_value(&candle.color),
                "wickcolor": dump_value(&candle.wickcolor),
                "bordercolor": dump_value(&candle.bordercolor),
                "opens": candle.opens.iter().map(dump_value).collect::<Vec<_>>(),
                "highs": candle.highs.iter().map(dump_value).collect::<Vec<_>>(),
                "lows": candle.lows.iter().map(dump_value).collect::<Vec<_>>(),
                "closes": candle.closes.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let barcolors: Vec<serde_json::Value> = outcome
        .barcolors
        .iter()
        .map(|paint| {
            serde_json::json!({
                "title": paint.title,
                "values": paint.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let fills: Vec<serde_json::Value> = outcome
        .fills
        .iter()
        .map(|fill| {
            serde_json::json!({
                "title": fill.title,
                "first": fill.first,
                "second": fill.second,
                "first_title": fill.first_title,
                "second_title": fill.second_title,
                "color": dump_value(&fill.color),
                "values": fill.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let events: Vec<serde_json::Value> = outcome
        .draw_events
        .iter()
        .map(|event| {
            serde_json::json!({
                "bar": event.bar,
                "kind": format!("{:?}", event.kind),
                "object": event.object,
                "key": event.key,
            })
        })
        .collect();
    serde_json::json!({
        "evaluated": outcome.evaluated,
        "series": series,
        "plots": plots,
        "plotcandles": candles,
        "barcolors": barcolors,
        "fills": fills,
        "draw_events": events,
    })
    .to_string()
}
/// Record or compare the checked-in golden for one chart timeframe. With
/// `PROTOPYNE_RECORD=1` the file regenerates and the test passes
/// trivially; otherwise the slim dump must match textually (exact
/// float text, hence within kernel tolerance by construction).
fn check_golden(timeframe: &str, outcome: &RunOutcome) {
    let path = golden_path(timeframe);
    let dump = dump_slim(outcome);
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::write(&path, &dump).expect("golden records");
        return;
    }
    let recorded = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("golden {} records first", path.display()));
    assert_eq!(dump, recorded, "rerun matches the checked-in golden");
}

/// Structural proof shared by all three chart timeframes: bar count,
/// snapshot series, captures, session/pivot branches, and determinism.
fn prove_chart(modules: &[LinkedModule], period: &str, bars: &[Bar]) -> RunOutcome {
    let outcome = run_chart(modules, period, bars);
    let expect_evaluated = bars.len() - WARMUP as usize;
    assert_eq!(
        outcome.evaluated, expect_evaluated,
        "every non-warmup bar evaluates"
    );
    assert_eq!(
        outcome.snapshots.len(),
        expect_evaluated,
        "one snapshot per evaluated bar"
    );
    for name in PINNED_SERIES {
        // Bar 100: every kernel is warm (`atr(50)` needs 50 bars), so each
        // series binds a value. Bar 0 may read `na` (zero-initialized
        // fields divided before their first fetch) — genuine Pine warmth,
        // pinned in the golden, never asserted finite here.
        assert!(
            present(&outcome, 100, name),
            "snapshot carries {name} once kernels warm"
        );
        assert!(
            present(&outcome, expect_evaluated - 1, name),
            "snapshot carries {name} on the last evaluated bar"
        );
    }
    assert!(
        outcome.fills.iter().any(|fill| fill.title == "Trend Ribbon"),
        "ribbon fill pair captured"
    );
    for title in ["Ribbon Health Osc", "MA Health Osc", "Osc Fast", "Osc Slow"] {
        assert!(
            outcome.plots.iter().any(|plot| plot.title == title),
            "{title} plot captured"
        );
    }
    for title in ["Delta Buy Volume", "Delta Sell Volume"] {
        assert!(
            outcome
                .plotcandles
                .iter()
                .any(|candle| candle.title == title),
            "{title} plotcandle captured"
        );
    }
    assert!(!outcome.barcolors.is_empty(), "barcolor captured");
    // The session filter runs (the `time()` call resolves, never `na`) and
    // the pivot path records labels exactly when threshold crossings occur
    // in-fixture; absence of crossings asserts explicitly, never vacuously.
    let labels: Vec<_> = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "label")
        .collect();
    let opt_num = |at: usize| match outcome.value_at(at, "_osc1_fast") {
        Some(Some(PineValue::Float(v))) => Some(v),
        Some(Some(PineValue::Int(v))) => Some(v as f64),
        _ => None,
    };
    let fast_crosses = (1..expect_evaluated).any(|at| {
        matches!(
            (opt_num(at - 1), opt_num(at)),
            (Some(prev), Some(now))
                if (prev >= 200.0 && now < 200.0) || (prev <= -200.0 && now > -200.0)
        )
    });
    if fast_crosses {
        assert!(!labels.is_empty(), "threshold crossings record pivot labels");
    } else {
        assert!(
            labels.is_empty(),
            "no crossings in-fixture, so no pivot labels — asserted, not assumed"
        );
    }
    // Multi-timeframe contexts fetched through injection: first-bar init
    // skips the chart timeframe, and every other context carries its
    // fetched timeframe (never the chart placeholder) once the scheduler
    // gate fires. Expected members per chart timeframe.
    let expect_mtf: &[&str] = match period {
        "1" => &["M5", "M15", "H1", "H4"],
        "5" => &["M15", "H1", "H4"],
        "15" => &["M5", "H1", "H4"],
        _ => panic!("chart period pins its MTF set"),
    };
    let mtf = match outcome.value_at(expect_evaluated - 1, "mtf_context") {
        Some(Some(PineValue::Map(entries))) => entries,
        other => panic!("mtf map on the last bar, got {other:?}"),
    };
    assert_eq!(mtf.len(), expect_mtf.len(), "one context per MTF member");
    for member in expect_mtf.iter().copied() {
        let wanted = PineValue::EnumHandle(protopyne_kernels::EnumHandle::new(
            "Timeframes",
            member,
        ));
        assert!(
            mtf.values().any(|(_, value)| {
                matches!(value, Some(PineValue::Object(instance))
                    if instance.fields.get("tf") == Some(&Some(wanted.clone())))
            }),
            "{member} context fetched"
        );
    }
    // The MTF table builds only when the scheduler gate is due on the
    // last bar. The seconds-stretched schedule (engine `time` runs in
    // whole seconds against the library's millisecond intervals) rarely
    // lands on `islast`, so both outcomes assert concretely: due means
    // table events exist; not-due means the next slot still lies past the
    // last bar (proven, not assumed) with no table events.
    let sched = match outcome.value_at(expect_evaluated - 1, "scheduler") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("scheduler object on the last bar, got {other:?}"),
    };
    let next_due = match sched.fields.get("next_update").cloned().flatten() {
        Some(PineValue::IntMap(entries)) => entries
            .get(&180_000)
            .copied()
            .expect("M3 frame scheduled"),
        other => panic!("scheduler holds its map, got {other:?}"),
    };
    let last_ts = bars[expect_evaluated - 1].ts.timestamp();
    let tables = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "table")
        .count();
    if next_due <= last_ts {
        assert!(tables > 0, "due gate builds the MTF table on islast");
    } else {
        assert_eq!(
            tables, 0,
            "not-due gate builds no table — asserted with the slot proof"
        );
    }
    // Determinism: an identical second run is byte-identical.
    let again = run_chart(modules, period, bars);
    assert_eq!(
        dump_canonical(&again),
        dump_canonical(&outcome),
        "rerun over identical inputs is byte-identical"
    );
    outcome
}

#[test]
fn integration_lite_runs_clean_on_1m() {
    let modules = linked_closure();
    let bars = load_history("SPY_1M.csv");
    assert_eq!(bars.len(), 5311, "1M fixture row count pinned");
    let outcome = prove_chart(&modules, "1", &bars);
    // Late-bar finiteness: every kernel is warm by the last bar, so a
    // stuck-`na` (a masked refusal or a dead update path) fails here.
    // Early-bar `na` is genuine Pine warmth and stays pinned in the
    // golden, never asserted finite.
    let last = outcome.evaluated - 1;
    for name in [
        "_eit_adaptive",
        "_ribbon_signal",
        "_ribbon_mean",
        "ribbon_health",
        "_osc1_fast",
        "_osc1_slow",
        "alpha",
        "dvc_buy_top",
        "dvc_sell_bot",
    ] {
        assert!(
            num(&outcome, last, name).is_finite(),
            "{name} is finite on the last bar"
        );
    }
    check_golden("1m", &outcome);
}

#[test]
fn integration_lite_runs_clean_on_5m() {
    let modules = linked_closure();
    let bars = load_history("SPY_5M.csv");
    assert_eq!(bars.len(), 1149, "5M fixture row count pinned");
    let outcome = prove_chart(&modules, "5", &bars);
    check_golden("5m", &outcome);
}

#[test]
fn integration_lite_runs_clean_on_15m() {
    let modules = linked_closure();
    let bars = load_history("SPY_15M.csv");
    assert_eq!(bars.len(), 384, "15M fixture row count pinned");
    let outcome = prove_chart(&modules, "15", &bars);
    check_golden("15m", &outcome);
}

/// One tick update: legs track bar-so-far OHLC plus the rounded cumulative
/// volume over the constant bar-open time; the tick stamp rides the side
/// channel. Cumulative rounding keeps the total error under one share.
fn tick_update(
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

fn bars_at(outcome: &RunOutcome, bar: usize) -> protopyne_kernels::UserInstance {
    match outcome.value_at(bar, "bars") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected Bars object for bars on tick {bar}, got {other:?}"),
    }
}

fn field_num(instance: &protopyne_kernels::UserInstance, name: &str) -> f64 {
    match instance.fields.get(name) {
        Some(Some(PineValue::Float(value))) => *value,
        Some(Some(PineValue::Int(value))) => *value as f64,
        other => panic!("expected number field {name}, got {other:?}"),
    }
}

/// Full-tape tick proof: every `SPY_ticks.csv` row feeds `Bars :: fetch`
/// through the linked closure (indicator root included, so its MTF market
/// contexts fetch through the same injected datasets as the chart runs),
/// and each covered minute matches the library's own confirm contract
/// recomputed from the tape legs: the first-tick seeding loss sets the
/// drift the pull gate sees, quiet minutes reconcile to the leg volume,
/// and pulled minutes rebuild the ring from the confirmed injection
/// window (oldest rows first, ring capacity wins) plus the remainder.
/// Out-of-order rows ride the real rollback path; fractional share volumes
/// accumulate exactly in the harness with rounded cumulatives on the legs.
///
/// Float gates below compare exactly (no epsilon): the recompute must
/// decide bit-identically with the engine, and any tolerance would fork
/// the pull/remainder decisions it pins.
#[allow(clippy::float_cmp)]
#[test]
fn integration_lite_tick_tape_reconciles_minute_volumes() {
    let content = std::fs::read_to_string(history_path("SPY_ticks.csv")).expect("ticks load");
    let mut lines = content.lines();
    let header = lines.next().expect("ticks header");
    assert_eq!(
        header,
        "timestamp_ms,price,volume,is_out_of_order,original_timestamp_ms",
        "tick columns pinned"
    );
    let rows: Vec<(i64, f64, f64)> = lines
        .map(|line| {
            let mut cells = line.split(',');
            let stamp = cells.next().expect("stamp").parse::<i64>().expect("int stamp");
            let price = cells.next().expect("price").parse::<f64>().expect("price");
            let volume = cells.next().expect("volume").parse::<f64>().expect("volume");
            (stamp, price, volume)
        })
        .collect();
    assert_eq!(rows.len(), 115839, "tick fixture row count pinned");
    // Group ticks into minute buckets by observation time; legs carry the
    // running bar-so-far OHLC and the rounded cumulative volume. The leg
    // tape per bucket drives the confirm-contract recompute below.
    let mut ticks: Vec<Bar> = Vec::with_capacity(rows.len());
    let mut stamps: Vec<i64> = Vec::with_capacity(rows.len());
    let mut buckets: Vec<(i64, Vec<f64>, usize)> = Vec::new();
    let mut minute_open = 0.0;
    let mut minute_high = f64::NEG_INFINITY;
    let mut minute_low = f64::INFINITY;
    let mut minute_cum = 0.0;
    let mut minute_open_ms = 0i64;
    for (stamp, price, volume) in &rows {
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
        let bar_open =
            DateTime::from_timestamp_millis(open_ms).expect("bar-open stamp in range");
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
            Some((open, legs, last)) if *open == open_ms => {
                legs.push(leg);
                *last = at;
            }
            _ => buckets.push((open_ms, vec![leg], at)),
        }
    }
    let minutes = load_history("SPY_1M.csv");
    let m5 = load_history("SPY_5M.csv");
    let m15 = load_history("SPY_15M.csv");
    // The driver closure includes the indicator root, whose first-tick
    // setup and scheduler-gated updates fetch the MTF market contexts —
    // the same per-timeframe datasets the chart runs inject.
    let fresh_engine = || {
        let mut engine = PineEngine::new(
            PineRuntimeConfig::new()
                .chart_symbol("SPY".to_owned())
                .chart_period("1".to_owned())
                .max_bars(200_000)
                .build(),
        );
        inject_all(&mut engine, &minutes, &m5, &m15);
        engine
    };
    const DRIVER: &str = "import showmethegrail/libUtil/12 as UTIL\n\
        import showmethegrail/libBars/12 as BARS\n\
        var bars = BARS.Bars.new()\n\
        if barstate.isfirst\n    \
        bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\n\
        bars.fetch()\n";
    let graph = resolve_imports(
        load_closure_sources(&data_root())
            .expect("closure loads")
            .into_iter()
            .map(|(key, source)| {
                let program = parse_program(&source.text).expect("real file parses");
                let id = if key == "local/IntegrationLite" {
                    indicator_library_id()
                } else {
                    let (publisher, name) = key.split_once('/').expect("key shape");
                    protopyne_kernels::LibraryId {
                        publisher: publisher.to_owned(),
                        name: name.to_owned(),
                        version: 1,
                    }
                };
                ModuleInput::new(id, source, program)
            })
            .chain(std::iter::once({
                let source = protopyne_kernels::parse_pine_source("local/Driver", DRIVER);
                let program = parse_program(DRIVER).expect("driver parses");
                ModuleInput::new(
                    protopyne_kernels::LibraryId {
                        publisher: "local".to_owned(),
                        name: "Driver".to_owned(),
                        version: 1,
                    },
                    source,
                    program,
                )
            }))
            .collect(),
    )
    .expect("closure plus driver resolves");
    let tick_modules: Vec<LinkedModule> = graph
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
        .collect();
    let mut engine = fresh_engine();
    let outcome = engine
        .run_linked_ticks(&tick_modules, &ticks, &stamps, InputSet::new())
        .expect("tick run evaluates with zero refusals");
    assert_eq!(
        outcome.evaluated,
        ticks.len(),
        "every tape row evaluates exactly once"
    );
    // The library's confirm contract, recomputed per minute from the tape
    // legs (no engine internals). `fetch` clears on the first tick and
    // seeds tracking on the second without accumulating, so the confirmed
    // accumulator is the last leg minus the second-tick leg; legs are
    // non-decreasing cumulatives (no negative tape volumes), hence no
    // rollback path and exact telescoping. Constants mirror the library
    // source: pull gate `drift_threshold` (percent), remainder/volume
    // gate `VOLUME_EPSILON`, pull window `calc_bars_count = 300`, ring
    // capacity read from the object itself.
    const MEANINGFUL: f64 = 0.0001;
    const PULL_WINDOW: usize = 300;
    let minute_data = dataset_of(&minutes);
    let reference: HashMap<i64, &Bar> = minutes
        .iter()
        .map(|bar| (bar.ts.timestamp_millis(), bar))
        .collect();
    let mut covered = 0usize;
    let mut pulled = 0usize;
    let mut steady = 0usize;
    let mut prev_pulls = 0.0f64;
    for (open_ms, legs, last_at) in &buckets {
        assert!(
            reference.contains_key(open_ms),
            "every tape minute has a reference bar, missing {open_ms}"
        );
        let state = bars_at(&outcome, *last_at);
        let total = field_num(&state, "total_vol");
        let count = field_num(&state, "count");
        let pulls_now = field_num(&state, "pull_count");
        let threshold = field_num(&state, "drift_threshold");
        let capacity = field_num(&state, "max_candles") as usize;
        let leg_last = *legs.last().expect("nonempty bucket");
        let leg_first = if legs.len() >= 2 { legs[1] } else { leg_last };
        let seeded = leg_last - leg_first;
        let finalized = if seeded > MEANINGFUL { seeded } else { 0.0 };
        let drift = if leg_last > 0.0 {
            (finalized - leg_last).abs() / leg_last * 100.0
        } else {
            0.0
        };
        let expect_pull = drift > threshold;
        assert_eq!(
            pulls_now - prev_pulls,
            f64::from(u8::from(expect_pull)),
            "pull gate fires exactly on seeding drift at {open_ms}"
        );
        prev_pulls = pulls_now;
        let close_enough = |got: f64, want: f64| {
            (got - want).abs() <= 1e-9 * want.abs().max(1.0)
        };
        if expect_pull {
            // Pulled minute: the ring rebuilds from the confirmed prefix
            // (rows at or before the bar-open, last `PULL_WINDOW` of them)
            // keeping the oldest rows up to capacity, then the remainder
            // tops up only when the ring has room.
            let visible = minute_data
                .times
                .partition_point(|time| *time <= *open_ms);
            let served = visible.min(PULL_WINDOW);
            let kept = served.min(capacity);
            let skip = visible - served;
            let window: f64 = minute_data.volumes[skip..skip + kept].iter().sum();
            let remainder = leg_last - window;
            let fires = remainder.abs() > MEANINGFUL && kept < capacity;
            let expected = window + if fires { remainder.abs() } else { 0.0 };
            assert!(
                close_enough(total, expected),
                "pulled minute {open_ms} rebuilds the confirmed window, got {total}, want {expected}"
            );
            assert_eq!(
                count,
                (kept + usize::from(fires)) as f64,
                "pulled minute {open_ms} holds window plus possible remainder candles"
            );
            pulled += 1;
        } else {
            // Quiet minute: the remainder tops the seeded gap to the leg.
            let remainder = leg_last - finalized;
            let expected = finalized
                + if remainder.abs() > MEANINGFUL {
                    remainder
                } else {
                    0.0
                };
            let expected_count = f64::from(u8::from(finalized > MEANINGFUL))
                + f64::from(u8::from(remainder.abs() > MEANINGFUL));
            assert!(
                close_enough(total, expected),
                "steady minute {open_ms} reconstructs its leg volume, got {total}, want {expected}"
            );
            assert_eq!(
                count, expected_count,
                "steady minute {open_ms} holds finalize plus remainder candles"
            );
            steady += 1;
        }
        covered += 1;
    }
    assert!(covered > 0, "tick tape covers reference minutes");
    assert!(steady > 0, "quiet minutes exercise exact reconstruction");
    assert!(pulled > 0, "drifted minutes exercise the pull contract");
    // Determinism: an identical short-window rerun is byte-identical (the
    // full tape already takes minutes; the window proves the path, the
    // full run proves the coverage).
    let window = 3000usize.min(ticks.len());
    let rerun = |engine: &mut PineEngine| {
        engine
            .run_linked_ticks(
                &tick_modules,
                &ticks[..window],
                &stamps[..window],
                InputSet::new(),
            )
            .expect("window reruns")
    };
    let first = rerun(&mut fresh_engine());
    let second = rerun(&mut fresh_engine());
    assert_eq!(
        dump_canonical(&first),
        dump_canonical(&second),
        "tick rerun over identical inputs is byte-identical"
    );
}

/// Sixty rising bars with whole-second stamps: chart legs and injection
/// datasets share the rows, so every confirmed prefix is exact.
fn ramp_bars(n: u32) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let close = 100.0 + 5.0 * f64::from(i);
            let decimal = |v: f64| format!("{v:.2}").parse::<Decimal>().expect("decimal");
            Bar::new()
                .ts(
                    chrono::TimeZone::timestamp_opt(&chrono::Utc, 1_700_000_000 + i64::from(i) * 60, 0)
                        .single()
                        .expect("valid test timestamp"),
                )
                .o(decimal(close - 0.25))
                .h(decimal(close + 0.5))
                .l(decimal(close - 0.5))
                .c(decimal(close))
                .volume(1_000 + u64::from(i) * 10)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn ctx_field(outcome: &RunOutcome, at_bar: usize, field: &str) -> Option<PineValue> {
    match outcome.value_at(at_bar, "ctx") {
        Some(Some(PineValue::Object(instance))) => instance.fields.get(field).cloned().flatten(),
        other => panic!("MarketContext object on bar {at_bar}, got {other:?}"),
    }
}

fn field_num_at(outcome: &RunOutcome, at_bar: usize, field: &str) -> f64 {
    match ctx_field(outcome, at_bar, field) {
        Some(PineValue::Float(value)) => value,
        Some(PineValue::Int(value)) => value as f64,
        other => panic!("number field {field}, got {other:?}"),
    }
}

/// Focused `request.security` injection proof: one `MarketContext.fetch`
/// over injected MTF bars agrees with the kernel batch over the same
/// confirmed `calc_bars_count` tail within kernel tolerance. Runs
/// independently of the scheduler lottery, so the injection path stays
/// pinned even on runs whose gate never fires.
#[test]
fn security_injection_agrees_with_kernels() {
    const DRIVER: &str = "import showmethegrail/libUtil/12 as UTIL\n\
        import showmethegrail/libApex/88 as libApex\n\
        var ctx = libApex.MarketContext.new()\n\
        if barstate.isfirst\n    \
        ctx.init()\n\
        ctx.fetch(UTIL.Timeframes.M5)\n";
    let bars = ramp_bars(60);
    let modules = link_driver(DRIVER);
    let mut engine = engine_for("1");
    engine
        .inject_security("SPY", "5", dataset_of(&bars))
        .expect("M5 injects");
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("injected fetch evaluates");
    let last = bars.len() - 1;
    // Oracle: kernel batch over the same last-30 confirmed bars the
    // `calc_bars_count = 30` tail takes, built from the identical dataset
    // columns the injection serves.
    let tail = &bars[bars.len() - 30..];
    let fed = dataset_of(tail);
    let ohlcv = protopyne_kernels::OwnedOhlcv {
        open: fed.opens.clone(),
        high: fed.highs.clone(),
        low: fed.lows.clone(),
        close: fed.closes.clone(),
        volume: fed.volumes.clone(),
    };
    let registry = protopyne_kernels::indicator::builtin_registry().expect("builtins");
    let batch = |kernel: &str, params: protopyne_kernels::ParamSet| {
        registry
            .get(kernel)
            .expect("kernel registered")
            .batch(&ohlcv.views(), &params)
            .expect("valid batch")
            .values
            .last()
            .copied()
            .flatten()
            .expect("kernel warm on the tail")
    };
    // Volume-sourced oracle: the bridge overrides `close` with the source
    // column, so the oracle batches over the same override.
    let vol_ohlcv = protopyne_kernels::OwnedOhlcv {
        close: fed.volumes.clone(),
        open: fed.opens.clone(),
        high: fed.highs.clone(),
        low: fed.lows.clone(),
        volume: fed.volumes.clone(),
    };
    let batch_vol = |kernel: &str, params: protopyne_kernels::ParamSet| {
        registry
            .get(kernel)
            .expect("kernel registered")
            .batch(&vol_ohlcv.views(), &params)
            .expect("valid batch")
            .values
            .last()
            .copied()
            .flatten()
            .expect("kernel warm on the tail")
    };
    let int = protopyne_kernels::ParamSet::new().with_int("length", 21);
    let close = |at: &str| field_num_at(&outcome, last, at);
    assert!((close("ma_mean") - batch("ema", int)).abs() < 1e-9, "ema over tail");
    let alma_params = protopyne_kernels::ParamSet::new()
        .with_int("length", 8)
        .with_float("offset", 0.85)
        .with_float("sigma", 6.0);
    assert!((close("ma_primary") - batch("alma", alma_params)).abs() < 1e-9, "alma over tail");
    assert!((close("atr") - batch("atr", protopyne_kernels::ParamSet::new().with_int("length", 14))).abs() < 1e-9, "atr over tail");
    assert!((close("vol_sma") - batch_vol("sma", protopyne_kernels::ParamSet::new().with_int("length", 20))).abs() < 1e-9, "volume sma over tail");
    match ctx_field(&outcome, last, "tf") {
        Some(PineValue::EnumHandle(handle)) => assert_eq!(handle.member, "M5", "context records the requested timeframe"),
        other => panic!("timeframe field, got {other:?}"),
    }
}

/// Focused MTF-table assembly proof: `createMTFTable` over fetched
/// contexts records the table lifecycle with row-label cells through the
/// recorders, independent of the scheduler lottery.
#[test]
fn mtf_table_assembly_records_cells() {
    const DRIVER: &str = "import showmethegrail/libUtil/12 as UTIL\n\
        import showmethegrail/libApex/88 as libApex\n\
        import showmethegrail/libCandles/21 as libCandles\n\
        var ctx = libApex.MarketContext.new()\n\
        var mt = libApex.ManagedTable.new()\n\
        if barstate.isfirst\n    \
        ctx.init()\n\
        ctx.fetch()\n\
        if barstate.islast\n    \
        keys = array.from(UTIL.Timeframes.M5)\n    \
        mtf = map.new<UTIL.Timeframes, libApex.MarketContext>()\n    \
        libCandles.createMTFTable(mt, UTIL.getPosition(UTIL.Positions.MIDDLE_LEFT), keys, mtf, ctx, UTIL.getSize(UTIL.Sizes.SMALL))\n";
    let bars = ramp_bars(60);
    let modules = link_driver(DRIVER);
    let mut engine = engine_for("1");
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("table assembly evaluates");
    let tables: Vec<_> = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "table")
        .collect();
    assert!(
        tables.iter().any(|event| format!("{:?}", event.kind) == "Created"),
        "table created once on the last bar"
    );
    let cells = tables
        .iter()
        .filter(|event| format!("{:?}", event.kind) == "Updated")
        .count();
    assert!(cells >= 20, "header and data cells recorded, got {cells}");
    assert!(
        outcome.draw_events.iter().any(|event| event.object == "table"
            && event.args.iter().any(|arg| arg.value == Some(PineValue::Str("Regime".to_owned())))),
        "row-label cell carries its text through the recorder"
    );
}
