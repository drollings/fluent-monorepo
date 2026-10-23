use super::*;
use crate::parity::assert_expr_parity;
use crate::research::frame_of_values;
use polars::prelude::{col, lit, when, Expr, IntoLazy, NamedFrom as _, RoundMode};

fn vals(values: &[f64]) -> Vec<Option<f64>> {
    values.iter().map(|v| Some(*v)).collect()
}

/// Trend-shaped scale series, same family as the series-combinator suites.
fn series(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 100.0 + i as f64 * 0.7 + ((i * 13) % 5) as f64 * 0.9)
        .collect()
}

/// Small-magnitude probe for transcendental Expr-parity: absolute epsilons are
/// meaningless on `exp(100)`-scale outputs, so growth functions meet the
/// reference where both sides are small and exact.
fn small() -> Vec<f64> {
    (-10..11).map(|i| f64::from(i) * 0.25).collect()
}

/// Single-column funnel mirroring the series suites: first selected column
/// out with the same null/NaN mapping (`NaN` reads `None`, infinities pass
/// through as values so guarded-vs-raw departures stay observable).
fn series_fixture(values: &[Option<f64>], expr: Expr) -> Vec<Option<f64>> {
    use crate::indicator::fin_or_none;
    let frame = frame_of_values("x", values).expect("frame");
    let selected = frame.lazy().select([expr]).collect().expect("select");
    assert_eq!(selected.height(), values.len(), "height preserved");
    let first = selected.get_columns().first().expect("column").clone();
    let cast = first
        .cast(&polars::prelude::DataType::Float64)
        .expect("cast");
    let chunked = cast.f64().expect("f64");
    (0..chunked.len())
        .map(|i| chunked.get(i).and_then(fin_or_none))
        .collect()
}

/// Three-column funnel for horizontal expressions over `x`, `y`, `z`.
fn triple_fixture(
    x: &[f64],
    y: &[f64],
    z: &[f64],
    expr: impl Fn(Expr, Expr, Expr) -> Expr,
) -> Vec<Option<f64>> {
    use crate::indicator::fin_or_none;
    use polars::prelude::DataFrame;
    let frame = DataFrame::new(vec![
        polars::prelude::Series::new("x".into(), vals(x)).into(),
        polars::prelude::Series::new("y".into(), vals(y)).into(),
        polars::prelude::Series::new("z".into(), vals(z)).into(),
    ])
    .expect("triple frame");
    let selected = frame
        .lazy()
        .select([expr(col("x"), col("y"), col("z"))])
        .collect()
        .expect("select");
    let first = selected.get_columns().first().expect("column").clone();
    let cast = first
        .cast(&polars::prelude::DataType::Float64)
        .expect("cast");
    let chunked = cast.f64().expect("f64");
    (0..chunked.len())
        .map(|i| chunked.get(i).and_then(fin_or_none))
        .collect()
}
fn pair_fixture(x: &[f64], y: &[f64], expr: impl Fn(Expr, Expr) -> Expr) -> Vec<Option<f64>> {
    use crate::indicator::fin_or_none;
    use polars::prelude::DataFrame;
    let frame = DataFrame::new(vec![
        polars::prelude::Series::new("x".into(), vals(x)).into(),
        polars::prelude::Series::new("y".into(), vals(y)).into(),
    ])
    .expect("pair frame");
    let selected = frame
        .lazy()
        .select([expr(col("x"), col("y"))])
        .collect()
        .expect("select");
    let first = selected.get_columns().first().expect("column").clone();
    let cast = first
        .cast(&polars::prelude::DataType::Float64)
        .expect("cast");
    let chunked = cast.f64().expect("f64");
    (0..chunked.len())
        .map(|i| chunked.get(i).and_then(fin_or_none))
        .collect()
}

fn check(name: &str, got: &[Option<f64>], want: &[Option<f64>], eps: f64) {
    assert_eq!(got.len(), want.len(), "{name} length");
    assert_expr_parity(got, want, eps, name).expect("parity");
}

