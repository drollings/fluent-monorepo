//! Buffer-library goldens: the shipped `libBuffers` source linked with
//! small driver scripts, pinning each export family against hand-computed
//! expectations.
//!
//! Drivers call through the `BUFFERS` alias exactly like the real indicator
//! does, so cross-module dispatch, ring/array duality, and the statistical
//! helpers run the same code the end-to-end proof uses. `libBuffers` ships
//! no imports of its own, so the harness links it alone.
//!
//! Float goldens below are hand-computed decimal expectations with a tight
//! tolerance, never measured quantities.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::{
    InputSet, LibraryId, LinkedModule, ModuleInput, PineRuntimeConfig, load_pine_source,
    parse_pine_source, parse_program,
};
use protopyne_types::Bar;
use std::collections::HashMap;
use std::path::PathBuf;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;

const IMPORTS: &str = "import showmethegrail/libBuffers/6 as BUFFERS\n";

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn library_id(publisher: &str, name: &str, version: u64) -> LibraryId {
    LibraryId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
        version,
    }
}

/// One shipped library as a graph input: identity from the header scan,
/// structure from the parser, over the same byte-identical text.
fn shipped_module(file: &str, publisher: &str, name: &str, version: u64) -> ModuleInput {
    let path = data_root().join(file);
    let source = load_pine_source(&path).expect("shipped library loads");
    let program = parse_program(&source.text).expect("shipped library parses");
    ModuleInput::new(library_id(publisher, name, version), source, program)
}

fn driver_module(text: &str) -> ModuleInput {
    let source = parse_pine_source("local/Driver", text);
    let program = parse_program(text).expect("driver parses");
    ModuleInput::new(library_id("local", "Driver", 1), source, program)
}

/// Link the shipped buffers library under graph order with one driver root
/// calling through the `BUFFERS` alias.
fn linked(driver: &str) -> Vec<LinkedModule> {
    let graph = crate::resolve_imports(vec![
        shipped_module(
            "libraries/_libBuffers.pine",
            "showmethegrail",
            "libBuffers",
            6,
        ),
        driver_module(driver),
    ])
    .expect("library plus driver resolve");
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

fn test_engine() -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    )
}

