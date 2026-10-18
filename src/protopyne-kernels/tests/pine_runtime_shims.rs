//! Closed-bar runtime shims: driver cursor state, injected bar timestamps,
//! chart scope, session/source/color inputs, and fail-closed live-fetch
//! refusal.
//!
//! Bars are the only clock: `time` and `timenow` read injected timestamps,
//! while symbol and timeframe arrive as chart scope on the runtime config.
//! Live `request.*` fetch has no headless meaning and refuses with an
//! injection alternative.
//!
//! Exact float comparisons below are over exact small integral timestamps
//! and counters, never measured quantities.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::{
    inputs::InputDef,
    parse_program,
    pine::builtins_core::{call as core_call, CoreArg},
};
use chrono::TimeZone as _;

fn bar_at(index: i64) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .timestamp_opt(1_700_000_000 + index * 60, 0)
            .single()
            .expect("valid test timestamp"))
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(format!("{}.00", 10 + index).parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn five_bars() -> Vec<Bar> {
    (0..5).map(bar_at).collect()
}

fn engine(warmup: u64) -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .warmup_bars(warmup)
            .build(),
    )
}

fn run(script: &str, bars: &[Bar], warmup: u64) -> RunOutcome {
    let program = parse_program(script).expect("script parses");
    engine(warmup)
        .run(&program, bars, InputSet::new())
        .expect("script runs")
}

fn run_err(script: &str, bars: &[Bar], warmup: u64) -> String {
    let program = parse_program(script).expect("script parses");
    engine(warmup)
        .run(&program, bars, InputSet::new())
        .expect_err("script refuses")
        .to_string()
}

fn flag(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Bool(value))) => value,
        other => panic!("expected bool for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> f64 {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Float(value))) => value,
        Some(Some(PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn text(outcome: &RunOutcome, at_bar: usize, name: &str) -> String {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Str(value))) => value,
        other => panic!("expected string for {name} on bar {at_bar}, got {other:?}"),
    }
}

const CURSOR_SCRIPT: &str = "first = barstate.isfirst\nnew_bar = barstate.isnew\nconfirmed = barstate.isconfirmed\nlast = barstate.islast\nhistory = barstate.ishistory\nlive = barstate.isrealtime\nidx = bar_index";

#[test]
fn cursor_flags_follow_the_full_run_without_warmup() {
    let outcome = run(CURSOR_SCRIPT, &five_bars(), 0);
    assert_eq!(outcome.evaluated, 5);
    let first = [true, false, false, false, false];
    let last = [false, false, false, false, true];
    for at_bar in 0..5 {
        assert_eq!(
            flag(&outcome, at_bar, "first"),
            first[at_bar],
            "isfirst {at_bar}"
        );
        assert!(flag(&outcome, at_bar, "new_bar"), "isnew {at_bar}");
        assert!(flag(&outcome, at_bar, "confirmed"), "isconfirmed {at_bar}");
        assert_eq!(
            flag(&outcome, at_bar, "last"),
            last[at_bar],
            "islast {at_bar}"
        );
        assert!(flag(&outcome, at_bar, "history"), "ishistory {at_bar}");
        assert!(!flag(&outcome, at_bar, "live"), "isrealtime {at_bar}");
        assert_eq!(
            num(&outcome, at_bar, "idx"),
            at_bar as f64,
            "index {at_bar}"
        );
    }
}

#[test]
fn cursor_flags_use_absolute_bars_when_warmup_is_skipped() {
    let outcome = run(CURSOR_SCRIPT, &five_bars(), 2);
    assert_eq!(outcome.evaluated, 3);
    assert_eq!(outcome.snapshots[0].bar, 2);
    assert_eq!(outcome.snapshots[1].bar, 3);
    assert_eq!(outcome.snapshots[2].bar, 4);
    for at_bar in 0..3 {
        assert!(
            !flag(&outcome, at_bar, "first"),
            "warmup bars never observe isfirst"
        );
        assert!(flag(&outcome, at_bar, "new_bar"), "isnew {at_bar}");
        assert!(
            flag(&outcome, at_bar, "confirmed"),
            "closed bars confirm {at_bar}"
        );
        assert_eq!(num(&outcome, at_bar, "idx"), (at_bar + 2) as f64);
    }
    assert!(!flag(&outcome, 0, "last"));
    assert!(!flag(&outcome, 1, "last"));
    assert!(flag(&outcome, 2, "last"));
}

