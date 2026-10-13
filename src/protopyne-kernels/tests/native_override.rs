//! Whole-library native override wiring: version skew warns once and still
//! serves, Pine text stays loaded for audit, and reference routing flips
//! between the native table and Pine behind the `NativeOptions` flag (on
//! by default since the cutover; both positions pinned explicitly).
//!
//! M5 is wiring plus audit, not execution: the engine still evaluates the
//! shipped Pine in both positions, so the golden asserts the link path is
//! behavior-preserving (identical outcomes) while audits and routing differ.
//! `#[allow(clippy::float_cmp)]` covers the hand-computed golden literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::pine::test_fixture as fixture;
use crate::{
    InputSet, LinkedModule, ModuleGraph, ModuleId, ModuleInput, PineEngine, PineRuntimeConfig,
    RunOutcome, resolve_imports,
};
use chrono::TimeZone as _;
use protopyne_types::Bar;
use std::path::PathBuf;

/// The Pine-visible surface the native provider supersedes.
const LIB_BARS_PROVIDES: &[&str] = &[
    "Bars",
    "calculateDriftPct",
    "estimateBuyVolume",
    "getLTFBoundary",
    "getCandleIndex",
    "isValidVolume",
];

fn shipped(file: &str, name: &str, version: u64) -> ModuleInput {
    fixture::shipped_module(file, "showmethegrail", name, version)
}

fn driver(text: &str) -> ModuleInput {
    fixture::driver_module(text)
}

fn driver_id() -> ModuleId {
    ModuleId::new("local", "Driver")
}

/// Graph with libBars claimed at `version` plus importers pinning 6 and 88:
/// the skew fixture every routing test below shares.
fn skewed_graph() -> (ModuleGraph, ModuleInput) {
    let lib_bars = shipped("libraries/_libBars.pine", "libBars", 12);
    let modules = vec![
        shipped("libraries/_libUtil.pine", "libUtil", 12),
        lib_bars.clone(),
        driver(
            "import showmethegrail/libUtil/12 as UTIL\n\
             import showmethegrail/libBars/6 as BARS_A\n\
             import showmethegrail/libBars/88 as BARS_B\n",
        ),
    ];
    let graph = resolve_imports(modules).expect("skew still resolves");
    (graph, lib_bars)
}

fn enabled() -> NativeOptions {
    NativeOptions {
        enabled: true,
        only_symbols: None,
    }
}

fn disabled() -> NativeOptions {
    NativeOptions {
        enabled: false,
        only_symbols: None,
    }
}

#[test]
fn pins_6_12_88_serve_native_identically_with_one_warning() {
    let (graph, lib_bars) = skewed_graph();
    let overlay = apply_overrides(&graph, &enabled()).expect("override applies");
    assert!(overlay.is_served("showmethegrail", "libBars"), "libBars serves native");
    assert_eq!(overlay.provides("showmethegrail", "libBars"), Some(LIB_BARS_PROVIDES));
    // Both aliases route to the one native table, whatever they pinned.
    for alias in ["BARS_A", "BARS_B"] {
        for symbol in LIB_BARS_PROVIDES {
            match overlay.resolve_reference(&driver_id(), alias, symbol) {
                Ok(NativeResolution::Native { library, symbol: routed }) => {
                    assert_eq!(library, NativeLibraryId::new("showmethegrail", "libBars"));
                    assert_eq!(routed, *symbol);
                }
                other => panic!("{alias}.{symbol} routes native, got {other:?}"),
            }
        }
    }
    // The native table mirrors the Pine export surface it supersedes.
    let exports = graph.exports_of(&ModuleId::new("showmethegrail", "libBars")).expect("linked");
    for symbol in LIB_BARS_PROVIDES {
        assert!(exports.iter().any(|name| name == symbol), "Pine still exports {symbol}");
    }
    // One audit row: first-copy version 12 served as rust, Pine SHA kept.
    assert_eq!(overlay.audits().len(), 1, "one served library, one row");
    let audit = &overlay.audits()[0];
    assert_eq!(audit.requested_version, Some(12));
    assert_eq!(audit.served, NativeServed::Rust);
    assert_eq!(audit.pine_sha, pine_sha_of(&lib_bars.source.text), "SHA covers kept Pine text");
    assert_eq!(audit.pine_sha.len(), 16, "64-bit hex digest");
    // Pins 6/12/88 collapse to one implementation with exactly one warning.
    assert_eq!(overlay.warnings().len(), 1, "skew warns once: {:?}", overlay.warnings());
    let finding = &overlay.warnings()[0].message;
    for pin in ["6", "12", "88"] {
        assert!(finding.contains(pin), "warning names pin {pin}: {finding}");
    }
}

