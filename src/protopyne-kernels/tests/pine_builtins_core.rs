//! Scalar and structural namespace tests: `math.*`, `str.*`, `input.*`,
//! `array.*`, `map.*`, `color.*`, `timeframe.*`, plus the opaque display
//! constants (`display.*`, `size.*`, `position.*`).
//!
//! Scalar math expectations on the reference overlap are transcribed from
//! the reference runtime's own unit corpus as literals; absence propagates
//! (`na` in, `na` out) and kind mismatches refuse instead of coercing.

use super::*;
use crate::pine::syntax::PineType;
use crate::{inputs::InputSet, parse_program, PineRuntimeConfig};
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
    assert_eq!(
        math_num("max", &[float_arg(1.0), float_arg(5.0)]),
        Some(5.0)
    );
    assert_eq!(
        math_num("max", &[float_arg(1.0), float_arg(5.0), float_arg(3.0)]),
        Some(5.0)
    );
    assert_eq!(
        math_num("min", &[float_arg(1.0), float_arg(5.0)]),
        Some(1.0)
    );
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
    assert_eq!(
        math_num("pow", &[float_arg(2.0), float_arg(10.0)]),
        Some(1024.0)
    );
}

#[test]
fn math_sqrt_log_exp_sign_match_reference() {
    assert_eq!(math_num("sqrt", &[float_arg(16.0)]), Some(4.0));
    assert_eq!(math_num("sqrt", &[float_arg(-1.0)]), None);
    assert_eq!(
        math_num("log", &[float_arg(std::f64::consts::E)]),
        Some(1.0)
    );
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
    assert!(
        message.contains("math.abs"),
        "arity names the function: {message}"
    );
    let message = run_err("math", "abs", &[str_arg("x")]);
    assert!(
        message.contains("math.abs"),
        "kind names the function: {message}"
    );
    let message = run_err("math", "no_such_fn", &[float_arg(1.0)]);
    assert!(
        message.contains("math.no_such_fn"),
        "unknown names itself: {message}"
    );
    let message = run_err("nope", "abs", &[float_arg(1.0)]);
    assert!(
        message.contains("nope.abs"),
        "unknown namespace names itself: {message}"
    );
}

// ---- str.* (reference overlap, transcribed) ----

#[test]
fn str_tostring_renders_scalars() {
    let mut inputs = InputSet::new();
    let mut render = |value: Option<PineValue>| {
        call("str", "tostring", "s", &[pos(value)], &[], &mut inputs).expect("renders")
    };
    assert_eq!(
        render(Some(PineValue::Float(3.25))),
        Some(PineValue::Str("3.25".to_owned()))
    );
    assert_eq!(
        render(Some(PineValue::Int(42))),
        Some(PineValue::Str("42".to_owned()))
    );
    assert_eq!(
        render(Some(PineValue::Bool(true))),
        Some(PineValue::Str("true".to_owned()))
    );
    assert_eq!(
        render(Some(PineValue::Str("hi".to_owned()))),
        Some(PineValue::Str("hi".to_owned()))
    );
    assert_eq!(render(None), Some(PineValue::Str("NaN".to_owned())));
}

