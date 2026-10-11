//! Real-file parse conformance: every shipped Pine file parses with zero
//! errors, per-module statement/export shapes are pinned, a malformed corpus
//! still refuses, and parse-then-display round-trips structurally on both the
//! overlap subset and the full real closure.
//!
//! Display normalizes (comments and blank lines vanish, blocks re-indent to
//! four spaces, expressions gain clarifying parentheses), so round-trip means
//! structural equality of the two programs, never byte equality of text.
//! Eight-digit `#rrggbbaa` literals are the one documented normalization:
//! the alpha byte scales into the `0..=100` transparency lane, which cannot
//! invert exactly, so display emits the opaque `#rrggbb` channels.
//!
//! Test path note: this file rides the `pine::syntax` unit module, so the
//! runnable filter is `pine::syntax`, not the file stem.

use super::*;
use crate::display_program;
use std::path::PathBuf;

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn load(relative: &str) -> String {
    std::fs::read_to_string(data_root().join(relative)).expect("real file reads")
}

fn parse_file(relative: &str) -> PineProgram {
    parse_program(&load(relative)).expect("real file parses with zero errors")
}

/// Top-level `export`ed names in source order, mirroring the import graph's
/// visibility rule (functions, methods, consts, enums, types only).
fn exported_names_of(program: &PineProgram) -> Vec<String> {
    let mut names = Vec::new();
    for stmt in &program.statements {
        if let PineStmt::ExportDecl(inner) = stmt {
            let name = match inner.as_ref() {
                PineStmt::FuncDecl { name, .. }
                | PineStmt::MethodDecl { name, .. }
                | PineStmt::ConstDecl { name, .. }
                | PineStmt::EnumDecl { name, .. }
                | PineStmt::TypeDecl { name, .. } => Some(name.clone()),
                _ => None,
            };
            if let Some(name) = name {
                names.push(name);
            }
        }
    }
    names
}

fn str_vec(names: &[&str]) -> Vec<String> {
    names.iter().map(ToString::to_string).collect()
}