#[test]
fn disabled_flag_serves_pine_with_sha_retained() {
    let (graph, lib_bars) = skewed_graph();
    let overlay = apply_overrides(&graph, &disabled()).expect("override applies while off");
    assert!(!overlay.is_served("showmethegrail", "libBars"), "flag off serves nothing");
    assert_eq!(overlay.provides("showmethegrail", "libBars"), None, "no native table while off");
    match overlay.resolve_reference(&driver_id(), "BARS_A", "calculateDriftPct") {
        Ok(NativeResolution::Pine(target)) => {
            assert_eq!(target, ModuleId::new("showmethegrail", "libBars"));
        }
        other => panic!("flag off routes Pine, got {other:?}"),
    }
    // Audit still records the Pine link with the same SHA as the on-path.
    assert_eq!(overlay.audits().len(), 1);
    let audit = &overlay.audits()[0];
    assert_eq!(audit.served, NativeServed::Pine);
    assert_eq!(audit.pine_sha, pine_sha_of(&lib_bars.source.text));
    let on = apply_overrides(&graph, &enabled()).expect("on-path applies");
    assert_eq!(on.audits()[0].pine_sha, audit.pine_sha, "same kept text, same SHA");
    // Pine graph untouched: order and exports identical with the flag either way.
    assert_eq!(graph.order(), on.graph().order());
    assert_eq!(
        graph.exports_of(&ModuleId::new("showmethegrail", "libBars")),
        on.graph().exports_of(&ModuleId::new("showmethegrail", "libBars"))
    );
}

#[test]
fn unskewed_link_warns_nothing() {
    let modules = vec![
        shipped("libraries/_libUtil.pine", "libUtil", 12),
        shipped("libraries/_libBars.pine", "libBars", 12),
        driver(
            "import showmethegrail/libUtil/12 as UTIL\n\
             import showmethegrail/libBars/12 as BARS\n",
        ),
    ];
    let graph = resolve_imports(modules).expect("aligned pins resolve");
    let overlay = apply_overrides(&graph, &enabled()).expect("override applies");
    assert!(overlay.warnings().is_empty(), "one pin, no skew, no warning");
    assert!(overlay.is_served("showmethegrail", "libBars"));
}

#[test]
fn unknown_import_and_cycle_refusals_preserved_with_native_on() {
    // Unknown imports refuse at link time, before any flag is consulted.
    let ghost = driver("import showmethegrail/ghostLib/1 as G\nx = G.y\n");
    let err = resolve_imports(vec![ghost]).expect_err("unknown import refuses");
    assert!(err.to_string().contains("unknown pine import"), "reason: {err}");
    // Cycles refuse the same way; the overlay never sees a cyclic graph.
    let modules = vec![
        fixture::synthetic_library("local", "alpha", "import local/beta/1 as B\nalpha() =>\n    B.beta()"),
        fixture::synthetic_library("local", "beta", "import local/alpha/1 as A\nalpha2() =>\n    A.alpha()"),
    ];
    let err = resolve_imports(modules).expect_err("cycle refuses");
    assert!(err.to_string().contains("cycle"), "reason: {err}");
    // And a served symbol the Pine module never exported still refuses
    // through the Pine path rather than misrouting.
    let (graph, _) = skewed_graph();
    let overlay = apply_overrides(&graph, &disabled()).expect("off-path applies");
    let err = overlay
        .resolve_reference(&driver_id(), "BARS_A", "not_a_libBars_symbol")
        .expect_err("unexported symbol refuses");
    assert!(err.to_string().contains("not exported"), "reason: {err}");
}