/// One closed bar from decimal legs and a whole-second timestamp.
fn bar(open: &str, high: &str, low: &str, close: &str, volume: u64, ts_secs: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::TimeZone::timestamp_opt(&chrono::Utc, ts_secs, 0)
                .single()
                .expect("valid test timestamp"),
        )
        .o(open.parse().expect("decimal"))
        .h(high.parse().expect("decimal"))
        .l(low.parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

/// Five rising bars (closes 10..50): every push-per-bar driver below
/// accumulates one value per bar in order.
fn rising_bars() -> Vec<Bar> {
    (0u32..5)
        .map(|i| {
            let close = 10.0 + 10.0 * f64::from(i);
            bar(
                &format!("{:.2}", close - 0.25),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect()
}

fn run_linked(driver: &str, bars: &[Bar]) -> RunOutcome {
    let modules = linked(driver);
    let mut engine = test_engine();
    engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("linked run evaluates")
}

fn run_err(driver: &str, bars: &[Bar]) -> String {
    let modules = linked(driver);
    let mut engine = test_engine();
    engine
        .run_linked(&modules, bars, InputSet::new())
        .expect_err("driver refuses")
        .to_string()
}

fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> f64 {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Float(value))) => value,
        Some(Some(PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn is_absent(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    matches!(outcome.value_at(at_bar, name), None | Some(None))
}

fn arr_at(outcome: &RunOutcome, at_bar: usize, name: &str) -> Vec<Option<PineValue>> {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Array(items))) => items,
        other => panic!("expected array for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn arr_nums(outcome: &RunOutcome, at_bar: usize, name: &str) -> Vec<Option<f64>> {
    arr_at(outcome, at_bar, name)
        .iter()
        .map(|slot| match slot {
            Some(PineValue::Float(v)) => Some(*v),
            Some(PineValue::Int(v)) => Some(*v as f64),
            _ => None,
        })
        .collect()
}

fn is_true(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    matches!(
        outcome.value_at(at_bar, name),
        Some(Some(PineValue::Bool(true)))
    )
}

/// Numeric instance field (ints widen, like the engine's reads).
fn fld(instance: &crate::UserInstance, name: &str) -> f64 {
    match instance.fields.get(name) {
        Some(Some(PineValue::Float(value))) => *value,
        Some(Some(PineValue::Int(value))) => *value as f64,
        other => panic!("expected number field {name}, got {other:?}"),
    }
}

fn obj_at(outcome: &RunOutcome, at_bar: usize, name: &str) -> crate::UserInstance {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected object for {name} on bar {at_bar}, got {other:?}"),
    }
}

#[test]
fn array_mode_push_get_size_newest_first() {
    // Array mode appends without eviction: five pushes read back
    // newest-first, past-the-end reads absent, and the overflow flag arms
    // once the data outgrows the declared capacity.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(3)\n\
             b.push(close)\nsz = b.size()\nnewest = b.get(0)\noldest = b.get(4)\noob = b.get(5)\nfull = b.is_full()"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 4, "sz"), 5.0);
    assert_eq!(num(&outcome, 4, "newest"), 50.0);
    assert_eq!(num(&outcome, 4, "oldest"), 10.0);
    assert!(is_absent(&outcome, 4, "oob"), "past-the-end reads absent");
    // Fullness is exact equality: three valid over capacity three reads
    // full, while five valid over capacity three does not.
    assert!(is_true(&outcome, 2, "full"), "exact capacity reads full");
    assert!(!is_true(&outcome, 4, "full"), "overgrown never reads full");
    match outcome.value_at(4, "b") {
        Some(Some(PineValue::Object(instance))) => {
            assert_eq!(instance.type_name, "BufferFloat");
            match instance.fields.get("needs_compaction") {
                Some(Some(PineValue::Bool(true))) => {}
                other => panic!("overflow flag armed, got {other:?}"),
            }
        }
        other => panic!("expected BufferFloat object, got {other:?}"),
    }
}

#[test]
fn ring_mode_wraps_and_bounds() {
    // Ring mode overwrites oldest-first: five pushes into capacity three
    // keep the newest three, and indexes at/past the count read absent.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(3, true)\n\
             b.push(close)\nsz = b.size()\ng0 = b.get(0)\ng1 = b.get(1)\ng2 = b.get(2)\ng3 = b.get(3)\ncap = b.max_capacity()"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 4, "sz"), 3.0);
    assert_eq!(num(&outcome, 4, "g0"), 50.0);
    assert_eq!(num(&outcome, 4, "g1"), 40.0);
    assert_eq!(num(&outcome, 4, "g2"), 30.0);
    assert!(is_absent(&outcome, 4, "g3"), "index past the window reads absent");
    assert_eq!(num(&outcome, 4, "cap"), 3.0);
    assert_eq!(fld(&obj_at(&outcome, 4, "b"), "count"), 5.0);
}

#[test]
fn clear_resets_both_modes() {
    // Clearing empties the store, zeroes the lifetime count, and — in
    // ring mode — re-arms the head, so the next push lands at slot zero.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(3, true)\n\
             b.push(close)\nif bar_index == 2\n    b.clear()\nsz = b.size()\nempty = b.is_empty()\ng0 = b.get(0)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 1, "sz"), 2.0);
    assert_eq!(num(&outcome, 2, "sz"), 0.0);
    assert!(is_true(&outcome, 2, "empty"));
    assert_eq!(num(&outcome, 4, "sz"), 2.0, "bars 3-4 refill after clear");
    assert_eq!(num(&outcome, 4, "g0"), 50.0);
}

#[test]
fn linearized_view_tracks_chronology_and_invalidates() {
    // Ring linearization reads oldest-to-newest across the head wrap, and
    // every push invalidates the cache: the post-push view on one bar
    // differs from the pre-push view (a stale cache would repeat it).
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(3, true)\n\
             before = b.getLinearized()\nb.push(close)\nafter = b.getLinearized()\n\
             n_before = array.size(before)\nn_after = array.size(after)"
        ),
        &bars,
    );
    assert_eq!(arr_nums(&outcome, 3, "before"), vec![Some(10.0), Some(20.0), Some(30.0)]);
    assert_eq!(arr_nums(&outcome, 3, "after"), vec![Some(20.0), Some(30.0), Some(40.0)]);
    assert_eq!(num(&outcome, 3, "n_before"), 3.0);
    assert_eq!(num(&outcome, 3, "n_after"), 3.0);
    // Array mode linearizes to the raw store contents in push order.
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(9)\n\
             b.push(close)\nlin = b.getLinearized()"
        ),
        &bars,
    );
    assert_eq!(
        arr_nums(&outcome, 4, "lin"),
        vec![Some(10.0), Some(20.0), Some(30.0), Some(40.0), Some(50.0)]
    );
}