#[test]
fn str_tostring_capped_decimal_family() {
    // `"#"` integers, `"#."` plus N hashes caps at N decimals with
    // trailing zeros trimmed; anything else refuses naming the subset.
    let mut inputs = InputSet::new();
    let mut render = |value: Option<PineValue>, pattern: &str| {
        call(
            "str",
            "tostring",
            "s",
            &[pos(value), pos(Some(PineValue::Str(pattern.to_owned())))],
            &[],
            &mut inputs,
        )
        .map_err(|err| err.to_string())
    };
    assert_eq!(
        render(Some(PineValue::Float(3.26)), "#.#"),
        Ok(Some(PineValue::Str("3.3".to_owned()))),
        "one hash caps at one decimal"
    );
    assert_eq!(
        render(Some(PineValue::Float(3.20)), "#.#"),
        Ok(Some(PineValue::Str("3.2".to_owned()))),
        "trailing zeros trim"
    );
    assert_eq!(
        render(Some(PineValue::Float(3.257)), "#.##"),
        Ok(Some(PineValue::Str("3.26".to_owned()))),
        "two hashes keep prior behavior"
    );
    assert_eq!(
        render(Some(PineValue::Float(1.23456)), "#.###"),
        Ok(Some(PineValue::Str("1.235".to_owned()))),
        "three hashes cap at three"
    );
    assert_eq!(
        render(None, "#.#"),
        Ok(Some(PineValue::Str("NaN".to_owned()))),
        "absence stays absence"
    );
    assert_eq!(
        render(Some(PineValue::Float(1.0)), "#,##"),
        Err("pine evaluation refused: str.tostring takes a '#' or '#.#...' decimal format, got \"#,##\"".to_owned()),
        "non-hash pattern refuses"
    );
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
    assert_eq!(
        one("upper", &[str_arg("wma")]),
        Some(PineValue::Str("WMA".to_owned()))
    );
    assert_eq!(
        one("lower", &[str_arg("WMA")]),
        Some(PineValue::Str("wma".to_owned()))
    );
    assert_eq!(
        one("trim", &[str_arg("  a  ")]),
        Some(PineValue::Str("a".to_owned()))
    );
    assert_eq!(
        one("startswith", &[str_arg("FastMAs"), str_arg("Fast")]),
        Some(PineValue::Bool(true))
    );
    assert_eq!(
        one("endswith", &[str_arg("FastMAs"), str_arg("MAs")]),
        Some(PineValue::Bool(true))
    );
    assert_eq!(
        one("replace", &[str_arg("a-b-c"), str_arg("-"), str_arg("_")]),
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
    inputs
        .set("unused", PineValue::Bool(true))
        .expect_err("undeclared refuses");
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
    inputs
        .set("site-b", PineValue::Int(100))
        .expect("in-range override applies");
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
        &[
            pos(Some(PineValue::Bool(true))),
            named("title", Some(PineValue::Str("show".to_owned()))),
        ],
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
    let out = call(
        "input",
        "source",
        "site-e",
        &[float_arg(7.5)],
        &[],
        &mut inputs,
    )
    .expect("source passes through");
    assert_eq!(out, Some(PineValue::Float(7.5)));
}

#[test]
fn input_enum_keeps_opaque_member() {
    let mut inputs = InputSet::new();
    let member = PineValue::EnumHandle(crate::value::EnumHandle::new("Smoothings", "WMA"));
    let out = call(
        "input",
        "enum",
        "site-f",
        &[pos(Some(member.clone()))],
        &[],
        &mut inputs,
    )
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
    let size =
        call("array", "size", "s", &[pos(from.clone())], &[], &mut inputs).expect("size reads");
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
    assert_eq!(
        num_array(from.as_ref()),
        vec![Some(5_000.0), Some(15_000.0), Some(30_000.0)]
    );
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
    assert_eq!(
        num_array(pushed.as_ref()),
        vec![Some(3.0), Some(1.0), Some(2.0)]
    );
    // The input array is untouched: ops return new arrays, never mutate.
    assert_eq!(num_array(base.as_ref()), vec![Some(3.0), Some(1.0)]);
    assert_eq!(
        num_of(one("pop", &[pos(pushed.clone())]).as_ref()),
        Some(2.0)
    );
    assert_eq!(
        num_array(one("slice", &[pos(pushed.clone()), int_arg(1), int_arg(3)]).as_ref()),
        vec![Some(1.0), Some(2.0)]
    );
    assert_eq!(
        num_array(one("sort", &[pos(pushed)]).as_ref()),
        vec![Some(1.0), Some(2.0), Some(3.0)]
    );
}

#[test]
fn array_reductions_skip_absent() {
    let mut inputs = InputSet::new();
    let mut one = |method: &str, args: &[CoreArg]| {
        call("array", method, "s", args, &[], &mut inputs).expect("valid array call")
    };
    let base = one(
        "from",
        &[
            float_arg(1.0),
            float_arg(2.0),
            float_arg(3.0),
            float_arg(4.0),
        ],
    );
    assert_eq!(
        num_of(one("sum", &[pos(base.clone())]).as_ref()),
        Some(10.0)
    );
    assert_eq!(num_of(one("avg", &[pos(base.clone())]).as_ref()), Some(2.5));
    assert_eq!(num_of(one("max", &[pos(base.clone())]).as_ref()), Some(4.0));
    assert_eq!(num_of(one("min", &[pos(base.clone())]).as_ref()), Some(1.0));
    let unbiased = one(
        "stdev",
        &[pos(base.clone()), pos(Some(PineValue::Bool(false)))],
    );
    assert!(
        (num_of(unbiased.as_ref()).expect("stdev defined") - 1.290_994_448_735_805_6).abs() < 1e-12
    );
}

// ---- map.* (int-to-int, the scheduler's state shape) ----

fn int_type() -> PineType {
    PineType {
        name: "int".to_owned(),
        args: Vec::new(),
        array_depth: 0,
    }
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
    assert_eq!(
        one("get", &[pos(filled.clone()), int_arg(7)]),
        Some(PineValue::Int(42))
    );
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
    let rgb = one(
        "rgb",
        &[int_arg(250), int_arg(240), int_arg(40), int_arg(20)],
    );
    assert!(matches!(rgb, Some(PineValue::ColorHandle(_))));
    assert_eq!(num_of(rgb.as_ref()), None, "handles never read as numbers");
    let white = resolve_constant("color.white").expect("white resolves");
    assert_ne!(Some(white), rgb, "distinct colors mint distinct handles");
    let start = one("rgb", &[int_arg(0), int_arg(0), int_arg(0), int_arg(0)]);
    let end = one("rgb", &[int_arg(100), int_arg(0), int_arg(0), int_arg(0)]);
    let mut grad = |v: f64| {
        one(
            "from_gradient",
            &[
                float_arg(v),
                float_arg(0.0),
                float_arg(100.0),
                pos(start.clone()),
                pos(end.clone()),
            ],
        )
    };
    assert_eq!(grad(0.0), start, "bottom endpoint is the start color");
    assert_eq!(grad(100.0), end, "top endpoint is the end color");
}

#[test]
fn color_new_absent_transparency_propagates() {
    // `color.new(color, na)` reads `na` (the transparency is a value
    // parameter, so absence propagates); only a non-color first argument
    // refuses. RSIMD ramps transparency off a warming series.
    let mut inputs = InputSet::new();
    let white = resolve_constant("color.white").expect("white resolves");
    let absent = call(
        "color",
        "new",
        "s",
        &[pos(Some(white.clone())), pos(None)],
        &[],
        &mut inputs,
    )
    .expect("absent transparency evaluates");
    assert_eq!(absent, None, "na transparency propagates");
    let present = call(
        "color",
        "new",
        "s",
        &[pos(Some(white.clone())), pos(Some(PineValue::Int(50)))],
        &[],
        &mut inputs,
    )
    .expect("present transparency evaluates");
    assert!(matches!(present, Some(PineValue::ColorHandle(_))));
}

// ---- display/size/position/timeframe constants + in_seconds ----

#[test]
fn display_constants_resolve_opaque() {
    // Display flags lower as engine-local record integers (none 0 through
    // all 15) so the documented Pine combining (`display.all -
    // display.status_line`) runs through ordinary arithmetic; captures
    // only record them. Every other display namespace stays opaque.
    for (name, flag) in [
        ("display.none", 0),
        ("display.data_window", 1),
        ("display.price_scale", 2),
        ("display.status_line", 4),
        ("display.pane", 8),
        ("display.all", 15),
    ] {
        let got = resolve_constant(name).expect("constant resolves");
        assert_eq!(got, PineValue::Int(flag), "{name} records flag {flag}");
    }
    for name in ["size.tiny", "size.auto", "position.top_right"] {
        let got = resolve_constant(name).expect("constant resolves");
        assert!(matches!(got, PineValue::EnumHandle(_)), "{name} is opaque");
        assert_eq!(
            num_of(Some(got).as_ref()),
            None,
            "{name} never reads numeric"
        );
    }
    assert_eq!(
        resolve_constant("timeframe.period"),
        None,
        "chart scope resolves dynamically, never statically"
    );
    assert_eq!(resolve_constant("display.something"), None);
    assert_eq!(resolve_constant("close"), None);
}

#[test]
fn timeframe_in_seconds_maps_period_strings() {
    let mut inputs = InputSet::new();
    let mut one = |value: &str| {
        call(
            "timeframe",
            "in_seconds",
            "s",
            &[str_arg(value)],
            &[],
            &mut inputs,
        )
        .expect("maps")
    };
    assert_eq!(one("5"), Some(PineValue::Int(300)));
    assert_eq!(one("D"), Some(PineValue::Int(86_400)));
    assert_eq!(one("15S"), Some(PineValue::Int(15)));
    let bad = call(
        "timeframe",
        "in_seconds",
        "s",
        &[str_arg("nope")],
        &[],
        &mut inputs,
    );
    assert!(bad.is_err(), "unparseable timeframe refuses");
}

#[test]
fn input_timeframe_answers_its_period_string() {
    // The default validates against the chart-period parser and reads
    // back as text; overrides replace it wholesale.
    let mut inputs = InputSet::new();
    let out = call(
        "input",
        "timeframe",
        "site-tf",
        &[str_arg("D")],
        &[],
        &mut inputs,
    )
    .expect("timeframe declares");
    assert_eq!(out, Some(PineValue::Str("D".to_owned())));
    let out = call(
        "input",
        "timeframe",
        "site-tf",
        &[str_arg("D")],
        &[],
        &mut inputs,
    )
    .expect("re-declare reads back");
    assert_eq!(out, Some(PineValue::Str("D".to_owned())));
    inputs
        .set("site-tf", PineValue::Str("60".to_owned()))
        .expect("override applies");
    let out = call(
        "input",
        "timeframe",
        "site-tf",
        &[str_arg("D")],
        &[],
        &mut inputs,
    )
    .expect("override wins");
    assert_eq!(out, Some(PineValue::Str("60".to_owned())));
    // Non-string defaults and unparseable periods refuse with reasons.
    let mut inputs = InputSet::new();
    let bad = call(
        "input",
        "timeframe",
        "site-bad",
        &[int_arg(5)],
        &[],
        &mut inputs,
    );
    assert!(bad.is_err(), "non-string default refuses");
    let bad = call(
        "input",
        "timeframe",
        "site-bad2",
        &[str_arg("nope")],
        &[],
        &mut inputs,
    );
    assert!(bad.is_err(), "unparseable period refuses");
}

#[test]
fn timeframe_bucket_counts_sub_daily_and_calendar_units() {
    use super::timeframe_bucket;
    use chrono::TimeZone as _;
    let epoch = |y: i32, mo: u32, d: u32, h: u32, mi: u32| {
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid test timestamp")
            .timestamp()
    };
    // Friday 2026-01-02T09:00:00Z; Saturday midnight; Monday Jan 5;
    // February 1 (January 2026 opens on a Thursday).
    let fri_09 = epoch(2026, 1, 2, 9, 0);
    let sat_00 = epoch(2026, 1, 3, 0, 0);
    let mon_05 = epoch(2026, 1, 5, 0, 0);
    let feb_01 = epoch(2026, 2, 1, 0, 0);
    // Minute buckets floor-divide: the same bar twice agrees, the next
    // minute disagrees.
    assert_eq!(
        timeframe_bucket("1", fri_09).expect("minute buckets"),
        timeframe_bucket("1", fri_09 + 59).expect("same minute")
    );
    assert_ne!(
        timeframe_bucket("1", fri_09).expect("minute buckets"),
        timeframe_bucket("1", fri_09 + 60).expect("next minute")
    );
    // Day buckets split at UTC midnight exactly.
    assert_eq!(
        timeframe_bucket("D", fri_09).expect("day buckets"),
        timeframe_bucket("D", sat_00 - 1).expect("same UTC day")
    );
    assert_ne!(
        timeframe_bucket("D", fri_09).expect("day buckets"),
        timeframe_bucket("D", sat_00).expect("next UTC day")
    );
    // Monday-start weeks: Friday Jan 2 and Saturday Jan 3 share a week;
    // Monday Jan 5 opens the next one.
    assert_eq!(
        timeframe_bucket("W", fri_09).expect("week buckets"),
        timeframe_bucket("W", sat_00).expect("same week")
    );
    assert_ne!(
        timeframe_bucket("W", sat_00).expect("week buckets"),
        timeframe_bucket("W", mon_05).expect("next week")
    );
    // Calendar months: January and February disagree; quarters agree
    // within Q1 and split at April.
    assert_ne!(
        timeframe_bucket("M", fri_09).expect("month buckets"),
        timeframe_bucket("M", feb_01).expect("next month")
    );
    assert_eq!(
        timeframe_bucket("3M", fri_09).expect("quarter buckets"),
        timeframe_bucket("3M", feb_01).expect("same quarter")
    );
    // Shapes outside the served set refuse with reasons, never silently.
    assert!(timeframe_bucket("2W", fri_09).is_err(), "2W refuses");
    assert!(timeframe_bucket("1H", fri_09).is_err(), "H suffix refuses");
    assert!(timeframe_bucket("nope", fri_09).is_err(), "junk refuses");
}

// ---- engine wiring: namespaces dispatch, constants resolve ----

fn bar(open: &str, close: &str) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .with_ymd_and_hms(2026, 1, 2, 9, 0, 0)
            .single()
            .expect("valid test timestamp"))
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
    engine
        .run(&program, &bars, InputSet::new())
        .expect("script runs")
}