#[test]
fn partial_override_request_refuses() {
    let (graph, _) = skewed_graph();
    let err = apply_overrides(
        &graph,
        &NativeOptions { enabled: true, only_symbols: Some(vec!["Bars".to_owned()]) },
    )
    .expect_err("subset override refuses");
    let reason = err.to_string();
    assert!(reason.contains("whole-library"), "granularity rule named: {reason}");
    assert!(reason.contains("showmethegrail/libBars"), "library named: {reason}");
}

#[test]
fn alias_last_wins_preserved() {
    let modules = vec![
        shipped("libraries/_libUtil.pine", "libUtil", 12),
        shipped("libraries/_libBars.pine", "libBars", 12),
        driver(
            "import showmethegrail/libBars/6 as BARS\n\
             import showmethegrail/libBars/12 as BARS\n",
        ),
    ];
    let graph = resolve_imports(modules).expect("repeated alias resolves");
    let table = graph.alias_table(&driver_id()).expect("importer table");
    assert_eq!(table.get("BARS"), Some(&ModuleId::new("showmethegrail", "libBars")));
    // The overlay delegates alias lookup to the graph, so last-wins survives
    // the native flip: the same alias routes native with identical outcome.
    let overlay = apply_overrides(&graph, &enabled()).expect("override applies");
    match overlay.resolve_reference(&driver_id(), "BARS", "getCandleIndex") {
        Ok(NativeResolution::Native { .. }) => {}
        other => panic!("repeated alias still routes native, got {other:?}"),
    }
}

#[test]
fn non_native_libraries_unaffected() {
    // libUtil has no native row at all and stays Pine-served with zero
    // routing change; libApex serves natively since its provider landed
    // (the serving flip — audit rust, `MarketContext` routed native).
    let modules = vec![
        shipped("libraries/_libUtil.pine", "libUtil", 12),
        shipped("libraries/_libBuffers.pine", "libBuffers", 6),
        shipped("libraries/_libDrawing.pine", "libDrawing", 61),
        shipped("libraries/_libBars.pine", "libBars", 12),
        shipped("libraries/_libApex.pine", "libApex", 1),
        driver(
            "import showmethegrail/libUtil/12 as UTIL\n\
             import showmethegrail/libApex/1 as APEX\n",
        ),
    ];
    let graph = resolve_imports(modules).expect("apex closure resolves");
    let overlay = apply_overrides(&graph, &enabled()).expect("override applies");
    assert!(!overlay.is_served("showmethegrail", "libUtil"));
    assert_eq!(overlay.provides("showmethegrail", "libUtil"), None);
    assert!(overlay.is_served("showmethegrail", "libApex"), "provider serves");
    match overlay.resolve_reference(&driver_id(), "APEX", "MarketContext") {
        Ok(NativeResolution::Native { .. }) => {}
        other => panic!("served symbol routes native, got {other:?}"),
    }
    match overlay.resolve_reference(&driver_id(), "UTIL", "getTimeframeMS") {
        Ok(NativeResolution::Pine(target)) => {
            assert_eq!(target, ModuleId::new("showmethegrail", "libUtil"));
        }
        other => panic!("non-native routes Pine, got {other:?}"),
    }
    // Audit rows exist only for native-table libraries: libBars (rust),
    // libBuffers (rust), and libApex (rust, served since its provider
    // landed). No other module is observed.
    let mut rows: Vec<(&str, NativeServed)> = overlay
        .audits()
        .iter()
        .map(|audit| (audit.requested_name.as_str(), audit.served))
        .collect();
    rows.sort_by_key(|row| row.0);
    assert_eq!(
        rows,
        vec![
            ("libApex", NativeServed::Rust),
            ("libBars", NativeServed::Rust),
            ("libBuffers", NativeServed::Rust)
        ],
        "only table libraries audited"
    );
}