#[test]
fn compact_removes_na_in_array_mode_only() {
    // Five closes plus two absent pushes leave two holes; compact packs
    // the survivors newest-first and reports the removed count per bar it
    // runs on (bars 1 and 3 each shed one hole, later bars find none).
    // Ring mode cannot compact without breaking its structure: zero.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(9)\n\
             b.push(close)\nif bar_index == 1 or bar_index == 3\n    b.push(na)\n\
             removed = b.compact()\nsz = b.size()\ng0 = b.get(0)\ng1 = b.get(1)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 1, "removed"), 1.0);
    assert_eq!(num(&outcome, 3, "removed"), 1.0);
    assert_eq!(num(&outcome, 4, "removed"), 0.0);
    assert_eq!(num(&outcome, 4, "sz"), 5.0);
    // Compact packs the newest-first scan, so storage ends newest-first
    // and `get` (written for insertion order) mirrors: the library's own
    // shape, executed faithfully, not an engine reorder.
    assert_eq!(num(&outcome, 4, "g0"), 40.0);
    assert_eq!(num(&outcome, 4, "g1"), 20.0);
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(3, true)\n\
             b.push(close)\nremoved = b.compact()"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 4, "removed"), 0.0);
}

#[test]
fn valid_helpers_scan_newest_first() {
    // Five closes plus absent pushes on bars 1 and 3: storage reads
    // [50, na, 40, 30, na, 20, 10] newest-first, so the five survivors
    // sit at indices [0, 2, 3, 5, 6].
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(9)\n\
             b.push(close)\nif bar_index == 1 or bar_index == 3\n    b.push(na)\n\
             vc = b.valid_count()\nvals = b.for_each_valid()\nidx = b.get_valid_indices()\nempty = b.is_empty()"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 4, "vc"), 5.0);
    assert_eq!(
        arr_nums(&outcome, 4, "vals"),
        vec![Some(50.0), Some(40.0), Some(30.0), Some(20.0), Some(10.0)]
    );
    assert_eq!(
        arr_nums(&outcome, 4, "idx"),
        vec![Some(0.0), Some(2.0), Some(3.0), Some(5.0), Some(6.0)]
    );
    assert!(!is_true(&outcome, 4, "empty"));
}

#[test]
fn correlation_linreg_predict_hand_rolled() {
    // x = 1..4, y = 2x: perfect correlation 1.0, slope 1 / intercept 1 on
    // x itself, and slope 2 / intercept 2 on y predicting 10.0 next.
    // Singleton buffers answer absence instead of inventing a fit.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var bx = BUFFERS.BufferFloat.new()\nvar by = BUFFERS.BufferFloat.new()\n\
             if barstate.isfirst\n    bx.init(9)\n    by.init(9)\n\
             bx.push(bar_index + 1)\nby.push(2 * (bar_index + 1))\n\
             c = bx.correlation(by)\n[s, i] = bx.linreg()\n[ys, yi] = by.linreg()\np = by.predict()\np2 = by.predict(2)"
        ),
        &bars,
    );
    let corr = num(&outcome, 3, "c");
    assert!((corr - 1.0).abs() < EPS, "perfect correlation, got {corr}");
    assert!((num(&outcome, 3, "s") - 1.0).abs() < EPS);
    assert!((num(&outcome, 3, "i") - 1.0).abs() < EPS);
    assert!((num(&outcome, 3, "ys") - 2.0).abs() < EPS);
    assert!((num(&outcome, 3, "yi") - 2.0).abs() < EPS);
    assert!((num(&outcome, 3, "p") - 10.0).abs() < EPS);
    assert!((num(&outcome, 3, "p2") - 12.0).abs() < EPS);
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var bx = BUFFERS.BufferFloat.new()\nvar by = BUFFERS.BufferFloat.new()\n\
             if barstate.isfirst\n    bx.init(9)\n    by.init(9)\n    bx.push(1.0)\n    by.push(2.0)\n\
             c = bx.correlation(by)\n[s, i] = bx.linreg()\np = bx.predict()"
        ),
        &bars,
    );
    assert!(is_absent(&outcome, 4, "c"), "singleton correlation reads na");
    assert!(is_absent(&outcome, 4, "s"), "singleton slope reads na");
    assert!(is_absent(&outcome, 4, "i"), "singleton intercept reads na");
    assert!(is_absent(&outcome, 4, "p"), "singleton prediction reads na");
}