#[test]
fn engine_dispatches_scalar_namespaces() {
    let outcome = run_script("x = math.max(1, 5)\ns = str.tostring(3.25)\n");
    assert_eq!(
        num_of(outcome.value_at(0, "x").expect("x bound").as_ref()),
        Some(5.0)
    );
    assert_eq!(
        outcome.value_at(0, "s").expect("s bound"),
        Some(PineValue::Str("3.25".to_owned()))
    );
}

#[test]
fn engine_declares_inputs_and_resolves_display_constants() {
    let outcome =
        run_script("len = input.int(3, minval=3)\nd = display.none\nc = color.rgb(1, 2, 3)\n");
    assert_eq!(
        num_of(outcome.value_at(0, "len").expect("len bound").as_ref()),
        Some(3.0)
    );
    assert_eq!(
        outcome.value_at(0, "d").expect("d bound"),
        Some(PineValue::Int(0)),
        "display.none records flag 0"
    );
    assert!(matches!(
        outcome.value_at(0, "c").expect("c bound"),
        Some(PineValue::ColorHandle(_))
    ));
}

#[test]
fn color_channel_readers_report_packed_channels() {
    // `color.t/r/g/b` read back the packed lanes (`t` is 0-100, the rest
    // 0-255); absence propagates and non-colors refuse.
    let mut inputs = InputSet::new();
    let mut one = |method: &str, args: &[CoreArg]| {
        call("color", method, "s", args, &[], &mut inputs).expect("valid color call")
    };
    let color = one(
        "rgb",
        &[int_arg(10), int_arg(20), int_arg(30), int_arg(40)],
    );
    assert_eq!(
        one("t", &[pos(color.clone())]),
        Some(PineValue::Int(40)),
        "transparency lane reads 0-100"
    );
    assert_eq!(
        one("r", &[pos(color.clone())]),
        Some(PineValue::Int(10)),
        "red lane reads 0-255"
    );
    assert_eq!(
        one("g", &[pos(color.clone())]),
        Some(PineValue::Int(20)),
        "green lane reads 0-255"
    );
    assert_eq!(
        one("b", &[pos(color.clone())]),
        Some(PineValue::Int(30)),
        "blue lane reads 0-255"
    );
    assert_eq!(
        one("t", &[pos(None)]),
        None,
        "absent color propagates"
    );
    let err = call(
        "color",
        "t",
        "s",
        &[float_arg(1.5)],
        &[],
        &mut inputs,
    )
    .expect_err("a non-color refuses");
    assert!(
        err.to_string().contains("color.t takes a color"),
        "the refusal names the call: {err}"
    );
}