#[test]
fn unary_math_matches_elementwise_forms() {
    let data = series(20);
    let opt = vals(&data);
    check(
        "abs",
        &comb_abs(&data),
        &series_fixture(&opt, col("x").abs()),
        1e-12,
    );
    check(
        "sqrt",
        &comb_sqrt(&data),
        &series_fixture(&opt, col("x").sqrt()),
        1e-12,
    );
    check(
        "cbrt",
        &comb_cbrt(&data),
        &series_fixture(&opt, col("x").cbrt()),
        1e-12,
    );
    check(
        "floor",
        &comb_floor(&data),
        &series_fixture(&opt, col("x").floor()),
        0.0,
    );
    check(
        "ceiling",
        &comb_ceiling(&data),
        &series_fixture(&opt, col("x").ceil()),
        0.0,
    );
    check(
        "round-0",
        &comb_round(&data, 0),
        &series_fixture(&opt, col("x").round(0, RoundMode::HalfAwayFromZero)),
        0.0,
    );
    check(
        "log",
        &comb_log(&data),
        &series_fixture(&opt, col("x").log(std::f64::consts::E)),
        1e-12,
    );
    check(
        "log10",
        &comb_log10(&data),
        &series_fixture(&opt, col("x").log(10.0)),
        1e-12,
    );
    check(
        "log2",
        &comb_log2(&data),
        &series_fixture(&opt, col("x").log(2.0)),
        1e-12,
    );
    check(
        "log1p",
        &comb_log1p(&data),
        &series_fixture(&opt, col("x").log1p()),
        1e-12,
    );
    // Growth and periodic functions meet the reference on small magnitudes,
    // where absolute epsilons still discriminate.
    let tiny = small();
    let tiny_opt = vals(&tiny);
    check(
        "exp",
        &comb_exp(&tiny),
        &series_fixture(&tiny_opt, col("x").exp()),
        1e-9,
    );
    check(
        "sin",
        &comb_sin(&tiny),
        &series_fixture(&tiny_opt, col("x").sin()),
        1e-12,
    );
    check(
        "cos",
        &comb_cos(&tiny),
        &series_fixture(&tiny_opt, col("x").cos()),
        1e-12,
    );
    check(
        "tan",
        &comb_tan(&tiny),
        &series_fixture(&tiny_opt, col("x").tan()),
        1e-9,
    );
    check(
        "sinh",
        &comb_sinh(&tiny),
        &series_fixture(&tiny_opt, col("x").sinh()),
        1e-9,
    );
    check(
        "cosh",
        &comb_cosh(&tiny),
        &series_fixture(&tiny_opt, col("x").cosh()),
        1e-9,
    );
    check(
        "tanh",
        &comb_tanh(&tiny),
        &series_fixture(&tiny_opt, col("x").tanh()),
        1e-12,
    );
    // Reference tables pin the real math, not just the plumbing: the
    // docstring values below are the upstream authority (rounded display
    // values, not the constants themselves).
    #[allow(clippy::approx_constant)]
    let exp_table = [0.367_879, 1.0, 2.718_282, 7.389_056];
    check(
        "exp-table",
        &comb_exp(&[-1.0, 0.0, 1.0, 2.0]),
        &vals(&exp_table),
        1e-6,
    );
    check(
        "expm1-table",
        &comb_expm1(&[-1.0, 0.0, 1.0, 2.0]),
        &vals(&[-0.632_121, 0.0, 1.718_282, 6.389_056]),
        1e-6,
    );
    check(
        "tanh-table",
        &comb_tanh(&[-1.0, 0.0, 1.0, 2.0]),
        &vals(&[-0.761_594, 0.0, 0.761_594, 0.964_028]),
        1e-6,
    );
    check(
        "degrees-table",
        &comb_degrees(&[0.0, std::f64::consts::FRAC_PI_2, std::f64::consts::PI]),
        &vals(&[0.0, 90.0, 180.0]),
        1e-9,
    );
    check(
        "radians-table",
        &comb_radians(&[0.0, 90.0, 180.0]),
        &vals(&[0.0, std::f64::consts::FRAC_PI_2, std::f64::consts::PI]),
        1e-12,
    );
    // Non-finite outcomes pass through as values everywhere except the
    // guarded division: the reference tables pin `0 -> inf` (inverse),
    // `0 -> -inf` (`log`, `log1p`), and only `NaN` normalizes to `None`.
    assert_eq!(comb_sqrt(&[-1.0]), vec![None]);
    assert_eq!(comb_log(&[-1.0, 0.0]), vec![None, Some(f64::NEG_INFINITY)]);
    assert_eq!(comb_log1p(&[-1.0]), vec![Some(f64::NEG_INFINITY)]);
    assert_eq!(comb_inverse(&[0.0]), vec![Some(f64::INFINITY)]);
    assert_eq!(comb_cot(&[0.0]), vec![Some(f64::INFINITY)]);
    assert_eq!(comb_exp(&[1000.0]), vec![Some(f64::INFINITY)]);
    assert_eq!(comb_power(&[-1.0], &[0.5]), vec![None]);
    assert_eq!(comb_logit(&[0.0]), vec![Some(f64::NEG_INFINITY)]);
}