#[test]
fn closure_files_parse_with_pinned_shapes() {
    // (file, top-level statements, exports in source order).
    let cases: Vec<(&str, usize, Vec<String>)> = vec![
        (
            "libraries/_libUtil.pine",
            54,
            str_vec(&[
                "SIGNAL_NONE",
                "SIGNAL_BULL_ENTRY",
                "SIGNAL_BULL_EXIT",
                "SIGNAL_BEAR_ENTRY",
                "SIGNAL_BEAR_EXIT",
                "OSC_UPPER_BOUND",
                "OSC_UPPER_THRESHOLD",
                "OSC_UPPER_RANGE",
                "OSC_CENTER",
                "OSC_LOWER_RANGE",
                "OSC_LOWER_THRESHOLD",
                "OSC_LOWER_BOUND",
                "OSC_RANGE_WIDTH",
                "COLOR_NONE",
                "COLOR_NEUTRAL",
                "Timeframes",
                "getTimeframeAbbrev",
                "getTimeframeMS",
                "getTimeframeFromMS",
                "getTimeframeFromMultiplier",
                "getTimeframe",
                "getBarsInTimeframes",
                "isChartTimeframe",
                "MarketRegimes",
                "getRegimeEmoji",
                "Smoothings",
                "Oscillators",
                "PatternTypes",
                "CandlePatterns",
                "isPatternType",
                "risingMeterChar",
                "tanh",
                "fast_tanh",
                "safeDivide",
                "sameSign",
                "safeArrayAccess",
                "Sizes",
                "LineStyles",
                "Positions",
                "LineType",
                "ExtensionType",
                "getSize",
                "getLineStyle",
                "getPosition",
                "isPositionOverlay",
                "Pivots",
                "Scheduler",
                "init",
                "register",
                "updateEarliestDue",
                "every",
                "validateOHLCV",
                "estimateDeltaVolume",
            ]),
        ),
        (
            "libraries/_libMAs.pine",
            10,
            str_vec(&[
                "getOscillatorColor",
                "getKalman",
                "getEIT",
                "getSupertrend",
                "factorySmoothing",
                "getMACD",
                "getVossPredictiveFilter",
                "normalizeOsc",
            ]),
        ),
        (
            "libraries/_libBars.pine",
            38,
            str_vec(&[
                "Bars",
                "calculateDriftPct",
                "estimateBuyVolume",
                "getLTFBoundary",
                "getCandleIndex",
                "isValidVolume",
                "init",
                "clear",
                "size",
                "hasVolumeWaiting",
                "get",
                "isSpike",
                "pullAndReconcile",
                "onConfirmLTF",
                "onConfirm",
                "getVolumeImbalance",
                "getAlpha",
                "fetch",
            ]),
        ),
        (
            "libraries/_libApex.pine",
            32,
            str_vec(&[
                "getHealthMAs",
                "MarketContext",
                "init",
                "detectRegime",
                "fetch",
                "getColorMomentumAlpha",
                "calcVolumePreview",
                "detectPivotFromOscillatorExtreme",
                "ManagedTable",
                "update",
                "delete",
                "SweepType",
                "OrderBlock",
                "FairValueGap",
                "LiquidityZone",
                "StructureBreak",
                "MarketLevel",
                "init",
                "clean",
                "updateLevelSuccess",
                "updateLevelTouch",
                "breakLevel",
                "binarySearchMarketLevels",
                "flip",
                "remove",
                "createNewZoneWithProfile",
                "draw",
            ]),
        ),
        (
            "libraries/_libBuffers.pine",
            65,
            str_vec(&[
                "BufferFloat",
                "BufferInt",
                "getLinearized",
                "getLinearized",
                "init",
                "push",
                "get",
                "size",
                "clear",
                "valid_count",
                "is_empty",
                "is_full",
                "max_capacity",
                "for_each_valid",
                "get_valid_indices",
                "compact",
                "max",
                "min",
                "sum",
                "avg",
                "stdev",
                "variance",
                "median",
                "mode",
                "standardize",
                "percentile",
                "covariance",
                "correlation",
                "linreg",
                "predict",
                "init",
                "push",
                "get",
                "size",
                "clear",
                "valid_count",
                "is_empty",
                "is_full",
                "max_capacity",
                "for_each_valid",
                "get_valid_indices",
                "compact",
                "max",
                "min",
                "sum",
                "avg",
                "median",
                "mode",
                "valid_count",
                "for_each_valid",
                "get_valid_indices",
                "covariance",
                "correlation",
                "linreg",
                "predict",
                "mark_for_removal",
                "compact",
                "remove_value",
                "last",
                "push_and_limit",
                "valid_count_int",
                "for_each_valid_int",
                "get_valid_indices_int",
                "push_and_limit",
            ]),
        ),
        (
            "libraries/_libDrawing.pine",
            31,
            str_vec(&[
                "VisualElement",
                "PendingUpdate",
                "DrawingManager",
                "init",
                "getNextId",
                "init",
                "remove",
                "remove",
                "enforceElementLimit",
                "needsUpdate",
                "processUpdates",
                "setStyle",
                "setAutoExtend",
                "disableAutoExtend",
                "extend",
                "extendAll",
                "cleanup",
                "contains",
                "contains",
                "getElement",
                "getElement",
                "addElement",
                "addStructureLine",
                "makeStatic",
                "addLabeledZone",
            ]),
        ),
        (
            "libraries/_libCandles.pine",
            56,
            str_vec(&[
                "getCandlePatternAcronym",
                "getCandlePatternEmoji",
                "getCandlePatternExplanation",
                "detectCandlePatterns",
                "fetchWithCandlePatterns",
                "createMTFColumn",
                "createMTFTable",
            ]),
        ),
        ("indicators/_IntegrationLite.pine", 136, Vec::new()),
    ];
    let mut actuals = Vec::new();
    for (relative, _, _) in &cases {
        let program = parse_file(relative);
        actuals.push((
            relative.to_string(),
            program.statements.len(),
            exported_names_of(&program),
        ));
    }
    let expected: Vec<(String, usize, Vec<String>)> = cases
        .into_iter()
        .map(|(relative, stmts, exports)| (relative.to_string(), stmts, exports))
        .collect();
    assert_eq!(
        actuals, expected,
        "statement/export snapshots pin the real-file shapes"
    );
}

#[test]
fn malformed_corpus_still_refuses() {
    // Truly malformed Pine: each input must fail, never silently parse.
    let cases = vec![
        "if",
        "x =",
        "for i = 1 to",
        "while",
        "switch",
        "switch y",
        "export 123",
        "enum",
        "type",
        "method",
        "x = (1",
        "x = [a, 1",
        "[a, b = foo()",
        "for i =",
        "const",
        "f(a b)",
    ];
    for source in cases {
        assert!(
            parse_program(source).is_err(),
            "malformed input still refuses: {source:?}"
        );
    }
}