#[test]
fn legacy_bare_colors_resolve_as_their_dotted_handles() {
    // Pre-namespace scripts spell colors bare (`lime`, `red`, ...): each
    // bare name mints the identical handle as its `color.*` spelling, so
    // both engines serve one color identity. Anything outside the legacy
    // set stays unbound.
    for bare in [
        "white", "black", "red", "green", "blue", "orange", "teal", "aqua", "gray", "lime",
        "yellow", "silver", "maroon", "olive", "navy", "purple", "fuchsia",
    ] {
        let dotted = format!("color.{bare}");
        assert_eq!(
            resolve_constant(bare),
            resolve_constant(&dotted),
            "{bare} mints the {dotted} handle"
        );
    }
    assert_eq!(resolve_constant("chartreuse"), None);
}

#[test]
fn math_degree_radian_conversions_match_reference() {
    // `math.todegrees`/`math.toradians` convert between radians and
    // degrees; absence propagates.
    assert_eq!(
        math_num("todegrees", &[float_arg(std::f64::consts::PI)]),
        Some(180.0)
    );
    assert_eq!(
        math_num("toradians", &[float_arg(180.0)]),
        Some(std::f64::consts::PI)
    );
    assert_eq!(run_math("todegrees", &[pos(None)]), None);
}

#[test]
fn bare_type_names_read_as_their_spelling() {
    // Legacy scripts pass `type=bool` to `input(...)`: the bare type
    // keywords name their type as strings (the input kind still infers
    // from the default — the tag carries no additional information, so
    // every input arm ignores it). Anything outside the keyword set
    // stays unbound.
    for keyword in ["bool", "int", "float", "string"] {
        assert_eq!(
            resolve_constant(keyword),
            Some(PineValue::Str(keyword.to_owned())),
            "{keyword} names its type"
        );
    }
    assert_eq!(resolve_constant("chart"), None);
}