#[test]
fn inverse_trig_and_squares_match() {
    let unit = [-0.5, 0.0, 0.5];
    let opt = vals(&unit);
    check(
        "asin",
        &comb_arc_sin(&unit),
        &series_fixture(&opt, col("x").arcsin()),
        1e-12,
    );
    check(
        "acos",
        &comb_arc_cos(&unit),
        &series_fixture(&opt, col("x").arccos()),
        1e-12,
    );
    let wide = [-1.0, 0.0, 1.0];
    let wide_opt = vals(&wide);
    check(
        "atan",
        &comb_arc_tan(&wide),
        &series_fixture(&wide_opt, col("x").arctan()),
        1e-12,
    );
    check(
        "atan2",
        &comb_arc_tan2(&wide, &wide),
        &pair_fixture(&wide, &wide, Expr::arctan2),
        1e-12,
    );
    let data = series(10);
    let opt = vals(&data);
    check(
        "square",
        &comb_square(&data),
        &series_fixture(&opt, col("x").clone() * col("x")),
        1e-9,
    );
    check(
        "cube",
        &comb_cube(&data),
        &series_fixture(&opt, col("x").clone() * col("x").clone() * col("x")),
        1e-9,
    );
    // Out-of-domain inverse trig refuses.
    assert_eq!(comb_arc_sin(&[2.0]), vec![None]);
    assert_eq!(comb_arc_cos(&[-2.0]), vec![None]);
}

