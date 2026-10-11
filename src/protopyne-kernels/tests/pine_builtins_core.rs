//! Scalar and structural namespace tests: `math.*`, `str.*`, `input.*`,
//! `array.*`, `map.*`, `color.*`, `timeframe.*`, plus the opaque display
//! constants (`display.*`, `size.*`, `position.*`, `timeframe.period`).
//!
//! Scalar math expectations on the reference overlap are transcribed from
//! the reference runtime's own unit corpus as literals; absence propagates
//! (`na` in, `na` out) and kind mismatches refuse instead of coercing.

use super::*;
use crate::pine::syntax::PineType;
use crate::{PineRuntimeConfig, inputs::InputSet, parse_program};
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn pos(value: Option<PineValue>) -> CoreArg {
    CoreArg::pos(value)
}

fn named(name: &str, value: Option<PineValue>) -> CoreArg {
    CoreArg::named(name, value)
}

fn float_arg(v: f64) -> CoreArg {
    pos(Some(PineValue::Float(v)))
}

fn int_arg(v: i64) -> CoreArg {
    pos(Some(PineValue::Int(v)))
}

fn str_arg(v: &str) -> CoreArg {
    pos(Some(PineValue::Str(v.to_owned())))
}

fn num_of(value: Option<&PineValue>) -> Option<f64> {
    match value {
        Some(PineValue::Float(v)) => Some(*v),
        Some(PineValue::Int(v)) => Some(*v as f64),
        _ => None,
    }
}

fn run_math(method: &str, args: &[CoreArg]) -> Option<PineValue> {
    let mut inputs = InputSet::new();
    call("math", method, "test-site", args, &[], &mut inputs).expect("valid math call")
}

fn math_num(method: &str, args: &[CoreArg]) -> Option<f64> {
    num_of(run_math(method, args).as_ref())
}

fn run_err(namespace: &str, method: &str, args: &[CoreArg]) -> String {
    let mut inputs = InputSet::new();
    call(namespace, method, "test-site", args, &[], &mut inputs)
        .expect_err("call refuses")
        .to_string()
}

// ---- math.* (reference overlap, transcribed) ----

#[test]
fn math_abs_max_min_match_reference() {
    assert_eq!(math_num("abs", &[float_arg(-3.5)]), Some(3.5));
    assert_eq!(math_num("abs", &[int_arg(-4)]), Some(4.0));
    assert_eq!(math_num("max", &[float_arg(1.0), float_arg(5.0)]), Some(5.0));
    assert_eq!(
        math_num("max", &[float_arg(1.0), float_arg(5.0), float_arg(3.0)]),
        Some(5.0)
    );
    assert_eq!(math_num("min", &[float_arg(1.0), float_arg(5.0)]), Some(1.0));
    // Absence propagates, never coerces to zero.
    assert_eq!(math_num("max", &[pos(None), float_arg(5.0)]), None);
    assert_eq!(math_num("abs", &[pos(None)]), None);
}

#[test]
fn math_floor_ceil_round_pow_match_reference() {
    assert_eq!(math_num("floor", &[float_arg(3.7)]), Some(3.0));
    assert_eq!(math_num("ceil", &[float_arg(3.2)]), Some(4.0));
    // Half away from zero, matching the reference corpus.
    assert_eq!(math_num("round", &[float_arg(2.5)]), Some(3.0));
    assert_eq!(math_num("round", &[float_arg(-2.5)]), Some(-3.0));
    // Two-argument precision, as the target libraries call it.
    assert_eq!(
        math_num("round", &[float_arg(2.345), int_arg(2)]),
        Some(2.35)
    );
    assert_eq!(math_num("pow", &[float_arg(2.0), float_arg(10.0)]), Some(1024.0));
}

#[test]
fn math_sqrt_log_exp_sign_match_reference() {
    assert_eq!(math_num("sqrt", &[float_arg(16.0)]), Some(4.0));
    assert_eq!(math_num("sqrt", &[float_arg(-1.0)]), None);
    assert_eq!(math_num("log", &[float_arg(std::f64::consts::E)]), Some(1.0));
    assert_eq!(math_num("exp", &[float_arg(0.0)]), Some(1.0));
    assert_eq!(math_num("sign", &[float_arg(-4.0)]), Some(-1.0));
    assert_eq!(math_num("sign", &[float_arg(0.0)]), Some(0.0));
}