#[test]
fn unknown_cursor_member_refuses() {
    let reason = run_err("x = barstate.future", &five_bars(), 0);
    assert!(
        reason.contains("barstate"),
        "reason names the table: {reason}"
    );
}

#[test]
fn time_and_now_read_injected_timestamps() {
    let outcome = run(
        "t = time\nnow = timenow\nchart = time(\"5\")\nsession = time(\"5\", \"0930-1600\")",
        &five_bars(),
        0,
    );
    for at_bar in 0..5 {
        let want = (1_700_000_000 + at_bar as i64 * 60) as f64;
        assert_eq!(num(&outcome, at_bar, "t"), want);
        assert_eq!(num(&outcome, at_bar, "now"), want);
        assert_eq!(num(&outcome, at_bar, "chart"), want);
        assert_eq!(num(&outcome, at_bar, "session"), want);
    }
}

#[test]
fn chart_scope_reads_symbol_and_period_strings() {
    let outcome = run(
        "sym = syminfo.tickerid\ntf = timeframe.period",
        &five_bars(),
        0,
    );
    assert_eq!(text(&outcome, 0, "sym"), "SPY");
    assert_eq!(text(&outcome, 0, "tf"), "5");
}

#[test]
fn session_time_supports_only_the_chart_period() {
    let reason = run_err("x = time(\"15\")", &five_bars(), 0);
    assert!(
        reason.contains("chart period"),
        "mismatched timeframe refuses: {reason}"
    );
    let reason = run_err("x = time(\"5\", \"\")", &five_bars(), 0);
    assert!(
        reason.contains("session"),
        "empty session refuses: {reason}"
    );
    let reason = run_err(
        "x = time(\"5\", \"0930-1600\", \"America/New_York\")",
        &five_bars(),
        0,
    );
    assert!(
        reason.contains("timezone"),
        "unmapped timezone refuses: {reason}"
    );
}

#[test]
fn chart_period_validation_rejects_unparseable_scope() {
    let program = parse_program("x = timeframe.period").expect("parses");
    let mut bad_engine = PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("nope".to_owned())
            .build(),
    );
    let reason = bad_engine
        .run(&program, &five_bars(), InputSet::new())
        .expect_err("bad scope refuses")
        .to_string();
    assert!(
        reason.contains("invalid runtime configuration"),
        "scope refusal is fail-closed: {reason}"
    );
}

#[test]
fn input_variants_read_back_live_defaults() {
    let outcome = run(
        "enum Smoothings\n    WMA = \"WMA\"\n    EMA = \"EMA\"\nsess = input.session(\"0930-1600\", title = \"Session\")\nkind = input.enum(Smoothings.WMA, title = \"Kind\")\nsrc = input.source(close)\npaint = input.color(#26a69a, title = \"Paint\")",
        &five_bars(),
        0,
    );
    for at_bar in 0..5 {
        assert_eq!(text(&outcome, at_bar, "sess"), "0930-1600");
        assert_eq!(
            outcome.value_at(at_bar, "kind"),
            Some(Some(PineValue::EnumHandle(crate::value::EnumHandle::new(
                "Smoothings",
                "WMA"
            ))))
        );
        assert_eq!(num(&outcome, at_bar, "src"), (10 + at_bar as i64) as f64);
        assert!(
            matches!(
                outcome.value_at(at_bar, "paint"),
                Some(Some(PineValue::ColorHandle(_)))
            ),
            "color stays opaque"
        );
    }
}