fn assert_round_trip(source: &str) {
    let program = parse_program(source).expect("round-trip fixture parses");
    let displayed = display_program(&program);
    let reparsed =
        parse_program(&displayed).expect("displayed output re-parses");
    assert_eq!(
        reparsed, program,
        "parse-then-display is structurally stable for {source:?}\ndisplayed:\n{displayed}"
    );
}

#[test]
fn display_round_trips_overlap_subset() {
    // One snippet per overlapping grammar shape: statements, expressions,
    // declarations, and headers the reference parser also covers.
    let cases = vec![
        "length = 20",
        "x := 1",
        "o := open, h := high",
        "float o = na, h = na",
        "float o = na, float h = na, l = low",
        "int n = 10",
        "[a, b] = foo()",
        "var x = 1",
        "varip float last = na",
        "var float threshold = 2.0",
        "const int SIGNAL_NONE = 0",
        "const color C = color.rgb(0, 0, 0, 100.0)",
        "plot(close)",
        "plot(close, title = \"T\", color = color.red)",
        "x = close[1]",
        "x = open + high * 2.0 - low / close % 5",
        "x = a and b or not c",
        "x = a == b and c != d",
        "x = a <= b and c >= d and e < f and g > h",
        "x = a ? b : c",
        "x = nz(na, 0)",
        "x = ta.sma(close, 14)",
        "x = math.max(1, 2)",
        "x = array.new_float(10)",
        "x = map.new<int, int>()",
        "x = UTIL.getSize(size.auto)",
        "x = this.register(name, start)",
        "x = Type.new().init(a, b)",
        "x = lookup(sym).field",
        "x = a.b",
        "x = a . b",
        "x = time(timeframe.period, \"0930-1600\")",
        "x = time",
        "c = #00ff00",
        "s = \"hello \\\"world\\\"\"",
        "s = 'single'",
        "b = true",
        "t = [a, 1, 2.5]",
        "getEIT(src, len) =>\n    alpha = 2.0 / (len + 1)\n    alpha * src",
        "method push(BufferFloat this, float value) =>\n    this.size = array.size(this.buf)",
        "export const int X = 1",
        "export getX(src) =>\n    src",
        "enum Timeframes\n    M1 = 60000\n    M5 = 300000",
        "export type Bars\n    varip float last_price = na\n    threshold = 300\n    int count = 0",
        "if a\n    b\n    c\nelse\n    d",
        "if a\n    b\nelse if c\n    d\nelse\n    e",
        "if a\n    b\nelse\n    if c\n        d",
        "for i = 1 to 10\n    x := x + i",
        "for x in arr\n    total := total + x",
        "for [k, v] in pairs\n    total := total + v",
        "while done\n    done := step()",
        "x = switch y\n    1 => a\n    => b",
        "switch y\n    1 => a\n    => b",
        "switch\n    a > b => 1\n    => 0",
        "x = switch y\n    1 =>\n        a\n        b\n    => c",
        "import showmethegrail/libUtil/12 as UTIL",
        "library('libUtil')",
        "indicator(\"Integration Lite\", overlay = true)",
        "x = -close",
        "x = -(a + b)",
        "x = na",
        "x = 1e-9",
        "x = 0.000000001 + 2.5",
    ];
    for source in cases {
        assert_round_trip(source);
    }
}

#[test]
fn display_round_trips_real_closure() {
    // Full structural round-trip on every shipped file: display drops
    // comments and layout but must preserve every statement and expression.
    let files = vec![
        "libraries/_libUtil.pine",
        "libraries/_libMAs.pine",
        "libraries/_libBars.pine",
        "libraries/_libApex.pine",
        "libraries/_libBuffers.pine",
        "libraries/_libDrawing.pine",
        "libraries/_libCandles.pine",
        "indicators/_IntegrationLite.pine",
    ];
    for relative in files {
        let program = parse_file(relative);
        let displayed = display_program(&program);
        let reparsed = parse_program(&displayed)
            .expect("displayed real file re-parses");
        assert_eq!(
            reparsed, program,
            "real file {relative} survives parse-then-display"
        );
    }
}