#[test]
fn push_and_limit_pins_both_overloads() {
    // Both same-named standalone overloads run: float and int arrays shed
    // the oldest element past the cap, and a non-positive cap answers
    // absence (the trailing guard takes no branch).
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}fa = BUFFERS.push_and_limit(array.from(1.0, 2.0, 3.0), 4.0, 3)\n\
             ia = BUFFERS.push_and_limit(array.from(1, 2, 3), 4, 3)\n\
             zn = BUFFERS.push_and_limit(array.from(1.0), 2.0, 0)\n\
             fs = array.size(fa)\nisz = array.size(ia)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 4, "fs"), 3.0);
    assert_eq!(arr_nums(&outcome, 4, "fa"), vec![Some(2.0), Some(3.0), Some(4.0)]);
    assert_eq!(num(&outcome, 4, "isz"), 3.0);
    assert_eq!(arr_nums(&outcome, 4, "ia"), vec![Some(2.0), Some(3.0), Some(4.0)]);
    assert!(is_absent(&outcome, 4, "zn"), "non-positive cap takes no branch");
}

#[test]
fn standalone_mutations_write_back_to_caller_arrays() {
    // Collection arguments ride by reference: marking, removing, and
    // compacting through standalone calls mutate the caller's array, and
    // the descending scan in `last` finds the newest survivor. The array
    // shrinks as the driver consumes it, so bar 0 shows the full
    // lifecycle and bar 4 the exhausted remainder.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var a = array.from(1.0, 2.0, 3.0)\n\
             marked = BUFFERS.mark_for_removal(a, 2)\n\
             newest = BUFFERS.last(a)\n\
             oob = BUFFERS.mark_for_removal(a, 9)\n\
             gone = BUFFERS.remove_value(a, 2.0)\n\
             after = BUFFERS.last(a)\n\
             again = BUFFERS.remove_value(a, 2.0)\n\
             kept = BUFFERS.compact(a)"
        ),
        &bars,
    );
    assert!(is_true(&outcome, 0, "marked"));
    assert_eq!(num(&outcome, 0, "newest"), 2.0, "marked slot skipped");
    assert!(!is_true(&outcome, 0, "oob"), "past-the-end mark fails");
    assert!(is_true(&outcome, 0, "gone"));
    assert_eq!(num(&outcome, 0, "after"), 1.0, "removed value compacted away");
    assert!(!is_true(&outcome, 0, "again"), "second removal finds nothing");
    assert_eq!(num(&outcome, 0, "kept"), 0.0, "already packed");
    assert!(!is_true(&outcome, 4, "marked"), "consumed array rejects the mark");
    assert_eq!(num(&outcome, 4, "after"), 1.0, "lone survivor reads back");
}

#[test]
fn standalone_valid_helpers_covariance_and_regression() {
    // Standalone helpers over a holed array: counts, filtered values and
    // indices, biased/unbiased covariance, and the na-filtering linreg
    // fitting [1, 5] to slope 4 / intercept 1, predicting 9.0 next.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}a = array.from(1.0, na, 5.0)\n\
             vc = BUFFERS.valid_count(a)\n\
             vals = BUFFERS.for_each_valid(a)\n\
             idx = BUFFERS.get_valid_indices(a)\n\
             x = array.from(1.0, 2.0, 3.0, 4.0)\n\
             y = array.from(2.0, 4.0, 6.0, 8.0)\n\
             cb = BUFFERS.covariance(x, y)\n\
             cu = BUFFERS.covariance(x, y, false)\n\
             c = BUFFERS.correlation(x, y)\n\
             [s, i] = BUFFERS.linreg(a)\n\
             p = BUFFERS.predict(a)\n\
             thin = BUFFERS.correlation(array.from(1.0), array.from(2.0))"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 4, "vc"), 2.0);
    assert_eq!(arr_nums(&outcome, 4, "vals"), vec![Some(1.0), Some(5.0)]);
    assert_eq!(arr_nums(&outcome, 4, "idx"), vec![Some(0.0), Some(2.0)]);
    assert!((num(&outcome, 4, "cb") - 2.5).abs() < EPS);
    assert!((num(&outcome, 4, "cu") - 10.0 / 3.0).abs() < EPS);
    assert!((num(&outcome, 4, "c") - 1.0).abs() < EPS);
    assert!((num(&outcome, 4, "s") - 4.0).abs() < EPS);
    assert!((num(&outcome, 4, "i") - 1.0).abs() < EPS);
    assert!((num(&outcome, 4, "p") - 9.0).abs() < EPS);
    assert!(is_absent(&outcome, 4, "thin"), "singleton correlation reads na");
}