/// One bar from decimal legs and a millisecond timestamp (exact below 2^53).
fn ms_bar(open: &str, high: &str, low: &str, close: &str, volume: u64, ts_millis: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::Utc
                .timestamp_opt(
                    ts_millis.div_euclid(1000),
                    ts_millis.rem_euclid(1000) as u32 * 1_000_000,
                )
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

fn linked(modules: Vec<ModuleInput>) -> Vec<LinkedModule> {
    fixture::linked_modules(modules)
}

fn test_engine() -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    )
}

fn watched(outcome: &RunOutcome, name: &str) -> f64 {
    match outcome.value_at(0, name) {
        Some(Some(crate::PineValue::Float(value))) => value,
        Some(Some(crate::PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name}, got {other:?}"),
    }
}

#[test]
fn link_path_identical_on_and_off() {
    // The M5 golden: the same shipped modules linked once, evaluated under
    // both flag positions. Eval still runs Pine either way (execution
    // cutover is M8), so every watched value must agree exactly while the
    // audits record rust vs pine.
    let script = "import showmethegrail/libUtil/12 as UTIL\n\
        import showmethegrail/libBars/12 as BARS\n\
        drift = BARS.calculateDriftPct(110.0, 100.0)\n\
        buy_up = BARS.estimateBuyVolume(100.0, 5.0)\n\
        valid = BARS.isValidVolume(0.5)\n\
        idx = BARS.getCandleIndex(1700000050000, 60000)\n";
    let modules = || {
        vec![
            shipped("libraries/_libUtil.pine", "libUtil", 12),
            shipped("libraries/_libBars.pine", "libBars", 12),
            driver(script),
        ]
    };
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, 1_699_999_980_000)];
    let mut engine = test_engine();
    let on_outcome = engine
        .run_linked(&linked(modules()), &bars, InputSet::new())
        .expect("on-path evaluates");
    let mut engine = test_engine();
    let off_outcome = engine
        .run_linked(&linked(modules()), &bars, InputSet::new())
        .expect("off-path evaluates");
    for name in ["drift", "buy_up", "idx"] {
        assert_eq!(watched(&on_outcome, name), watched(&off_outcome, name), "{name} agrees");
    }
    assert_eq!(watched(&on_outcome, "drift"), 10.0, "hand-computed drift");
    assert_eq!(watched(&on_outcome, "buy_up"), 100.0, "hand-computed buy volume");
    assert_eq!(watched(&on_outcome, "idx"), 28_333_334.0, "hand-computed candle index");
    // Routing and audits differ exactly where the flag says they must.
    let on_graph = resolve_imports(modules()).expect("resolves");
    let on = apply_overrides(&on_graph, &enabled()).expect("on applies");
    let off = apply_overrides(&on_graph, &disabled()).expect("off applies");
    assert!(matches!(
        on.resolve_reference(&driver_id(), "BARS", "calculateDriftPct"),
        Ok(NativeResolution::Native { .. })
    ));
    assert!(matches!(
        off.resolve_reference(&driver_id(), "BARS", "calculateDriftPct"),
        Ok(NativeResolution::Pine(_))
    ));
    assert_eq!(on.audits()[0].served, NativeServed::Rust);
    assert_eq!(off.audits()[0].served, NativeServed::Pine);
    assert_eq!(on.audits()[0].pine_sha, off.audits()[0].pine_sha, "kept text identical");
}

fn data_root_check() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

#[test]
fn oracle_files_still_load_warn_only() {
    // The Pine oracles stay byte-identical on disk: every library loader
    // path used above reads them with warnings-only versioning.
    for file in ["libraries/_libBars.pine", "libraries/_libUtil.pine"] {
        assert!(data_root_check().join(file).exists(), "{file} shipped");
    }
}