#[test]
fn session_input_validates_and_keeps_overrides() {
    let mut inputs = InputSet::new();
    let args = [
        CoreArg::pos(Some(PineValue::Str("0930-1600".to_owned()))),
        CoreArg::named("title", Some(PineValue::Str("Session".to_owned()))),
    ];
    let first = core_call("input", "session", "session-site", &args, &[], &mut inputs)
        .expect("session declares");
    assert_eq!(first, Some(PineValue::Str("0930-1600".to_owned())));
    inputs
        .set("session-site", PineValue::Str("2200-0200".to_owned()))
        .expect("override applies");
    let second = core_call("input", "session", "session-site", &args, &[], &mut inputs)
        .expect("override reads back");
    assert_eq!(second, Some(PineValue::Str("2200-0200".to_owned())));

    let empty = [CoreArg::pos(Some(PineValue::Str(String::new())))];
    let reason = core_call("input", "session", "empty-site", &empty, &[], &mut inputs)
        .expect_err("empty session refuses");
    assert!(reason.to_string().contains("session"));
    core_call("input", "session", "session-site", &args, &[], &mut inputs)
        .expect("declaration stands");
    let bad_override = inputs.set("session-site", PineValue::Str(String::new()));
    assert!(bad_override.is_err(), "empty override refuses");
}

#[test]
fn source_input_tracks_the_series_until_overridden() {
    let mut inputs = InputSet::new();
    let first = core_call(
        "input",
        "source",
        "source-site",
        &[CoreArg::pos(Some(PineValue::Float(7.5)))],
        &[],
        &mut inputs,
    )
    .expect("source passes through");
    assert_eq!(first, Some(PineValue::Float(7.5)));
    assert_eq!(
        inputs.get("source-site"),
        None,
        "live default stores nothing"
    );
    inputs
        .set("source-site", PineValue::Float(9.0))
        .expect("override applies");
    let second = core_call(
        "input",
        "source",
        "source-site",
        &[CoreArg::pos(Some(PineValue::Float(8.5)))],
        &[],
        &mut inputs,
    )
    .expect("override wins");
    assert_eq!(second, Some(PineValue::Float(9.0)));

    let bad = core_call(
        "input",
        "source",
        "bad-source",
        &[CoreArg::pos(Some(PineValue::Array(Vec::new())))],
        &[],
        &mut inputs,
    );
    assert!(bad.is_err(), "non-series default refuses");
}

#[test]
fn enum_and_color_inputs_keep_overrides() {
    let mut inputs = InputSet::new();
    let wma = PineValue::EnumHandle(crate::value::EnumHandle::new("Smoothings", "WMA"));
    let ema = PineValue::EnumHandle(crate::value::EnumHandle::new("Smoothings", "EMA"));
    let declared = core_call(
        "input",
        "enum",
        "enum-site",
        &[CoreArg::pos(Some(wma.clone()))],
        &[],
        &mut inputs,
    )
    .expect("enum declares");
    assert_eq!(declared, Some(wma.clone()));
    inputs
        .set("enum-site", ema.clone())
        .expect("override applies");
    let read_back = core_call(
        "input",
        "enum",
        "enum-site",
        &[CoreArg::pos(Some(wma))],
        &[],
        &mut inputs,
    )
    .expect("override reads back");
    assert_eq!(read_back, Some(ema));

    let ink = PineValue::ColorHandle(crate::value::ColorHandle::new(1));
    let paper = PineValue::ColorHandle(crate::value::ColorHandle::new(2));
    let declared = core_call(
        "input",
        "color",
        "color-site",
        &[CoreArg::pos(Some(ink))],
        &[],
        &mut inputs,
    )
    .expect("color declares");
    assert!(matches!(declared, Some(PineValue::ColorHandle(_))));
    inputs.set("color-site", paper).expect("override applies");
    let read_back = core_call(
        "input",
        "color",
        "color-site",
        &[CoreArg::pos(declared)],
        &[],
        &mut inputs,
    )
    .expect("override reads back");
    assert_eq!(
        read_back,
        Some(PineValue::ColorHandle(crate::value::ColorHandle::new(2)))
    );
}

#[test]
fn source_declaration_without_a_seed_holds_no_value() {
    let mut inputs = InputSet::new();
    inputs
        .declare("bare-source", InputDef::Source)
        .expect("source declares");
    assert_eq!(inputs.get("bare-source"), None);
}

#[test]
fn live_fetch_refuses_with_an_injection_alternative() {
    for script in [
        "x = request.security(\"SPY\", \"D\", close)",
        "x = request.security_lower_tf(\"SPY\", \"1\", [time, close])",
    ] {
        let reason = run_err(script, &five_bars(), 0);
        assert!(
            reason.contains("refused headlessly"),
            "live fetch refuses: {reason}"
        );
        assert!(
            reason.contains("inject"),
            "refusal names injection: {reason}"
        );
    }
}