#[test]
fn buffer_stats_arms_hand_rolled() {
    // Reduction arms over [1,2,3,4]: biased variance 1.25, unbiased 5/3,
    // median 2.5, percentile endpoints exact, and z-scores symmetric about
    // the mean; a constant buffer standardizes to absence, never refusal.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(9)\n\
             b.push(bar_index + 1.0)\n\
             vb = b.variance()\nvu = b.variance(false)\nmed = b.median()\n\
             p0 = b.percentile(0.0)\np50 = b.percentile(50.0)\np100 = b.percentile(100.0)\n\
             z = b.standardize()\nzm = b.max()\nzn = b.min()\nzs = b.sum()\nza = b.avg()"
        ),
        &bars,
    );
    assert!((num(&outcome, 3, "vb") - 1.25).abs() < EPS);
    assert!((num(&outcome, 3, "vu") - 5.0 / 3.0).abs() < EPS);
    assert!((num(&outcome, 3, "med") - 2.5).abs() < EPS);
    assert_eq!(num(&outcome, 3, "p0"), 1.0);
    assert_eq!(num(&outcome, 3, "p50"), 2.5);
    assert_eq!(num(&outcome, 3, "p100"), 4.0);
    let scores = arr_nums(&outcome, 3, "z");
    assert_eq!(scores.len(), 4);
    assert!((scores[0].expect("z0") + 1.341_640_786_499_873_8).abs() < EPS);
    assert!((scores[3].expect("z3") - 1.341_640_786_499_873_8).abs() < EPS);
    assert!((scores[0].expect("z0") + scores[3].expect("z3")).abs() < EPS);
    assert!((scores[1].expect("z1") + scores[2].expect("z2")).abs() < EPS);
    assert_eq!(num(&outcome, 3, "zm"), 4.0);
    assert_eq!(num(&outcome, 3, "zn"), 1.0);
    assert_eq!(num(&outcome, 3, "zs"), 10.0);
    assert!((num(&outcome, 3, "za") - 2.5).abs() < EPS);
    // Constant input: zero spread standardizes every slot to absence.
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(9)\n\
             b.push(5.0)\nz = b.standardize()\nmed = b.median()"
        ),
        &bars,
    );
    assert_eq!(arr_nums(&outcome, 3, "z"), vec![None, None, None, None]);
    assert_eq!(num(&outcome, 3, "med"), 5.0);
}

#[test]
fn int_buffer_modes_and_medians() {
    // Integer twin: mode picks the repeat, even-count median midpoints to
    // a float, and min/max/sum compose over the linearized store.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferInt.new()\nif barstate.isfirst\n    b.init(9)\n\
             b.push(bar_index == 2 ? 7 : 3)\n\
             mo = b.mode()\nmed = b.median()\nmn = b.min()\nmx = b.max()\nsm = b.sum()\nav = b.avg()\nvc = BUFFERS.valid_count_int(array.from(1, na, 3))\n\
             iv = BUFFERS.for_each_valid_int(array.from(1, na, 3))\nii = BUFFERS.get_valid_indices_int(array.from(1, na, 3))"
        ),
        &bars,
    );
    // Store newest-first is [3,7,3,3]: mode 3 (three hits over one).
    assert_eq!(num(&outcome, 3, "mo"), 3.0);
    // Sorted [3,3,3,7]: even midpoint of 3 and 3.
    assert_eq!(num(&outcome, 3, "med"), 3.0);
    assert_eq!(num(&outcome, 3, "mn"), 3.0);
    assert_eq!(num(&outcome, 3, "mx"), 7.0);
    assert_eq!(num(&outcome, 3, "sm"), 16.0);
    assert_eq!(num(&outcome, 3, "av"), 4.0);
    assert_eq!(num(&outcome, 3, "vc"), 2.0);
    assert_eq!(arr_nums(&outcome, 3, "iv"), vec![Some(1.0), Some(3.0)]);
    assert_eq!(arr_nums(&outcome, 3, "ii"), vec![Some(0.0), Some(2.0)]);
}

#[test]
fn int_buffer_even_median_midpoints() {
    // An even store with distinct middles ([1,2,3,4] insertion order)
    // midpoints to 2.5, proving the float-valued even branch on ints.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferInt.new()\nif barstate.isfirst\n    b.init(9)\n\
             b.push(bar_index + 1)\nmed = b.median()\nmo = b.mode()"
        ),
        &bars,
    );
    assert!((num(&outcome, 3, "med") - 2.5).abs() < EPS);
    // All distinct: first value wins the frequency tie.
    assert_eq!(num(&outcome, 3, "mo"), 1.0);
}

