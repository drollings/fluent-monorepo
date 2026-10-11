//! Bar-by-bar engine tests: transcribed reference behavior plus documented
//! corrections, refusal reasons, and run isolation.
//!
//! The reference runtime is never a dependency, so its unit expectations are
//! transcribed here as literals on small hand-computed values. Every
//! intentional divergence (truthy numbers, truncation, warmup-blind `var`
//! seeding) has a pinning test named for the correction.

use super::*;
use crate::{PineRuntimeConfig, inputs::InputSet, parse_program};
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn bar(open: &str, close: &str) -> Bar {
    Bar::new()
        .ts(
            chrono::Utc
                .with_ymd_and_hms(2026, 1, 2, 9, 0, 0)
                .single()
                .expect("valid test timestamp"),
        )
        .o(open.parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn closes() -> Vec<Bar> {
    vec![
        bar("100.00", "10.00"),
        bar("100.00", "11.00"),
        bar("100.00", "12.00"),
        bar("100.00", "13.00"),
    ]
}

fn run_default(script: &str, bars: &[Bar]) -> RunOutcome {
    let program = parse_program(script).expect("script parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine
        .run(&program, bars, InputSet::new())
        .expect("script runs")
}

fn run_err(script: &str, bars: &[Bar]) -> String {
    let program = parse_program(script).expect("script parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine
        .run(&program, bars, InputSet::new())
        .expect_err("script refuses")
        .to_string()
}

/// Numeric reading of one snapshot cell (`int` auto-casts, like the engine).
fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> Option<f64> {
    match outcome.value_at(at_bar, name)? {
        Some(PineValue::Float(v)) => Some(v),
        Some(PineValue::Int(v)) => Some(v as f64),
        _ => None,
    }
}

fn is_absent(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    // A cell is absent when the snapshot holds `na` — or holds no entry at
    // all (e.g. a `var` seeded with `na` that nothing stored yet).
    matches!(outcome.value_at(at_bar, name), None | Some(None))
}

// ---- Transcribed reference behavior ----

#[test]
fn assignment_arithmetic_matches_reference() {
    // Reference `test_parse_simple_assignment` + `test_eval_binary_add`.
    let bars = closes();
    let outcome = run_default("length = 20\nx = 10 + 5", &bars);
    assert_eq!(outcome.evaluated, 4);
    for at_bar in 0..4 {
        assert_eq!(num(&outcome, at_bar, "length"), Some(20.0));
        assert_eq!(num(&outcome, at_bar, "x"), Some(15.0));
    }
}

#[test]
fn binary_operators_match_reference() {
    let bars = closes();
    let outcome = run_default(
        "a = 10 + 5\ns = 10 - 4\nm = 3 * 4\nd = 10 / 4\nr = 10 % 3\neq = (10 == 10)\nne = (10 != 9)\nlt = (3 < 4)\nge = (4 >= 5)\nneg = -7\nall = true and false\nany = true or false",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "a"), Some(15.0));
    assert_eq!(num(&outcome, 0, "s"), Some(6.0));
    assert_eq!(num(&outcome, 0, "m"), Some(12.0));
    assert_eq!(num(&outcome, 0, "d"), Some(2.5));
    assert_eq!(num(&outcome, 0, "r"), Some(1.0));
    assert_eq!(
        outcome.value_at(0, "eq"),
        Some(Some(PineValue::Bool(true)))
    );
    assert_eq!(
        outcome.value_at(0, "ne"),
        Some(Some(PineValue::Bool(true)))
    );
    assert_eq!(
        outcome.value_at(0, "lt"),
        Some(Some(PineValue::Bool(true)))
    );
    assert_eq!(
        outcome.value_at(0, "ge"),
        Some(Some(PineValue::Bool(false)))
    );
    assert_eq!(num(&outcome, 0, "neg"), Some(-7.0));
    assert_eq!(
        outcome.value_at(0, "all"),
        Some(Some(PineValue::Bool(false)))
    );
    assert_eq!(
        outcome.value_at(0, "any"),
        Some(Some(PineValue::Bool(true)))
    );
}

#[test]
fn string_concat_and_comparison_match_reference() {
    let bars = closes();
    let outcome = run_default(
        "greet = \"a\" + \"b\"\ntagged = \"n=\" + 20\nbig = \"b\" > \"a\"",
        &bars,
    );
    assert_eq!(
        outcome.value_at(0, "greet"),
        Some(Some(PineValue::Str("ab".to_owned())))
    );
    assert_eq!(
        outcome.value_at(0, "tagged"),
        Some(Some(PineValue::Str("n=20".to_owned())))
    );
    assert_eq!(
        outcome.value_at(0, "big"),
        Some(Some(PineValue::Bool(true)))
    );
}

#[test]
fn builtin_legs_read_current_bar() {
    // Reference `test_series_ctx`: legs expose the current bar's values.
    let bars = closes();
    let outcome = run_default("c = close\no = open", &bars);
    for (at_bar, want) in [10.0, 11.0, 12.0, 13.0].iter().enumerate() {
        assert_eq!(num(&outcome, at_bar, "c"), Some(*want));
        assert_eq!(num(&outcome, at_bar, "o"), Some(100.0));
    }
}

#[test]
fn offsets_read_history_and_go_na_out_of_range() {
    let bars = closes();
    let outcome = run_default("prev = close[1]\nfar = close[5]", &bars);
    assert!(is_absent(&outcome, 0, "prev"), "no history on the first bar");
    assert_eq!(num(&outcome, 1, "prev"), Some(10.0));
    assert_eq!(num(&outcome, 3, "prev"), Some(12.0));
    for at_bar in 0..4 {
        assert!(is_absent(&outcome, at_bar, "far"), "out of range stays na");
    }
}

#[test]
fn variable_history_offsets_follow_assignment() {
    let bars = closes();
    let outcome = run_default("x = close\ny = x[1]", &bars);
    assert!(is_absent(&outcome, 0, "y"));
    assert_eq!(num(&outcome, 2, "y"), Some(11.0));
}

#[test]
fn var_counter_persists_across_bars() {
    let bars = closes();
    let outcome = run_default("var c = 0\nc := c + 1", &bars);
    for (at_bar, want) in [1.0, 2.0, 3.0, 4.0].iter().enumerate() {
        assert_eq!(num(&outcome, at_bar, "c"), Some(*want));
    }
}

#[test]
fn if_else_branches_on_comparison() {
    let bars = vec![bar("9.00", "10.00"), bar("12.00", "11.00")];
    let outcome = run_default("dir = 0\nif close > open\n    dir := 1\nelse\n    dir := -1", &bars);
    assert_eq!(num(&outcome, 0, "dir"), Some(1.0));
    assert_eq!(num(&outcome, 1, "dir"), Some(-1.0));
}

#[test]
fn for_loop_accumulates() {
    let bars = closes();
    let outcome = run_default("s = 0\nfor i = 1 to 3\n    s := s + i", &bars);
    for at_bar in 0..4 {
        assert_eq!(num(&outcome, at_bar, "s"), Some(6.0));
        assert_eq!(num(&outcome, at_bar, "i"), Some(3.0));
    }
}

#[test]
fn while_loop_terminates() {
    let bars = closes();
    let outcome = run_default("i = 0\nwhile i < 3\n    i := i + 1", &bars);
    for at_bar in 0..4 {
        assert_eq!(num(&outcome, at_bar, "i"), Some(3.0));
    }
}

#[test]
fn ternary_selects_branch() {
    let bars = vec![bar("9.00", "10.00"), bar("12.00", "11.00")];
    let outcome = run_default("t = close > open ? 10 : 20", &bars);
    assert_eq!(num(&outcome, 0, "t"), Some(10.0));
    assert_eq!(num(&outcome, 1, "t"), Some(20.0));
}

#[test]
fn nz_and_na_match_reference() {
    // Reference `nz`/`na` specials: defaults fill, predicates detect.
    let bars = closes();
    let outcome = run_default(
        "n = nz(na)\nm = nz(close, -1)\nb = na(close)\nc = na(na)",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "n"), Some(0.0));
    assert_eq!(num(&outcome, 2, "m"), Some(12.0));
    assert_eq!(
        outcome.value_at(0, "b"),
        Some(Some(PineValue::Bool(false)))
    );
    assert_eq!(
        outcome.value_at(0, "c"),
        Some(Some(PineValue::Bool(true)))
    );
}

#[test]
fn fixnan_carries_forward_across_bars() {
    let bars = closes();
    let outcome = run_default("f = fixnan(close[5])", &bars);
    assert!(is_absent(&outcome, 0, "f"), "leading absence stays absent");
    assert!(is_absent(&outcome, 3, "f"), "still absent before any value");
    let mut longer = closes();
    longer.extend(closes());
    let outcome = run_default("f = fixnan(close[5])", &longer);
    assert_eq!(num(&outcome, 5, "f"), Some(10.0));
    assert_eq!(num(&outcome, 7, "f"), Some(12.0));
}

#[test]
fn literal_destructuring_binds_each_target() {
    let bars = closes();
    let outcome = run_default("[a, b] = [1, 2]", &bars);
    assert_eq!(num(&outcome, 0, "a"), Some(1.0));
    assert_eq!(num(&outcome, 0, "b"), Some(2.0));
}

// ---- Documented corrections (reference behavior deliberately not copied) ----

#[test]
fn truthy_numbers_refuse() {
    let bars = closes();
    for script in [
        "if 1\n    x = 1",
        "x = 1 and true",
        "x = not 1.0",
        "t = 1 ? 2 : 3",
    ] {
        let reason = run_err(script, &bars);
        assert!(
            reason.contains("bool"),
            "numbers never enter boolean positions: {script} -> {reason}"
        );
    }
}

#[test]
fn var_initializes_on_first_evaluated_bar() {
    // Warmup bars advance without evaluation, so `var` captures the first
    // evaluated bar — never silently data-bar zero.
    let bars = closes();
    let program = parse_program("var x = close").expect("parses");
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new()
            .warmup_bars(2)
            .build(),
    );
    let outcome = engine
        .run(&program, &bars, InputSet::new())
        .expect("runs");
    assert_eq!(outcome.evaluated, 2);
    assert_eq!(num(&outcome, 0, "x"), Some(12.0));
    assert_eq!(num(&outcome, 1, "x"), Some(12.0));
}

#[test]
fn var_na_seeding_waits_for_assignment() {
    let bars = vec![bar("9.00", "8.00"), bar("9.00", "10.00")];
    let outcome = run_default("var x = na\nif close > 9\n    x := 7", &bars);
    assert!(is_absent(&outcome, 0, "x"), "na seed holds until assigned");
    assert_eq!(num(&outcome, 1, "x"), Some(7.0));
}

#[test]
fn for_bounds_require_ints() {
    // The reference truncates float bounds; this executor refuses them.
    let bars = closes();
    let reason = run_err("for i = 1 to 2.9\n    x = i", &bars);
    assert!(reason.contains("int"), "float bound refuses: {reason}");
}

#[test]
fn division_by_zero_reads_absent() {
    // A zero denominator reads `na`, never a refusal: shipped code divides
    // zero-initialized fields before their first fetch, and Pine answers
    // `na`/`inf` there, never an abort. Infinities never enter the engine.
    let bars = closes();
    let outcome = run_default("x = 1 / 0\ny = 1 % 0\nz = 0 / 0", &bars);
    assert!(is_absent(&outcome, 0, "x"), "float divide by zero is na");
    assert!(is_absent(&outcome, 0, "y"), "float modulo by zero is na");
    assert!(is_absent(&outcome, 0, "z"), "zero over zero is na");
}

#[test]
fn while_bound_is_fail_closed() {
    let bars = closes();
    let reason = run_err("while true\n    x = 1", &bars);
    assert!(
        reason.contains("10"),
        "bound names the iteration cap: {reason}"
    );
}

// ---- Fail-closed refusals with reasons ----

#[test]
fn unknown_function_refuses_naming_it() {
    let bars = closes();
    // Bare calls still refuse; namespaced dispatch names the method.
    let reason = run_err("x = nosuchfn(close)", &bars);
    assert!(reason.contains("nosuchfn"), "reason names the callee: {reason}");
    let reason = run_err("x = ta.dmi(close, 14)", &bars);
    assert!(reason.contains("ta.dmi"), "reason names the method: {reason}");
    let reason = run_err("x = close.foo()", &bars);
    assert!(reason.contains("foo"), "reason names the method: {reason}");
}

#[test]
fn ta_calls_honor_source_and_recurrence() {
    // sma3 over [10,11,12,13]: na, na, 11, 12.
    let bars = closes();
    let outcome = run_default("s = ta.sma(close, 3)", &bars);
    assert!(is_absent(&outcome, 0, "s"));
    assert!(is_absent(&outcome, 1, "s"));
    assert_eq!(num(&outcome, 2, "s"), Some(11.0));
    assert_eq!(num(&outcome, 3, "s"), Some(12.0));

    // The source argument is honored: constant-open averages read 100,
    // never the close leg.
    let outcome = run_default("s = ta.sma(open, 2)", &bars);
    for at_bar in 0..4 {
        if at_bar == 0 {
            assert!(is_absent(&outcome, at_bar, "s"));
        } else {
            assert_eq!(num(&outcome, at_bar, "s"), Some(100.0));
        }
    }

    // ema uses kernel recurrence (seeded from bar 1 with length 2).
    let outcome = run_default("e = ta.ema(close, 2)", &bars);
    assert!(is_absent(&outcome, 0, "e"));
    assert_eq!(num(&outcome, 1, "e"), Some(10.5));
    assert_eq!(num(&outcome, 2, "e"), Some(11.5));

    // na sources propagate instead of reading legs.
    let outcome = run_default("n = ta.sma(close[5], 3)", &bars);
    for at_bar in 0..4 {
        assert!(is_absent(&outcome, at_bar, "n"), "na in, na out");
    }

    // Arity problems refuse through the engine with the method named.
    let reason = run_err("s = ta.sma(close)", &bars);
    assert!(reason.contains("ta.sma"), "refusal names the method: {reason}");
}

#[test]
fn methods_arrays_and_switches_refuse_with_reasons() {
    let bars = closes();
    // Header lines are inert (the loader/graph own them); unknown
    // namespaces still refuse naming the method.
    for (script, want) in [
        ("x = close.foo()", "foo"),
        ("x = XX.fn(1.0)", "xx.fn"),
    ] {
        let reason = run_err(script, &bars);
        assert!(
            reason.to_lowercase().contains(want),
            "{script} refuses naming {want}: {reason}"
        );
    }
}

#[test]
fn undefined_variable_and_stray_reassign_refuse() {
    let bars = closes();
    let undef = run_err("x = nope", &bars);
    assert!(undef.contains("nope"), "reason names the variable: {undef}");
    let stray = run_err("x := 1", &bars);
    assert!(stray.contains('x'), "reason names the target: {stray}");
}

// ---- Isolation ----

#[test]
fn concurrent_runs_stay_isolated() {
    let low = closes();
    let high: Vec<Bar> = vec![
        bar("100.00", "110.00"),
        bar("100.00", "111.00"),
        bar("100.00", "112.00"),
        bar("100.00", "113.00"),
    ];
    let script = "var c = 0\nc := c + 1\nx = close";
    let program = parse_program(script).expect("parses");

    let mut first = PineEngine::new(PineRuntimeConfig::new().build());
    let mut second = PineEngine::new(PineRuntimeConfig::new().build());
    let first_out = first
        .run(&program, &low, InputSet::new())
        .expect("first runs");
    let second_out = second
        .run(&program, &high, InputSet::new())
        .expect("second runs");
    assert_eq!(num(&first_out, 3, "c"), Some(4.0));
    assert_eq!(num(&second_out, 3, "c"), Some(4.0));
    assert_eq!(num(&first_out, 3, "x"), Some(13.0));
    assert_eq!(num(&second_out, 3, "x"), Some(113.0));

    // Re-running the first engine reproduces its own run exactly: per-run
    // state resets, so sequential runs never leak into each other either.
    let repeat = first
        .run(&program, &low, InputSet::new())
        .expect("re-runs");
    assert_eq!(num(&repeat, 3, "c"), Some(4.0));
    assert_eq!(num(&repeat, 3, "x"), Some(13.0));
}

// ---- Real-file grammar execution: augmented assignment ----

#[test]
fn augmented_assignment_accumulates() {
    // Series locals reset per bar, so every bar computes (1 + 2) * 3.
    let outcome = run_default("int x = 1\nx += 2\nx *= 3", &closes());
    assert_eq!(num(&outcome, 0, "x"), Some(9.0));
    assert_eq!(num(&outcome, 3, "x"), Some(9.0));
}

#[test]
fn multi_assign_runs_left_to_right() {
    let outcome = run_default("int a = 0\nint b = 0\na := 1, b := a + 1", &closes());
    assert_eq!(num(&outcome, 3, "a"), Some(1.0));
    assert_eq!(num(&outcome, 3, "b"), Some(2.0));
}

#[test]
fn multi_declaration_binds_every_item() {
    let outcome = run_default("float o = 1.5, h = 2.5, g", &closes());
    assert_eq!(num(&outcome, 3, "o"), Some(1.5));
    assert_eq!(num(&outcome, 3, "h"), Some(2.5));
    assert!(
        matches!(outcome.value_at(3, "g"), Some(None)),
        "missing initializer reads as na"
    );
}

// ---- Real-file grammar execution: bare `switch` ----

#[test]
fn bare_switch_takes_first_truthy_arm() {
    let script = "x = switch\n    close > 11.0 => \"high\"\n    close > 10.0 => \"mid\"\n    => \"low\"";
    let outcome = run_default(script, &closes());
    let text = |bar: usize| match outcome.value_at(bar, "x") {
        Some(Some(PineValue::Str(text))) => Some(text.clone()),
        other => panic!("expected string at bar {bar}, got {other:?}"),
    };
    assert_eq!(text(0).as_deref(), Some("low"));
    assert_eq!(text(1).as_deref(), Some("mid"));
    assert_eq!(text(2).as_deref(), Some("high"));
}

#[test]
fn bare_switch_without_match_yields_na() {
    let outcome = run_default("x = switch\n    close > 100.0 => 1", &closes());
    assert!(
        matches!(outcome.value_at(0, "x"), Some(None)),
        "no arm, no default: na"
    );
}

// ---- Real-file grammar execution: chained construction ----

#[test]
fn instance_call_chains_construction() {
    let script = "export type Counter\n    int n = 0\nexport method init(Counter this, int start) =>\n    this.n := start\n    this.n\ngot = Counter.new().init(7)";
    let outcome = run_default(script, &closes());
    assert_eq!(num(&outcome, 3, "got"), Some(7.0));
}

#[test]
fn instance_call_on_plain_value_refuses() {
    let reason = run_err("y = (1).init()", &closes());
    assert!(
        reason.contains("needs an object receiver"),
        "refusal names the rule: {reason}"
    );
}

#[test]
fn for_in_executes_over_arrays() {
    let outcome = run_default(
        "vals = array.from(5, 7)\ntotal = 0\nfor [i, v] in vals\n    total := total + i * 10 + v",
        &closes(),
    );
    assert_eq!(num(&outcome, 3, "total"), Some(22.0));
}

#[test]
fn for_in_over_scalar_refuses_with_reason() {
    let reason = run_err("for child_id in 5\n    child_id", &closes());
    assert!(
        reason.contains("'for ... in' iterates arrays"),
        "refusal names the rule: {reason}"
    );
}