#[test]
fn docstring_tables_pin_reference_values() {
    let nan = f64::NAN;
    // Horizontal add: null-ignoring, `None` only where every leg is missing.
    let (a, b, c) = (
        &[nan, 2.0, 3.0, 4.0, nan],
        &[5.0, nan, 3.0, 2.0, nan],
        &[1.0, 1.0, nan, 1.0, nan],
    );
    assert_eq!(
        comb_add(a, b, &[c]),
        vec![Some(6.0), Some(3.0), Some(6.0), Some(7.0), None]
    );
    assert_eq!(
        comb_multiply(a, b, &[c]),
        vec![Some(5.0), Some(2.0), Some(9.0), Some(8.0), None]
    );
    // Floor division: null with a zero divisor, floor toward negative inf.
    let fa = &[nan, -1.5, 0.0, 1.5, 2.5];
    assert_eq!(comb_div(fa, &[0.0; 5]), vec![None; 5]);
    assert_eq!(
        comb_div(fa, &[1.0; 5]),
        vec![None, Some(-2.0), Some(0.0), Some(1.0), Some(2.0)]
    );
    assert_eq!(
        comb_div(fa, &[nan, -1.0, 0.0, 1.0, 2.0]),
        vec![None, Some(1.0), None, Some(1.0), Some(1.0)]
    );
    // Guarded division refuses where the raw reference yields `inf`/`NaN`
    // (the one documented departure, shared with the operator maps) and
    // propagates missing numerators as `None`.
    let ia = &[nan, -1.0, 0.0, 1.0, 2.0];
    assert_eq!(comb_divide(ia, &[0.0; 5]), vec![None; 5]);
    assert_eq!(
        comb_divide(ia, &[1.0; 5]),
        vec![None, Some(-1.0), Some(0.0), Some(1.0), Some(2.0)]
    );
    assert_eq!(
        comb_divide(ia, ia),
        vec![None, Some(1.0), None, Some(1.0), Some(1.0)]
    );
    // Signed fraction keeps the sign of the input, including `-0.0`. The
    // docstring displays `-0.2`, but binary `1.2 % 1.0` is
    // `-0.19999999999999996` on both sides — eps, not bit-equality.
    check(
        "fraction-table",
        &comb_fraction(&[-2.5, -1.2, -1.0, nan, 2.0, 3.2]),
        &[
            Some(-0.5),
            Some(-0.2),
            Some(-0.0),
            None,
            Some(0.0),
            Some(0.2),
        ],
        1e-12,
    );
    // Half away from zero, never banker's: 4.5 -> 5, -4.5 -> -5.
    assert_eq!(
        comb_round(&[nan, 3.5, 4.5, -3.5, -4.5], 0),
        vec![None, Some(4.0), Some(5.0), Some(-4.0), Some(-5.0)]
    );
    assert_eq!(
        comb_round_down(&[nan, 3.5, 4.5, -3.5, -4.5], 2.0),
        vec![None, Some(2.0), Some(4.0), Some(-4.0), Some(-6.0)]
    );
    // Python-style remainder (sign follows the divisor on the pinned rows).
    assert_eq!(
        comb_mod(&[nan, -1.0, 0.0, 1.0, 2.0], &[2.0; 5]),
        vec![None, Some(1.0), Some(0.0), Some(1.0), Some(0.0)]
    );
    assert_eq!(
        comb_mod(&[nan, -1.0, 0.0, 1.0, 2.0], &[nan, -1.0, 0.0, 0.0, 2.0]),
        vec![None, Some(0.0), None, None, Some(0.0)]
    );
    // Sign-preserving power, with the y == 0 / y == 1 fast paths pinned.
    let sa = &[nan, -1.0, 0.0, 1.0, 2.0];
    assert_eq!(
        comb_signed_power(sa, &[0.0; 5]),
        vec![None, Some(-1.0), Some(0.0), Some(1.0), Some(1.0)]
    );
    assert_eq!(
        comb_signed_power(sa, &[1.0; 5]),
        vec![None, Some(-1.0), Some(0.0), Some(1.0), Some(2.0)]
    );
    assert_eq!(
        comb_signed_power(sa, &[2.0; 5]),
        vec![None, Some(-1.0), Some(0.0), Some(1.0), Some(4.0)]
    );
    assert_eq!(
        comb_signed_power(sa, sa),
        vec![None, Some(-1.0), Some(0.0), Some(1.0), Some(4.0)]
    );
    // Clamp both modes; the inverse mask writes `None` inside the band.
    let ka = &[nan, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    assert_eq!(
        comb_clamp(ka, 2.0, 5.0),
        vec![
            None,
            Some(2.0),
            Some(2.0),
            Some(3.0),
            Some(4.0),
            Some(5.0),
            Some(5.0)
        ]
    );
    assert_eq!(
        comb_clamp_inverse(ka, 2.0, 5.0, None),
        vec![None, Some(1.0), None, None, None, None, Some(6.0)]
    );
    assert_eq!(
        comb_clamp_inverse(&[1.0, 3.0, 6.0], 2.0, 5.0, Some(0.0)),
        vec![Some(1.0), Some(0.0), Some(6.0)]
    );
    // Cut counts the breaks strictly below the value.
    assert_eq!(
        comb_cut(
            &[nan, 1.0, 1.0, 1.0, 2.0, 2.0, 3.0, 10.0],
            &[2.0, 5.0, 20.0]
        ),
        vec![
            None,
            Some(0.0),
            Some(0.0),
            Some(0.0),
            Some(0.0),
            Some(0.0),
            Some(1.0),
            Some(2.0)
        ]
    );
    // Tail replaces the strict interior; boundary values stay.
    let ta = &[nan, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    assert_eq!(
        comb_tail(ta, 2.0, 5.0, 1.0),
        vec![
            None,
            Some(1.0),
            Some(2.0),
            Some(1.0),
            Some(1.0),
            Some(5.0),
            Some(6.0)
        ]
    );
    let la = &[nan, 1.0, 2.0, 3.0, 4.0, 5.0];
    assert_eq!(
        comb_left_tail(la, 3.0),
        vec![None, Some(1.0), Some(2.0), Some(3.0), None, None]
    );
    assert_eq!(
        comb_right_tail(la, 3.0),
        vec![None, None, None, Some(3.0), Some(4.0), Some(5.0)]
    );
    // The sign-log composite pins the base-10 convention from the upstream
    // example.
    assert_eq!(
        comb_s_log_1p(&[nan, 9.0, -9.0, 99.0, -99.0]),
        vec![None, Some(1.0), Some(-1.0), Some(2.0), Some(-2.0)]
    );
    // Horizontal variance is the *unnormalized* sum of squared deviations;
    // the all-missing row reads 0.0, not `None`.
    let (va, vb, vc) = (
        &[nan, -1.0, 1.0, 1.0, 2.0],
        &[nan, -1.0, 0.0, 1.0, 2.0],
        &[nan, -1.0, 0.0, 2.0, nan],
    );
    let legs = [va.as_slice(), vb.as_slice(), vc.as_slice()];
    check(
        "var-table",
        &comb_var(&legs),
        &[
            Some(0.0),
            Some(0.0),
            Some(2.0 / 3.0),
            Some(2.0 / 3.0),
            Some(0.0),
        ],
        1e-12,
    );
    check(
        "std-table",
        &comb_std(&legs),
        &[
            Some(0.0),
            Some(0.0),
            Some((2.0_f64 / 3.0).sqrt()),
            Some((2.0_f64 / 3.0).sqrt()),
            Some(0.0),
        ],
        1e-12,
    );
    check(
        "mean-table",
        &comb_mean(&[ia, &[nan, -1.0, 0.0, 0.0, 2.0]]),
        &[None, Some(-1.0), Some(0.0), Some(0.5), Some(2.0)],
        1e-12,
    );
    assert_eq!(
        comb_max(&[&[1.0, nan, 3.0], &[2.0, 5.0, nan]]),
        vec![Some(2.0), Some(5.0), Some(3.0)]
    );
    assert_eq!(
        comb_min(&[&[1.0, nan, 3.0], &[2.0, 5.0, nan]]),
        vec![Some(1.0), Some(5.0), Some(3.0)]
    );
}

#[test]
fn power_mod_and_floor_match_shift_forms() {
    let x = [1.0, 2.0, 3.0, 4.0, 5.0];
    let y = [2.0, 2.0, 1.0, 0.5, 3.0];
    check(
        "power",
        &comb_power(&x, &y),
        &pair_fixture(&x, &y, Expr::pow),
        1e-12,
    );
    check(
        "mod-positive",
        &comb_mod(&x, &y),
        &pair_fixture(&x, &y, |a, b| a % b),
        1e-12,
    );
    check(
        "div-floor",
        &comb_div(&x, &y),
        &pair_fixture(&x, &y, |a, b| (a / b).floor()),
        1e-12,
    );
    // Zero divisors refuse on every path.
    assert_eq!(comb_div(&[1.0], &[0.0]), vec![None]);
    assert_eq!(comb_mod(&[1.0], &[0.0]), vec![None]);
    assert_eq!(comb_divide(&[1.0], &[0.0]), vec![None]);
}

#[test]
fn comparisons_and_logic_follow_kleene() {
    let xs = [1.0, 2.0, 3.0, 4.0];
    let ys = [2.0, 2.0, 2.0, 2.0];
    check(
        "equal",
        &comb_equal(&xs, &ys),
        &pair_fixture(&xs, &ys, Expr::eq),
        0.0,
    );
    check(
        "less",
        &comb_less(&xs, &ys),
        &pair_fixture(&xs, &ys, Expr::lt),
        0.0,
    );
    let mask_t = [1.0, 1.0, 0.0, 0.0];
    let mask_u = [1.0, 0.0, 1.0, 0.0];
    check(
        "and",
        &comb_and(&mask_t, &[&mask_u]),
        &pair_fixture(&mask_t, &mask_u, |left, right| {
            (left.gt(lit(0.0))).logical_and(right.gt(lit(0.0)))
        }),
        0.0,
    );
    check(
        "or",
        &comb_or(&mask_t, &[&mask_u]),
        &pair_fixture(&mask_t, &mask_u, |left, right| {
            (left.gt(lit(0.0))).logical_or(right.gt(lit(0.0)))
        }),
        0.0,
    );
    check(
        "xor",
        &comb_xor(&mask_t, &mask_u),
        &pair_fixture(&mask_t, &mask_u, |left, right| {
            (left.gt(lit(0.0))).xor(right.gt(lit(0.0)))
        }),
        0.0,
    );
    check(
        "not",
        &comb_not(&mask_t),
        &series_fixture(&vals(&mask_t), col("x").gt(lit(0.0)).not()),
        0.0,
    );
    check(
        "if-else",
        &comb_if_else(&[1.0, 0.0], &[10.0, 10.0], &[20.0, 20.0]),
        &series_fixture(
            &vals(&[1.0, 0.0]),
            when(col("x").gt(lit(0.0)))
                .then(lit(10.0))
                .otherwise(lit(20.0)),
        ),
        0.0,
    );
    // A missing condition falls to `else`: staged with a true null on the
    // reference side (the kernel's `NaN` is the null analog), where `when`
    // misses its `then` arm. Note the staging trap this caught: feeding raw
    // `NaN` through `.gt(0)` takes the `then` arm instead, because float
    // `NaN` sorts above every value in comparisons — the kernel's condition
    // parameter corresponds to the boolean condition *result*, not the
    // pre-comparison float.
    {
        use polars::prelude::DataFrame;
        let frame = DataFrame::new(vec![
            polars::prelude::Series::new("c".into(), vec![Some(1.0), Some(0.0), None]).into(),
            polars::prelude::Series::new("t".into(), vec![Some(10.0), Some(10.0), Some(10.0)])
                .into(),
            polars::prelude::Series::new("e".into(), vec![Some(20.0), Some(20.0), Some(20.0)])
                .into(),
        ])
        .expect("if-else frame");
        let selected = frame
            .lazy()
            .select([when(col("c").gt(lit(0.0)))
                .then(col("t"))
                .otherwise(col("e"))])
            .collect()
            .expect("select");
        let first = selected.get_columns().first().expect("column").clone();
        let chunked = first
            .cast(&polars::prelude::DataType::Float64)
            .expect("cast")
            .f64()
            .expect("f64")
            .clone();
        let want: Vec<Option<f64>> = (0..chunked.len()).map(|i| chunked.get(i)).collect();
        check(
            "if-else-null",
            &comb_if_else(
                &[1.0, 0.0, f64::NAN],
                &[10.0, 10.0, 10.0],
                &[20.0, 20.0, 20.0],
            ),
            &want,
            0.0,
        );
    }
    // Kleene tables: false dominates AND, true dominates OR, and `None`
    // poisons comparisons, XOR, and negation.
    let (t1, f0, n) = (Some(1.0), Some(0.0), None);
    assert_eq!(comb_and(&[0.0], &[&[f64::NAN]]), vec![f0]);
    assert_eq!(comb_and(&[1.0], &[&[f64::NAN]]), vec![n]);
    assert_eq!(comb_and(&[f64::NAN], &[&[f64::NAN]]), vec![n]);
    assert_eq!(comb_or(&[1.0], &[&[f64::NAN]]), vec![t1]);
    assert_eq!(comb_or(&[0.0], &[&[f64::NAN]]), vec![n]);
    assert_eq!(comb_xor(&[1.0], &[f64::NAN]), vec![n]);
    assert_eq!(comb_xor(&[1.0], &[1.0]), vec![f0]);
    assert_eq!(comb_not(&[f64::NAN]), vec![n]);
    assert_eq!(comb_negate(&[2.0]), vec![f0]);
    assert_eq!(comb_equal(&[f64::NAN], &[f64::NAN]), vec![n]);
    assert_eq!(comb_less(&[1.0], &[f64::NAN]), vec![n]);
    // Single-leg AND/OR pass the leg through (missing stays missing).
    assert_eq!(comb_and(&[1.0, 0.0, f64::NAN], &[]), vec![t1, f0, n]);
    assert_eq!(comb_or(&[1.0, 0.0, f64::NAN], &[]), vec![t1, f0, n]);
}

#[test]
fn is_predicates_are_total() {
    // Every `is_*` predicate is decidable on every input — including `NaN`
    // itself — so these never read `None`. Note the transcribed quirk:
    // `is_not_finite` is infiniteness, *not* the negation of finiteness.
    let probe = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, 2.0, -3.0];
    let ones = |slots: &[bool]| {
        slots
            .iter()
            .map(|b| Some(f64::from(u8::from(*b))))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        comb_is_nan(&probe),
        ones(&[true, false, false, false, false, false])
    );
    assert_eq!(
        comb_is_null(&probe),
        ones(&[true, false, false, false, false, false])
    );
    assert_eq!(
        comb_is_finite(&probe),
        ones(&[false, false, false, true, true, true])
    );
    assert_eq!(
        comb_is_not_finite(&probe),
        ones(&[false, true, true, false, false, false])
    );
    assert_eq!(
        comb_is_not_nan(&probe),
        ones(&[false, true, true, true, true, true])
    );
    assert_eq!(
        comb_is_not_null(&probe),
        ones(&[false, true, true, true, true, true])
    );
    assert_eq!(
        comb_is_finite(&probe),
        series_fixture(
            &vals(&probe),
            col("x")
                .is_finite()
                .cast(polars::prelude::DataType::Float64)
        ),
    );
    assert_eq!(
        comb_is_nan(&probe),
        series_fixture(
            &vals(&probe),
            col("x").is_nan().cast(polars::prelude::DataType::Float64)
        ),
    );
}

#[test]
fn property_laws_hold() {
    let data = series(30);
    let flipped: Vec<f64> = data.iter().rev().copied().collect();
    // Add/mul commutativity over legs (with `NaN`s mixed in).
    let with_nan: Vec<f64> = data
        .iter()
        .enumerate()
        .map(|(i, v)| if i % 7 == 3 { f64::NAN } else { *v })
        .collect();
    assert_eq!(
        comb_add(&with_nan, &flipped, &[]),
        comb_add(&flipped, &with_nan, &[])
    );
    assert_eq!(
        comb_multiply(&with_nan, &flipped, &[]),
        comb_multiply(&flipped, &with_nan, &[])
    );
    // Clamp is idempotent and contains its output.
    let once = comb_clamp(&data, 101.0, 110.0);
    let as_vals: Vec<f64> = once.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
    assert_eq!(comb_clamp(&as_vals, 101.0, 110.0), once);
    for v in once.iter().flatten() {
        assert!((101.0..=110.0).contains(v), "clamp contains {v}");
    }
    // Sigmoid lives strictly inside (0, 1), hits 0.5 at zero, and mirrors.
    let sig = comb_sigmoid(&[-3.0, -1.0, 0.0, 1.0, 3.0]);
    let plain: Vec<f64> = sig.iter().map(|v| v.expect("finite")).collect();
    assert!(plain.iter().all(|v| *v > 0.0 && *v < 1.0), "sigmoid bounds");
    check("sigmoid-half", &comb_sigmoid(&[0.0]), &[Some(0.5)], 1e-12);
    for (neg, pos) in [(&sig[0], &sig[4]), (&sig[1], &sig[3])] {
        let (neg, pos) = (neg.expect("finite"), pos.expect("finite"));
        assert!((neg + pos - 1.0).abs() < 1e-12, "sigmoid antisymmetry");
    }
    // Softsign shares the bounds with a gentler tail.
    for v in comb_softsign(&[-100.0, -1.0, 0.0, 1.0, 100.0])
        .iter()
        .flatten()
    {
        assert!(v.abs() < 1.0, "softsign bounds {v}");
    }
    // Transcribed `expm1` inverts the accurate `log1p` on small inputs.
    for v in [-0.5, -0.1, 0.0, 0.1, 0.5] {
        let there = comb_expm1(&[v])[0].expect("finite");
        let back = comb_log1p(&[there])[0].expect("finite");
        assert!((back - v).abs() < 1e-12, "expm1/log1p round-trip at {v}");
    }
    // Degrees/radians and square/sqrt round-trip; fraction completes truncation.
    for v in [0.5, 1.0, 2.5, 100.0] {
        let deg = comb_degrees(&[v])[0].expect("finite");
        let back = comb_radians(&[deg])[0].expect("finite");
        assert!((back - v).abs() < 1e-12, "degrees round-trip at {v}");
        let sq = comb_square(&[-v])[0].expect("finite");
        let root = comb_sqrt(&[sq])[0].expect("finite");
        assert!((root - v).abs() < 1e-9, "square/sqrt round-trip at {v}");
        let frac = comb_fraction(&[-v])[0].expect("finite");
        assert!(
            ((-v).trunc() + frac + v).abs() < 1e-12,
            "fraction completes truncation at {v}"
        );
    }
    // Reverse is an involution; cut bins never decrease over sorted input.
    let neg = comb_reverse(&data);
    let back = comb_reverse(
        &neg.iter()
            .map(|v| v.unwrap_or(f64::NAN))
            .collect::<Vec<_>>(),
    );
    assert_eq!(back, vals(&data));
    let bins = comb_cut(&data, &[50.0, 100.0, 150.0, 200.0]);
    let mut last = 0.0;
    for b in bins.iter().flatten() {
        assert!(*b >= last, "cut monotone");
        last = *b;
    }
}

#[test]
fn casts_tails_and_fill_behave() {
    // Truncation toward zero; overflow and missing refuse.
    assert_eq!(
        comb_int(&[f64::NAN, 1.9, -1.9, 2.0, 3e9, -3e9, f64::INFINITY]),
        vec![None, Some(1.0), Some(-1.0), Some(2.0), None, None, None]
    );
    // Truthiness materialization; missing stays missing.
    assert_eq!(
        comb_bool(&[f64::NAN, 0.0, -2.0, 5.0]),
        vec![None, Some(0.0), Some(1.0), Some(1.0)]
    );
    // Inverted clamp bounds refuse fail-closed rather than swapping.
    assert!(comb_clamp(&[1.0, 2.0], 5.0, 2.0)
        .iter()
        .all(Option::is_none));
    assert!(comb_clamp_inverse(&[1.0, 2.0], 5.0, 2.0, Some(0.0))
        .iter()
        .all(Option::is_none));
    // Fill writes the value into every missing slot.
    assert_eq!(
        comb_fill_null(&[f64::NAN, 1.0, 2.0], 0.0),
        vec![Some(0.0), Some(1.0), Some(2.0)]
    );
    // Sigmoid/logit/softsign staging against plain arithmetic on clean data.
    let data = series(10);
    let opt = vals(&data);
    check(
        "sigmoid",
        &comb_sigmoid(&data),
        &series_fixture(&opt, lit(1.0) / (lit(1.0) + (-col("x")).exp())),
        1e-12,
    );
    check(
        "softsign",
        &comb_softsign(&data),
        &series_fixture(&opt, col("x").clone() / (lit(1.0) + col("x").abs())),
        1e-12,
    );
    check(
        "logit",
        &comb_logit(&data),
        &series_fixture(
            &opt,
            (col("x").clone() / (lit(1.0) + col("x"))).log(std::f64::consts::E),
        ),
        1e-12,
    );
    check(
        "cut-ladder",
        &comb_cut(&data, &[101.0, 105.0, 110.0]),
        &series_fixture(
            &opt,
            when(col("x").lt(lit(101.0)))
                .then(lit(0.0))
                .when(col("x").lt(lit(105.0)))
                .then(lit(1.0))
                .when(col("x").lt(lit(110.0)))
                .then(lit(2.0))
                .otherwise(lit(3.0)),
        ),
        0.0,
    );
}

#[test]
fn delegations_match_util_maps() {
    // The four M5 overlaps are thin delegations, never second implementations.
    let leg_a: Vec<f64> = series(15)
        .iter()
        .enumerate()
        .map(|(i, v)| if i % 5 == 2 { f64::NAN } else { *v })
        .collect();
    let leg_b: Vec<f64> = series(15).iter().rev().copied().collect();
    assert_eq!(
        comb_add(&leg_a, &leg_b, &[]),
        crate::util::add_expr(&leg_a, &leg_b, &[])
    );
    assert_eq!(
        comb_multiply(&leg_a, &leg_b, &[]),
        crate::util::mul_expr(&leg_a, &leg_b, &[])
    );
    assert_eq!(
        comb_subtract(&leg_a, &leg_b),
        crate::util::sub_expr(&leg_a, &leg_b)
    );
    assert_eq!(
        comb_divide(&leg_a, &leg_b),
        crate::util::div_expr(&leg_a, &leg_b)
    );
    // Horizontal parity on clean legs stages as plain arithmetic.
    let xs = [1.0, 2.0, 3.0, 4.0];
    let ys = [5.0, 6.0, 7.0, 8.0];
    let zs = [9.0, 1.0, 2.0, 3.0];
    check(
        "hadd",
        &comb_add(&xs, &ys, &[&zs]),
        &triple_fixture(&xs, &ys, &zs, |left, mid, right| left + mid + right),
        1e-12,
    );
    check(
        "hmul",
        &comb_multiply(&xs, &ys, &[&zs]),
        &triple_fixture(&xs, &ys, &zs, |a, b, c| a * b * c),
        1e-12,
    );
}