#[test]
fn array_helpers_concat_shift_and_bounds() {
    // Concatenation joins stores; shifting pops the head and answers the
    // removed value; an empty shift reads absent; out-of-range
    // percentiles refuse instead of inventing a rank.
    let bars = rising_bars();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}c = array.concat(array.from(1.0), array.from(2.0, 3.0))\n\
             s = array.shift(array.from(1.0, 2.0))\n\
             e = array.shift(array.new<float>())\n\
             nc = array.size(c)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 4, "nc"), 3.0);
    assert_eq!(arr_nums(&outcome, 4, "c"), vec![Some(1.0), Some(2.0), Some(3.0)]);
    assert_eq!(num(&outcome, 4, "s"), 1.0);
    assert!(is_absent(&outcome, 4, "e"), "empty shift reads absent");
    let reason = run_err(
        &format!(
            "{IMPORTS}var b = BUFFERS.BufferFloat.new()\nif barstate.isfirst\n    b.init(9)\n\
             b.push(1.0)\nq = b.percentile(150.0)"
        ),
        &bars,
    );
    assert!(
        reason.contains("percentile"),
        "out-of-range percentile names the rule: {reason}"
    );
}

#[test]
fn buffers_execute_as_source_objects_without_native_mirror() {
    // The buffer types live as Pine user instances: construction lowers
    // to a Pine object holding the declared fields, never to a native
    // handle. (The tree-wide negative assertion below guards the
    // stronger claim that no native mirror type exists at all.)
    let bars = rising_bars();
    let outcome = run_linked(
        &format!("{IMPORTS}b = BUFFERS.BufferFloat.new()\ni = BUFFERS.BufferInt.new()"),
        &bars,
    );
    for name in ["b", "i"] {
        match outcome.value_at(4, name) {
            Some(Some(PineValue::Object(instance))) => {
                for field in ["data", "max_size", "head", "count", "needs_compaction"] {
                    assert!(
                        instance.fields.contains_key(field),
                        "{name} carries {field}"
                    );
                }
            }
            other => panic!("expected user instance for {name}, got {other:?}"),
        }
    }
    match outcome.value_at(4, "b") {
        Some(Some(PineValue::Object(instance))) => assert_eq!(instance.type_name, "BufferFloat"),
        other => panic!("float buffer names its type, got {other:?}"),
    }
    match outcome.value_at(4, "i") {
        Some(Some(PineValue::Object(instance))) => assert_eq!(instance.type_name, "BufferInt"),
        other => panic!("int buffer names its type, got {other:?}"),
    }
}

/// Explicit non-goal: the buffer types execute as Pine source through the
/// engine and are never mirrored as native Rust items. This test walks
/// the crate's own source tree and fails if a native `BufferFloat` /
/// `BufferInt` struct, enum, or type alias is introduced.
#[test]
fn no_native_buffer_mirror_in_sources() {
    fn visit(dir: &PathBuf, offenders: &mut Vec<String>) {
        let entries = std::fs::read_dir(dir).expect("sources readable");
        for entry in entries {
            let path = entry.expect("entry readable").path();
            if path.is_dir() {
                visit(&path, offenders);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("source readable");
            for (index, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("").trim();
                let head = code
                    .strip_prefix("pub(crate) ")
                    .or_else(|| code.strip_prefix("pub "))
                    .unwrap_or(code);
                let declares = head.strip_prefix("struct ").or_else(|| head.strip_prefix("enum "));
                if let Some(rest) = declares {
                    let item = rest
                        .split(['<', '(', '{', ' ', ';'])
                        .next()
                        .unwrap_or("");
                    if matches!(item, "BufferFloat" | "BufferInt" | "FloatBuffer" | "IntBuffer") {
                        offenders.push(format!("{}:{}: {line}", path.display(), index + 1));
                    }
                }
                if let Some(rest) = head.strip_prefix("type ") {
                    let item = rest.split(['<', '=', ' ', ';']).next().unwrap_or("");
                    if matches!(item, "BufferFloat" | "BufferInt" | "FloatBuffer" | "IntBuffer") {
                        offenders.push(format!("{}:{}: {line}", path.display(), index + 1));
                    }
                }
            }
        }
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    visit(&root, &mut offenders);
    assert!(
        offenders.is_empty(),
        "native buffer mirror introduced: {offenders:?}"
    );
}