#[test]
fn xloc_members_resolve_opaque() {
    // Plot/drawing x-location selectors (`xloc.bar_time`,
    // `xloc.bar_index`) ride calls opaquely: the engine indexes plots by
    // series position, so the selector is recorded, never decided on.
    for member in ["bar_time", "bar_index"] {
        let name = format!("xloc.{member}");
        let got = resolve_constant(&name).expect("xloc resolves");
        assert!(matches!(got, PineValue::EnumHandle(_)), "{name} is opaque");
    }
    assert_eq!(resolve_constant("xloc.something"), None);
}

#[test]
fn plot_styles_resolve_bare_and_qualified() {
    // Plot styles ride `plot()` calls unevaluated-but-present: every
    // legacy bare spelling mints the identical handle as its
    // `plot.style_*` form, so both engines serve one style identity.
    for (bare, member) in [
        ("line", "style_line"),
        ("linebr", "style_linebr"),
        ("stepline", "style_stepline"),
        ("steplinebr", "style_steplinebr"),
        ("histogram", "style_histogram"),
        ("columns", "style_columns"),
        ("cross", "style_cross"),
        ("area", "style_area"),
        ("circles", "style_circles"),
    ] {
        let dotted = format!("plot.{member}");
        let got = resolve_constant(bare).expect("bare style resolves");
        assert_eq!(got, resolve_constant(&dotted).expect("dotted style resolves"));
        assert!(matches!(got, PineValue::EnumHandle(_)), "{bare} is opaque");
    }
    assert_eq!(resolve_constant("plot.style_something"), None);
}