#[test]
fn math_trig_serves_target_libraries() {
    // The target libraries call `cos` and `asin`; the rest complete the set.
    assert_eq!(math_num("cos", &[float_arg(0.0)]), Some(1.0));
    let half_pi = math_num("asin", &[float_arg(1.0)]).expect("asin defined");
    assert!((half_pi - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
}

#[test]
fn math_avg_sum_fold_all_arguments() {
    assert_eq!(
        math_num("avg", &[float_arg(1.0), float_arg(2.0), float_arg(3.0)]),
        Some(2.0)
    );
    assert_eq!(
        math_num("sum", &[float_arg(1.0), float_arg(2.0), float_arg(3.0)]),
        Some(6.0)
    );
    assert_eq!(math_num("sum", &[float_arg(4.0)]), Some(4.0));
}

#[test]
fn math_refusals_carry_names() {
    let message = run_err("math", "abs", &[]);
    assert!(message.contains("math.abs"), "arity names the function: {message}");
    let message = run_err("math", "abs", &[str_arg("x")]);
    assert!(message.contains("math.abs"), "kind names the function: {message}");
    let message = run_err("math", "no_such_fn", &[float_arg(1.0)]);
    assert!(message.contains("math.no_such_fn"), "unknown names itself: {message}");
    let message = run_err("nope", "abs", &[float_arg(1.0)]);
    assert!(message.contains("nope.abs"), "unknown namespace names itself: {message}");
}

// ---- str.* (reference overlap, transcribed) ----

#[test]
fn str_tostring_renders_scalars() {
    let mut inputs = InputSet::new();
    let mut render = |value: Option<PineValue>| {
        call("str", "tostring", "s", &[pos(value)], &[], &mut inputs).expect("renders")
    };
    assert_eq!(render(Some(PineValue::Float(3.25))), Some(PineValue::Str("3.25".to_owned())));
    assert_eq!(render(Some(PineValue::Int(42))), Some(PineValue::Str("42".to_owned())));
    assert_eq!(render(Some(PineValue::Bool(true))), Some(PineValue::Str("true".to_owned())));
    assert_eq!(render(Some(PineValue::Str("hi".to_owned()))), Some(PineValue::Str("hi".to_owned())));
    assert_eq!(render(None), Some(PineValue::Str("NaN".to_owned())));
}

#[test]
fn str_predicates_and_case_match_reference() {
    let mut inputs = InputSet::new();
    let mut one = |method: &str, args: &[CoreArg]| {
        call("str", method, "s", args, &[], &mut inputs).expect("valid str call")
    };
    assert_eq!(
        one("contains", &[str_arg("FastMAs"), str_arg("MA")]),
        Some(PineValue::Bool(true))
    );
    assert_eq!(one("length", &[str_arg("abc")]), Some(PineValue::Int(3)));
    assert_eq!(one("upper", &[str_arg("wma")]), Some(PineValue::Str("WMA".to_owned())));
    assert_eq!(one("lower", &[str_arg("WMA")]), Some(PineValue::Str("wma".to_owned())));
    assert_eq!(one("trim", &[str_arg("  a  ")]), Some(PineValue::Str("a".to_owned())));
    assert_eq!(
        one("startswith", &[str_arg("FastMAs"), str_arg("Fast")]),
        Some(PineValue::Bool(true))
    );
    assert_eq!(
        one("endswith", &[str_arg("FastMAs"), str_arg("MAs")]),
        Some(PineValue::Bool(true))
    );
    assert_eq!(
        one(
            "replace",
            &[str_arg("a-b-c"), str_arg("-"), str_arg("_")]
        ),
        Some(PineValue::Str("a_b_c".to_owned()))
    );
}

#[test]
fn str_split_substring_tonumber_pin_edges() {
    let mut inputs = InputSet::new();
    let mut one = |method: &str, args: &[CoreArg]| {
        call("str", method, "s", args, &[], &mut inputs).expect("valid str call")
    };
    assert_eq!(
        one("split", &[str_arg("a,b"), str_arg(",")]),
        Some(PineValue::Array(vec![
            Some(PineValue::Str("a".to_owned())),
            Some(PineValue::Str("b".to_owned())),
        ]))
    );
    assert_eq!(
        one("substring", &[str_arg("FastMAs"), int_arg(0), int_arg(4)]),
        Some(PineValue::Str("Fast".to_owned()))
    );
    assert_eq!(
        one("tonumber", &[str_arg("3.25")]),
        Some(PineValue::Float(3.25))
    );
    assert_eq!(one("tonumber", &[str_arg("nope")]), None);
}

// ---- input.* (defaults drive numerics, presentation is inert) ----

#[test]
fn input_int_declares_bounds_and_returns_default() {
    let mut inputs = InputSet::new();
    let args = [int_arg(3), named("minval", Some(PineValue::Int(3)))];
    let out = call("input", "int", "site-a", &args, &[], &mut inputs).expect("declares");
    assert_eq!(out, Some(PineValue::Int(3)));
    // Re-declaring the same site returns the stored value, and overrides win.
    inputs.set("unused", PineValue::Bool(true)).expect_err("undeclared refuses");
    let out = call("input", "int", "site-a", &args, &[], &mut inputs).expect("idempotent");
    assert_eq!(out, Some(PineValue::Int(3)));
}

#[test]
fn input_int_maxval_refuses_out_of_range_override() {
    let mut inputs = InputSet::new();
    let args = [
        int_arg(8),
        named("minval", Some(PineValue::Int(3))),
        named("maxval", Some(PineValue::Int(200))),
    ];
    call("input", "int", "site-b", &args, &[], &mut inputs).expect("declares");
    // Overrides flow through the same bounds: 500 exceeds maxval 200.
    let refused = inputs.set("site-b", PineValue::Int(500));
    assert!(refused.is_err(), "out-of-range override refuses");
    inputs.set("site-b", PineValue::Int(100)).expect("in-range override applies");
    let out = call("input", "int", "site-b", &args, &[], &mut inputs).expect("reads back");
    assert_eq!(out, Some(PineValue::Int(100)));
}

#[test]
fn input_bool_float_string_source_passthrough() {
    let mut inputs = InputSet::new();
    let out = call(
        "input",
        "bool",
        "site-c",
        &[pos(Some(PineValue::Bool(true))), named("title", Some(PineValue::Str("show".to_owned())))],
        &[],
        &mut inputs,
    )
    .expect("bool declares");
    assert_eq!(out, Some(PineValue::Bool(true)));
    let out = call(
        "input",
        "string",
        "site-d",
        &[str_arg("-")],
        &[],
        &mut inputs,
    )
    .expect("string declares");
    assert_eq!(out, Some(PineValue::Str("-".to_owned())));
    // `input.source` evaluates its default expression per bar and stores
    // nothing: the default rides through untouched.
    let out = call("input", "source", "site-e", &[float_arg(7.5)], &[], &mut inputs)
        .expect("source passes through");
    assert_eq!(out, Some(PineValue::Float(7.5)));
}

#[test]
fn input_enum_keeps_opaque_member() {
    let mut inputs = InputSet::new();
    let member = PineValue::EnumHandle(crate::value::EnumHandle::new("Smoothings", "WMA"));
    let out = call("input", "enum", "site-f", &[pos(Some(member.clone()))], &[], &mut inputs)
        .expect("enum declares");
    assert_eq!(out, Some(member));
}

// ---- array.* (functional: every op returns the new array) ----

fn num_array(value: Option<&PineValue>) -> Vec<Option<f64>> {
    match value {
        Some(PineValue::Array(items)) => items.iter().map(|slot| num_of(slot.as_ref())).collect(),
        other => panic!("expected array, got {other:?}"),
    }
}

#[test]
fn array_from_get_size_match_targets() {
    let mut inputs = InputSet::new();
    let from = call(
        "array",
        "from",
        "s",
        &[int_arg(5_000), int_arg(15_000), int_arg(30_000)],
        &[],
        &mut inputs,
    )
    .expect("from builds");
    let size = call("array", "size", "s", &[pos(from.clone())], &[], &mut inputs).expect("size reads");
    assert_eq!(size, Some(PineValue::Int(3)));
    let second = call(
        "array",
        "get",
        "s",
        &[pos(from.clone()), int_arg(1)],
        &[],
        &mut inputs,
    )
    .expect("get reads");
    assert_eq!(num_of(second.as_ref()), Some(15_000.0));
    let oob = call(
        "array",
        "get",
        "s",
        &[pos(from.clone()), int_arg(9)],
        &[],
        &mut inputs,
    );
    assert!(oob.is_err(), "out-of-bounds get refuses");
    assert_eq!(num_array(from.as_ref()), vec![Some(5_000.0), Some(15_000.0), Some(30_000.0)]);
}

#[test]
fn array_includes_clear_binary_search_pin_edges() {
    let mut inputs = InputSet::new();
    let from = call(
        "array",
        "from",
        "s",
        &[float_arg(5_000.0), float_arg(15_000.0), float_arg(30_000.0)],
        &[],
        &mut inputs,
    )
    .expect("from builds");
    let hit = call(
        "array",
        "includes",
        "s",
        &[pos(from.clone()), float_arg(15_000.0)],
        &[],
        &mut inputs,
    )
    .expect("includes reads");
    assert_eq!(hit, Some(PineValue::Bool(true)));
    // Lower-bound insertion point, as the target library relies on it: a
    // target between slots lands between them, past-the-end reads `size`.
    let at = call(
        "array",
        "binary_search_leftmost",
        "s",
        &[pos(from.clone()), float_arg(20_000.0)],
        &[],
        &mut inputs,
    )
    .expect("searches");
    assert_eq!(at, Some(PineValue::Int(2)));
    let past = call(
        "array",
        "binary_search_leftmost",
        "s",
        &[pos(from.clone()), float_arg(99_000.0)],
        &[],
        &mut inputs,
    )
    .expect("searches");
    assert_eq!(past, Some(PineValue::Int(3)));
    let cleared = call("array", "clear", "s", &[pos(from)], &[], &mut inputs).expect("clears");
    assert_eq!(cleared, Some(PineValue::Array(Vec::new())));
}

#[test]
fn array_push_pop_slice_sort_are_functional() {
    let mut inputs = InputSet::new();
    let mut one = |method: &str, args: &[CoreArg]| {
        call("array", method, "s", args, &[], &mut inputs).expect("valid array call")
    };
    let base = one("from", &[float_arg(3.0), float_arg(1.0)]);
    let pushed = one("push", &[pos(base.clone()), float_arg(2.0)]);
    assert_eq!(num_array(pushed.as_ref()), vec![Some(3.0), Some(1.0), Some(2.0)]);
    // The input array is untouched: ops return new arrays, never mutate.
    assert_eq!(num_array(base.as_ref()), vec![Some(3.0), Some(1.0)]);
    assert_eq!(num_of(one("pop", &[pos(pushed.clone())]).as_ref()), Some(2.0));
    assert_eq!(
        num_array(one("slice", &[pos(pushed.clone()), int_arg(1), int_arg(3)]).as_ref()),
        vec![Some(1.0), Some(2.0)]
    );
    assert_eq!(num_array(one("sort", &[pos(pushed)]).as_ref()), vec![Some(1.0), Some(2.0), Some(3.0)]);
}

#[test]
fn array_reductions_skip_absent() {
    let mut inputs = InputSet::new();
    let mut one = |method: &str, args: &[CoreArg]| {
        call("array", method, "s", args, &[], &mut inputs).expect("valid array call")
    };
    let base = one("from", &[float_arg(1.0), float_arg(2.0), float_arg(3.0), float_arg(4.0)]);
    assert_eq!(num_of(one("sum", &[pos(base.clone())]).as_ref()), Some(10.0));
    assert_eq!(num_of(one("avg", &[pos(base.clone())]).as_ref()), Some(2.5));
    assert_eq!(num_of(one("max", &[pos(base.clone())]).as_ref()), Some(4.0));
    assert_eq!(num_of(one("min", &[pos(base.clone())]).as_ref()), Some(1.0));
    let unbiased = one("stdev", &[pos(base.clone()), pos(Some(PineValue::Bool(false)))]);
    assert!((num_of(unbiased.as_ref()).expect("stdev defined") - 1.290_994_448_735_805_6).abs() < 1e-12);
}

// ---- map.* (int-to-int, the scheduler's state shape) ----

fn int_type() -> PineType {
    PineType { name: "int".to_owned(), args: Vec::new() }
}

#[test]
fn map_put_get_contains_size_keys_round_trip() {
    let mut inputs = InputSet::new();
    let types = [int_type(), int_type()];
    let empty = call("map", "new", "s", &[], &types, &mut inputs).expect("map builds");
    let mut one = |method: &str, args: &[CoreArg]| {
        call("map", method, "s", args, &[], &mut inputs).expect("valid map call")
    };
    let filled = one("put", &[pos(empty), int_arg(7), int_arg(42)]);
    assert_eq!(one("get", &[pos(filled.clone()), int_arg(7)]), Some(PineValue::Int(42)));
    assert_eq!(one("get", &[pos(filled.clone()), int_arg(8)]), None);
    assert_eq!(
        one("contains", &[pos(filled.clone()), int_arg(7)]),
        Some(PineValue::Bool(true))
    );
    assert_eq!(one("size", &[pos(filled.clone())]), Some(PineValue::Int(1)));
    assert_eq!(
        one("keys", &[pos(filled)]),
        Some(PineValue::Array(vec![Some(PineValue::Int(7))]))
    );
}

// ---- color.* (opaque handles, never arithmetic) ----

#[test]
fn color_rgb_named_new_gradient_stay_opaque() {
    let mut inputs = InputSet::new();
    let mut one = |method: &str, args: &[CoreArg]| {
        call("color", method, "s", args, &[], &mut inputs).expect("valid color call")
    };
    let rgb = one("rgb", &[int_arg(250), int_arg(240), int_arg(40), int_arg(20)]);
    assert!(matches!(rgb, Some(PineValue::ColorHandle(_))));
    assert_eq!(num_of(rgb.as_ref()), None, "handles never read as numbers");
    let white = resolve_constant("color.white").expect("white resolves");
    assert_ne!(Some(white), rgb, "distinct colors mint distinct handles");
    let start = one("rgb", &[int_arg(0), int_arg(0), int_arg(0), int_arg(0)]);
    let end = one("rgb", &[int_arg(100), int_arg(0), int_arg(0), int_arg(0)]);
    let mut grad = |v: f64| {
        one(
            "from_gradient",
            &[float_arg(v), float_arg(0.0), float_arg(100.0), pos(start.clone()), pos(end.clone())],
        )
    };
    assert_eq!(grad(0.0), start, "bottom endpoint is the start color");
    assert_eq!(grad(100.0), end, "top endpoint is the end color");
}

// ---- display/size/position/timeframe constants + in_seconds ----

#[test]
fn display_constants_resolve_opaque() {
    for name in ["display.none", "display.all", "size.tiny", "size.auto", "position.top_right", "timeframe.period"] {
        let got = resolve_constant(name).expect("constant resolves");
        assert!(matches!(got, PineValue::EnumHandle(_)), "{name} is opaque");
        assert_eq!(num_of(Some(got).as_ref()), None, "{name} never reads numeric");
    }
    assert_eq!(resolve_constant("display.something"), None);
    assert_eq!(resolve_constant("close"), None);
}

#[test]
fn timeframe_in_seconds_maps_period_strings() {
    let mut inputs = InputSet::new();
    let mut one = |value: &str| {
        call("timeframe", "in_seconds", "s", &[str_arg(value)], &[], &mut inputs).expect("maps")
    };
    assert_eq!(one("5"), Some(PineValue::Int(300)));
    assert_eq!(one("D"), Some(PineValue::Int(86_400)));
    assert_eq!(one("15S"), Some(PineValue::Int(15)));
    let bad = call("timeframe", "in_seconds", "s", &[str_arg("nope")], &[], &mut inputs);
    assert!(bad.is_err(), "unparseable timeframe refuses");
}

// ---- engine wiring: namespaces dispatch, constants resolve ----

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

fn run_script(script: &str) -> crate::pine::RunOutcome {
    let bars = vec![bar("100.00", "10.00"), bar("100.00", "11.00")];
    let program = parse_program(script).expect("script parses");
    let mut engine = crate::pine::PineEngine::new(PineRuntimeConfig::new().build());
    engine.run(&program, &bars, InputSet::new()).expect("script runs")
}

#[test]
fn engine_dispatches_scalar_namespaces() {
    let outcome = run_script("x = math.max(1, 5)\ns = str.tostring(3.25)\n");
    assert_eq!(num_of(outcome.value_at(0, "x").expect("x bound").as_ref()), Some(5.0));
    assert_eq!(
        outcome.value_at(0, "s").expect("s bound"),
        Some(PineValue::Str("3.25".to_owned()))
    );
}

#[test]
fn engine_declares_inputs_and_resolves_display_constants() {
    let outcome = run_script("len = input.int(3, minval=3)\nd = display.none\nc = color.rgb(1, 2, 3)\n");
    assert_eq!(num_of(outcome.value_at(0, "len").expect("len bound").as_ref()), Some(3.0));
    assert!(matches!(
        outcome.value_at(0, "d").expect("d bound"),
        Some(PineValue::EnumHandle(_))
    ));
    assert!(matches!(
        outcome.value_at(0, "c").expect("c bound"),
        Some(PineValue::ColorHandle(_))
    ));
}