#[test]
fn timeframe_from_seconds_inverts_in_seconds() {
    // `timeframe.from_seconds` renders the canonical period string for a
    // second count (minutes bare, sub-minute seconds with `S`, day-plus
    // units bare at one): every rendering round-trips through
    // `in_seconds`, and non-positive counts refuse.
    let mut inputs = InputSet::new();
    for (seconds, text) in [(60, "1"), (300, "5"), (45, "45S"), (86_400, "D")] {
        let rendered = match call(
            "timeframe",
            "from_seconds",
            "s",
            &[int_arg(seconds)],
            &[],
            &mut InputSet::new(),
        )
        .expect("renders")
        {
            Some(PineValue::Str(rendered)) => rendered,
            other => panic!("renders a string, got {other:?}"),
        };
        assert_eq!(rendered, text, "{seconds}s renders {text}");
        let back = call(
            "timeframe",
            "in_seconds",
            "s",
            &[str_arg(&rendered)],
            &[],
            &mut InputSet::new(),
        )
        .expect("parses");
        assert_eq!(
            back,
            Some(PineValue::Int(seconds)),
            "{text} parses back to {seconds}"
        );
    }
    let err = call(
        "timeframe",
        "from_seconds",
        "s",
        &[int_arg(0)],
        &[],
        &mut inputs,
    )
    .expect_err("zero refuses");
    assert!(
        err.to_string().contains("from_seconds"),
        "the refusal names the call: {err}"
    );
}

#[test]
fn input_string_accepts_layout_enum_handles() {
    // Shipped scripts pass `size.tiny` as an `input.string` default: the
    // layout enums read as strings at the input boundary (their member
    // text), exactly like the reference runtime's string constants. The
    // stored default is the member text; other handles still refuse.
    let mut inputs = InputSet::new();
    let tiny = resolve_constant("size.tiny").expect("size resolves");
    let got = call(
        "input",
        "string",
        "s",
        &[pos(Some(tiny)), pos(Some(PineValue::Str("T".to_owned())))],
        &[],
        &mut inputs,
    )
    .expect("enum default evaluates");
    assert_eq!(
        got,
        Some(PineValue::Str("tiny".to_owned())),
        "the member text stores"
    );
}

#[test]
fn array_typed_constructors_mint_empty_arrays() {
    // `array.new_line()` (and its drawing siblings) mint empty arrays:
    // element typing is declarative — pushes never check it — so every
    // typed constructor serves the same empty array as `array.new()`.
    let mut inputs = InputSet::new();
    for method in ["new_line", "new_linefill", "new_box", "new_label"] {
        let got = call("array", method, "s", &[], &[], &mut inputs).expect("constructs");
        assert_eq!(
            got,
            Some(PineValue::Array(Vec::new())),
            "{method} mints empty"
        );
    }
}
