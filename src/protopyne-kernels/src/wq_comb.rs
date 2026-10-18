//! WorldQuant-style scalar/vector combinators as pure functions.
//!
//! Elementwise maps over `&[f64]` plus horizontal reductions over legs,
//! mirroring the three readable reference modules (arithmetic, logical,
//! transformational). Like the time-series combinators these are stateless
//! helpers, not chart kernels: no registry, no compat rows, no warmup —
//! output length always equals the (minimum) input length and missing input
//! (`NaN`, the null analog) propagates positionally, never shifts.
//!
//! Three representation rules hold module-wide:
//!
//! * Missing is `NaN` in, `None` out. Every map funnels through
//!   [`fin_or_none`](crate::indicator::fin_or_none), so a `NaN` outcome
//!   (out-of-domain `sqrt`/`log`, negative-base fractional power) reads
//!   `None` exactly where the reference yields null.
//! * Infinities pass through as values. The reference's own tables pin
//!   `0 -> inf` (inverse), `0 -> -inf` (`log`, `log1p`), so only `NaN` is
//!   normalized — except [`comb_divide`], which delegates to the guarded
//!   [`div_expr`](crate::util::div_expr) map and reads `None` on zero
//!   denominators (the one documented departure, inherited from M5).
//! * Booleans are `1.0`/`0.0` with Kleene three-valued logic: `false`
//!   dominates AND, `true` dominates OR, and missing poisons comparisons,
//!   XOR, and negation. All `is_*` predicates are total (never `None`),
//!   matching the reference's null-vs-`NaN` split (`is_null` reads the
//!   `NaN` slots because slices have no separate null).
//!
//! The four M5 operator overlaps ([`comb_add`], [`comb_multiply`],
//! [`comb_subtract`], [`comb_divide`]) are thin delegations to
//! [`util`](crate::util), never second implementations. Deferred by design:
//! `_densify`/`filter_`/`_keep` raise in the reference (no formula to port);
//! `lit_` is call-site construction, not a map; `mean` over a single leg is
//! the identity. Casts that have no slice meaning beyond truncation or
//! truthiness are documented at each function.

use crate::indicator::fin_or_none;

/// Map one slice elementwise; missing stays missing.
fn map_one(values: &[f64], f: impl Fn(f64) -> f64) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| if v.is_nan() { None } else { fin_or_none(f(*v)) })
        .collect()
}

/// Map two aligned slices elementwise; any missing leg poisons its position.
fn map_two(a: &[f64], b: &[f64], f: impl Fn(f64, f64) -> f64) -> Vec<Option<f64>> {
    let len = a.len().min(b.len());
    (0..len)
        .map(|i| {
            if a[i].is_nan() || b[i].is_nan() {
                None
            } else {
                fin_or_none(f(a[i], b[i]))
            }
        })
        .collect()
}

/// Truthiness of one leg: nonzero is true; missing poisons to `None`.
fn truth(value: f64) -> Option<bool> {
    if value.is_nan() {
        None
    } else {
        Some(value != 0.0)
    }
}

fn bool_of(truth: bool) -> f64 {
    f64::from(u8::from(truth))
}

/// Reference sign convention: `-1`/`0`/`1`, with both signed zeros reading
/// `0` (unlike `f64::signum`, which reads `+0.0` as positive).
fn sign_of(value: f64) -> f64 {
    if value > 0.0 {
        1.0
    } else if value < 0.0 {
        -1.0
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Arithmetic: unary
// ---------------------------------------------------------------------------

/// Absolute value.
#[must_use]
pub fn comb_abs(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::abs)
}

/// Square root; negatives read `None` via the `NaN` funnel.
#[must_use]
pub fn comb_sqrt(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::sqrt)
}

/// Cube root (total: negatives stay defined).
#[must_use]
pub fn comb_cbrt(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::cbrt)
}

/// Natural exponential; overflow reads `Some(inf)` like the reference.
#[must_use]
pub fn comb_exp(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::exp)
}

/// `exp(x) - 1`, accurate near zero (transcribed, not `exp` minus one).
#[must_use]
pub fn comb_expm1(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::exp_m1)
}

/// Reciprocal `1 / x`; zero reads `Some(inf)` per the reference table.
#[must_use]
pub fn comb_inverse(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| 1.0 / v)
}

/// Natural logarithm; negatives read `None`, zero reads `Some(-inf)`.
#[must_use]
pub fn comb_log(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::ln)
}

/// Base-10 logarithm, same domain contract as [`comb_log`].
#[must_use]
pub fn comb_log10(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::log10)
}

/// Base-2 logarithm, same domain contract as [`comb_log`].
#[must_use]
pub fn comb_log2(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::log2)
}

/// `ln(1 + x)`; `x = -1` reads `Some(-inf)` per the reference table.
#[must_use]
pub fn comb_log1p(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::ln_1p)
}

/// Sine.
#[must_use]
pub fn comb_sin(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::sin)
}

/// Cosine.
#[must_use]
pub fn comb_cos(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::cos)
}

/// Tangent.
#[must_use]
pub fn comb_tan(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::tan)
}

/// Cotangent as `cos / sin` in reference op order; zeros read `Some(inf)`.
#[must_use]
pub fn comb_cot(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| v.cos() / v.sin())
}

/// Hyperbolic sine.
#[must_use]
pub fn comb_sinh(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::sinh)
}

/// Hyperbolic cosine.
#[must_use]
pub fn comb_cosh(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::cosh)
}

/// Hyperbolic tangent.
#[must_use]
pub fn comb_tanh(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::tanh)
}

/// Arccosine; out-of-range reads `None` via the `NaN` funnel.
#[must_use]
pub fn comb_arc_cos(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::acos)
}

/// Arcsine; out-of-range reads `None` via the `NaN` funnel.
#[must_use]
pub fn comb_arc_sin(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::asin)
}

/// Arctangent.
#[must_use]
pub fn comb_arc_tan(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::atan)
}

/// Two-argument arctangent `atan2(y, x)`.
#[must_use]
pub fn comb_arc_tan2(y: &[f64], x: &[f64]) -> Vec<Option<f64>> {
    map_two(y, x, f64::atan2)
}

/// Radians to degrees.
#[must_use]
pub fn comb_degrees(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::to_degrees)
}

/// Degrees to radians.
#[must_use]
pub fn comb_radians(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::to_radians)
}

/// Square.
#[must_use]
pub fn comb_square(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| v * v)
}

/// Cube.
#[must_use]
pub fn comb_cube(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| v * v * v)
}

/// Floor toward negative infinity.
#[must_use]
pub fn comb_floor(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::floor)
}

/// Ceiling toward positive infinity.
#[must_use]
pub fn comb_ceiling(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, f64::ceil)
}

/// Round half away from zero to `decimals` places (the reference's
/// documented non-banker's rounding: `4.5 -> 5`, `-4.5 -> -5`).
#[must_use]
pub fn comb_round(values: &[f64], decimals: u32) -> Vec<Option<f64>> {
    let scale = 10.0_f64.powi(decimals.cast_signed());
    map_one(values, |v| (v * scale).round() / scale)
}

/// Greatest multiple of `f` at or below the input, in reference op order
/// (`x // f * f` with floor division). Non-positive or non-finite `f`
/// refuses fail-closed as all-`None` rather than dividing by zero.
#[must_use]
pub fn comb_round_down(values: &[f64], f: f64) -> Vec<Option<f64>> {
    if !f.is_finite() || f <= 0.0 {
        return vec![None; values.len()];
    }
    map_one(values, |v| (v / f).floor() * f)
}

/// Signed fraction `sign(x) * (|x| % 1)`, transcribed verbatim including the
/// float remainder (so `-1.0` reads `-0.0`, matching the reference table).
#[must_use]
pub fn comb_fraction(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| sign_of(v) * (v.abs() % 1.0))
}

/// Sign function with the reference convention (`-1`/`0`/`1`); total.
#[must_use]
pub fn comb_sign(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, sign_of)
}

/// Logistic sigmoid `1 / (1 + exp(-x))`, strictly inside `(0, 1)`.
#[must_use]
pub fn comb_sigmoid(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| 1.0 / (1.0 + (-v).exp()))
}

/// Softsign `x / (1 + |x|)`, strictly inside `(-1, 1)`.
#[must_use]
pub fn comb_softsign(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| v / (1.0 + v.abs()))
}

/// Transcribed log-odds `ln(x / (1 + x))` — note the plus sign is the
/// reference's formula verbatim, not the textbook `x / (1 - x)` form.
#[must_use]
pub fn comb_logit(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| (v / (1.0 + v)).ln())
}

/// Sign-preserving log compression `sign(x) * log10(1 + |x|)`; the base-10
/// logarithm is pinned by the reference example (`9 -> 1`, `99 -> 2`).
#[must_use]
pub fn comb_s_log_1p(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| sign_of(v) * (1.0 + v.abs()).log10())
}

/// Negation.
#[must_use]
pub fn comb_reverse(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| -v)
}

/// Identity; missing stays missing.
#[must_use]
pub fn comb_nop(values: &[f64]) -> Vec<Option<f64>> {
    map_one(values, |v| v)
}

// ---------------------------------------------------------------------------
// Arithmetic: binary
// ---------------------------------------------------------------------------

/// Null-ignoring horizontal sum over two or more legs; delegates to the M5
/// [`add_expr`](crate::util::add_expr) map.
#[must_use]
pub fn comb_add(a: &[f64], b: &[f64], rest: &[&[f64]]) -> Vec<Option<f64>> {
    crate::util::add_expr(a, b, rest)
}

/// Null-ignoring horizontal product; delegates to
/// [`mul_expr`](crate::util::mul_expr).
#[must_use]
pub fn comb_multiply(a: &[f64], b: &[f64], rest: &[&[f64]]) -> Vec<Option<f64>> {
    crate::util::mul_expr(a, b, rest)
}

/// Elementwise difference; any missing leg poisons its position via
/// [`sub_expr`](crate::util::sub_expr).
#[must_use]
pub fn comb_subtract(a: &[f64], b: &[f64]) -> Vec<Option<f64>> {
    crate::util::sub_expr(a, b)
}

/// Guarded elementwise division via [`div_expr`](crate::util::div_expr):
/// zero and missing denominators refuse (`None`) instead of emitting `inf`,
/// the one documented departure from the reference's raw `x / y`.
#[must_use]
pub fn comb_divide(a: &[f64], b: &[f64]) -> Vec<Option<f64>> {
    crate::util::div_expr(a, b)
}

/// Floor division in reference op order (`(x / y).floor()`); zero and
/// missing divisors refuse as `None`, matching the reference's null rows.
#[must_use]
pub fn comb_div(a: &[f64], b: &[f64]) -> Vec<Option<f64>> {
    let len = a.len().min(b.len());
    (0..len)
        .map(|i| {
            if a[i].is_nan() || b[i].is_nan() || b[i] == 0.0 {
                None
            } else {
                fin_or_none((a[i] / b[i]).floor())
            }
        })
        .collect()
}

/// Python-style remainder (sign follows the divisor, so `-1 % 2 = 1` per the
/// reference table); zero and missing divisors refuse as `None`.
#[must_use]
pub fn comb_mod(a: &[f64], b: &[f64]) -> Vec<Option<f64>> {
    let len = a.len().min(b.len());
    (0..len)
        .map(|i| {
            if a[i].is_nan() || b[i].is_nan() || b[i] == 0.0 {
                None
            } else {
                fin_or_none(a[i].rem_euclid(b[i]))
            }
        })
        .collect()
}

/// Power `x ** y`; negative bases with fractional exponents read `None` via
/// the `NaN` funnel (`0 ** 0` reads `1`, matching the reference table).
#[must_use]
pub fn comb_power(x: &[f64], y: &[f64]) -> Vec<Option<f64>> {
    map_two(x, y, f64::powf)
}

/// Sign-preserving power, transcribing the reference's three arms verbatim:
/// `y == 1` keeps `x`, `y == 0` keeps the sign, otherwise `|x|^y * sign(x)`.
/// (Exact float comparison is intentional: these are level checks, and the
/// reference branches on the same literal equality.)
#[allow(clippy::float_cmp)]
#[must_use]
pub fn comb_signed_power(x: &[f64], y: &[f64]) -> Vec<Option<f64>> {
    map_two(x, y, |base, exp| {
        if exp == 1.0 {
            base.abs() * sign_of(base)
        } else if exp == 0.0 {
            sign_of(base)
        } else {
            base.abs().powf(exp) * sign_of(base)
        }
    })
}

// ---------------------------------------------------------------------------
// Arithmetic: horizontal reductions
// ---------------------------------------------------------------------------

/// Horizontal maximum ignoring missing legs; `None` only where every leg is
/// missing.
#[must_use]
pub fn comb_max(legs: &[&[f64]]) -> Vec<Option<f64>> {
    horizontal(legs, |present| {
        present.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    })
}

/// Horizontal minimum, same null contract as [`comb_max`].
#[must_use]
pub fn comb_min(legs: &[&[f64]]) -> Vec<Option<f64>> {
    horizontal(legs, |present| {
        present.iter().copied().fold(f64::INFINITY, f64::min)
    })
}

/// Horizontal mean ignoring missing legs; `None` only where every leg is
/// missing.
#[must_use]
pub fn comb_mean(legs: &[&[f64]]) -> Vec<Option<f64>> {
    horizontal(legs, |present| {
        present.iter().sum::<f64>() / present.len() as f64
    })
}

/// Horizontal variance as the *unnormalized* sum of squared deviations over
/// the present legs (transcribed: the reference never divides by a count),
/// with the all-missing row reading `Some(0.0)` per the reference table.
#[must_use]
pub fn comb_var(legs: &[&[f64]]) -> Vec<Option<f64>> {
    let len = legs.iter().map(|leg| leg.len()).min().unwrap_or(0);
    (0..len)
        .map(|i| {
            let present: Vec<f64> = legs
                .iter()
                .map(|leg| leg[i])
                .filter(|v| !v.is_nan())
                .collect();
            if present.is_empty() {
                return Some(0.0);
            }
            let mean = present.iter().sum::<f64>() / present.len() as f64;
            fin_or_none(present.iter().map(|v| (v - mean) * (v - mean)).sum())
        })
        .collect()
}

/// Horizontal standard deviation: square root of [`comb_var`].
#[must_use]
pub fn comb_std(legs: &[&[f64]]) -> Vec<Option<f64>> {
    comb_var(legs)
        .iter()
        .map(|slot| slot.map(f64::sqrt).and_then(fin_or_none))
        .collect()
}

fn horizontal(legs: &[&[f64]], fold: impl Fn(&[f64]) -> f64) -> Vec<Option<f64>> {
    let len = legs.iter().map(|leg| leg.len()).min().unwrap_or(0);
    (0..len)
        .map(|i| {
            let present: Vec<f64> = legs
                .iter()
                .map(|leg| leg[i])
                .filter(|v| !v.is_nan())
                .collect();
            if present.is_empty() {
                None
            } else {
                fin_or_none(fold(&present))
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Logical
// ---------------------------------------------------------------------------

/// Kleene AND over two or more legs: `false` dominates (a single false
/// wins even beside missing), otherwise missing poisons. A lone leg passes
/// through, matching the reference's single-arg early return.
#[must_use]
pub fn comb_and(first: &[f64], rest: &[&[f64]]) -> Vec<Option<f64>> {
    let len = rest.iter().fold(first.len(), |m, leg| m.min(leg.len()));
    (0..len)
        .map(|i| {
            let mut seen_none = false;
            let mut legs = [first]
                .into_iter()
                .chain(rest.iter().copied())
                .map(|leg| leg[i]);
            for value in &mut legs {
                match truth(value) {
                    Some(false) => return Some(bool_of(false)),
                    None => seen_none = true,
                    Some(true) => {}
                }
            }
            if seen_none {
                None
            } else {
                Some(bool_of(true))
            }
        })
        .collect()
}

/// Kleene OR: `true` dominates, otherwise missing poisons; lone leg passes
/// through like [`comb_and`].
#[must_use]
pub fn comb_or(first: &[f64], rest: &[&[f64]]) -> Vec<Option<f64>> {
    let len = rest.iter().fold(first.len(), |m, leg| m.min(leg.len()));
    (0..len)
        .map(|i| {
            let mut seen_none = false;
            let mut legs = [first]
                .into_iter()
                .chain(rest.iter().copied())
                .map(|leg| leg[i]);
            for value in &mut legs {
                match truth(value) {
                    Some(true) => return Some(bool_of(true)),
                    None => seen_none = true,
                    Some(false) => {}
                }
            }
            if seen_none {
                None
            } else {
                Some(bool_of(false))
            }
        })
        .collect()
}

/// Exclusive OR; missing on either leg poisons.
#[must_use]
pub fn comb_xor(a: &[f64], b: &[f64]) -> Vec<Option<f64>> {
    let len = a.len().min(b.len());
    (0..len)
        .map(|i| match (truth(a[i]), truth(b[i])) {
            (Some(x), Some(y)) => Some(bool_of(x ^ y)),
            _ => None,
        })
        .collect()
}

/// Logical negation; missing stays missing.
#[must_use]
pub fn comb_not(values: &[f64]) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| truth(*v).map(|t| bool_of(!t)))
        .collect()
}

/// Alias of [`comb_not`]: the reference defines `negate` as `not_`.
#[must_use]
pub fn comb_negate(values: &[f64]) -> Vec<Option<f64>> {
    comb_not(values)
}

/// Equality; missing on either leg poisons (a `NaN` never equals itself).
#[must_use]
pub fn comb_equal(a: &[f64], b: &[f64]) -> Vec<Option<f64>> {
    // Exact float comparison is the operator's meaning here, not a tolerance.
    #[allow(clippy::float_cmp)]
    let eq = |x: f64, y: f64| x == y;
    let len = a.len().min(b.len());
    (0..len)
        .map(|i| {
            if a[i].is_nan() || b[i].is_nan() {
                None
            } else {
                Some(bool_of(eq(a[i], b[i])))
            }
        })
        .collect()
}

/// Strict less-than; missing on either leg poisons.
#[must_use]
pub fn comb_less(a: &[f64], b: &[f64]) -> Vec<Option<f64>> {
    map_two(a, b, |x, y| f64::from(u8::from(x < y)))
}

/// Conditional select: truthy condition takes its `then` leg, falsy takes
/// `else`, and a missing condition falls to `else` (the reference's
/// `when/otherwise` shape, where null conditions miss the `then` arm).
#[must_use]
pub fn comb_if_else(cond: &[f64], then: &[f64], els: &[f64]) -> Vec<Option<f64>> {
    let len = cond.len().min(then.len()).min(els.len());
    (0..len)
        .map(|i| match truth(cond[i]) {
            Some(true) => fin_or_none(then[i]),
            _ => fin_or_none(els[i]),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Predicates: total `is_*` maps (never `None`)
// ---------------------------------------------------------------------------

/// Total `NaN` probe. (Slices have no separate null, so `NaN` is the null
/// analog and `is_null` below reads the same slots.)
fn probe_total(values: &[f64], f: impl Fn(f64) -> bool) -> Vec<Option<f64>> {
    values.iter().map(|v| Some(bool_of(f(*v)))).collect()
}

/// `true` exactly on missing (`NaN`) slots.
#[must_use]
pub fn comb_is_nan(values: &[f64]) -> Vec<Option<f64>> {
    probe_total(values, f64::is_nan)
}

/// `true` exactly on missing slots (the slice null analog).
#[must_use]
pub fn comb_is_null(values: &[f64]) -> Vec<Option<f64>> {
    probe_total(values, f64::is_nan)
}

/// `true` on finite values (`NaN` and both infinities read `false`).
#[must_use]
pub fn comb_is_finite(values: &[f64]) -> Vec<Option<f64>> {
    probe_total(values, f64::is_finite)
}

/// Transcribed quirk: the reference implements this as infiniteness, *not*
/// the negation of finiteness, so `NaN` reads `false` here.
#[must_use]
pub fn comb_is_not_finite(values: &[f64]) -> Vec<Option<f64>> {
    probe_total(values, f64::is_infinite)
}

/// Negation of [`comb_is_nan`].
#[must_use]
pub fn comb_is_not_nan(values: &[f64]) -> Vec<Option<f64>> {
    probe_total(values, |v| !v.is_nan())
}

/// Negation of [`comb_is_null`].
#[must_use]
pub fn comb_is_not_null(values: &[f64]) -> Vec<Option<f64>> {
    probe_total(values, |v| !v.is_nan())
}

// ---------------------------------------------------------------------------
// Transformational
// ---------------------------------------------------------------------------

/// Clamp into `[lower, upper]`; inverted bounds refuse fail-closed as
/// all-`None` rather than swapping the caller's intent.
#[must_use]
pub fn comb_clamp(values: &[f64], lower: f64, upper: f64) -> Vec<Option<f64>> {
    if lower.is_nan() || upper.is_nan() || lower > upper {
        return vec![None; values.len()];
    }
    map_one(values, |v| v.clamp(lower, upper))
}

/// Inverse clamp: values inside `[lower, upper]` are replaced with `mask`
/// (`None` mask writes missing), values outside pass through. Same
/// fail-closed contract as [`comb_clamp`].
#[must_use]
pub fn comb_clamp_inverse(
    values: &[f64],
    lower: f64,
    upper: f64,
    mask: Option<f64>,
) -> Vec<Option<f64>> {
    if lower.is_nan() || upper.is_nan() || lower > upper {
        return vec![None; values.len()];
    }
    values
        .iter()
        .map(|v| {
            if v.is_nan() {
                None
            } else if *v >= lower && *v <= upper {
                mask
            } else {
                Some(*v)
            }
        })
        .collect()
}

/// Bin into `breaks.len() + 1` categories counting the breaks strictly below
/// the value (right-closed bins, matching the reference's default
/// left-open cut: `2 -> 0`, `3 -> 1`, `10 -> 2` over `[2, 5, 20]`).
#[must_use]
pub fn comb_cut(values: &[f64], breaks: &[f64]) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| {
            if v.is_nan() || breaks.iter().any(|b| b.is_nan()) {
                None
            } else {
                Some(breaks.iter().filter(|b| **b < *v).count() as f64)
            }
        })
        .collect()
}

/// Keep values at or below `maximum`; greater values read `None`.
#[must_use]
pub fn comb_left_tail(values: &[f64], maximum: f64) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| {
            if v.is_nan() || maximum.is_nan() {
                None
            } else if *v <= maximum {
                Some(*v)
            } else {
                None
            }
        })
        .collect()
}

/// Keep values at or above `minimum`; lesser values read `None`.
#[must_use]
pub fn comb_right_tail(values: &[f64], minimum: f64) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| {
            if v.is_nan() || minimum.is_nan() {
                None
            } else if *v >= minimum {
                Some(*v)
            } else {
                None
            }
        })
        .collect()
}

/// Replace the strict interior `(lower, upper)` with `newval`; boundary
/// values pass through, matching the reference table (`2` and `5` stay).
#[must_use]
pub fn comb_tail(values: &[f64], lower: f64, upper: f64, newval: f64) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| {
            if v.is_nan() {
                None
            } else if *v > lower && *v < upper {
                fin_or_none(newval)
            } else {
                Some(*v)
            }
        })
        .collect()
}

/// Clear non-finite values (`NaN` and both infinities) to `None`.
#[must_use]
pub fn comb_purify(values: &[f64]) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| if v.is_finite() { Some(*v) } else { None })
        .collect()
}

/// Fill missing (`NaN`) slots with `value`; a `NaN` fill stays missing
/// through the funnel.
#[must_use]
pub fn comb_fill_null(values: &[f64], value: f64) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| {
            if v.is_nan() {
                fin_or_none(value)
            } else {
                Some(*v)
            }
        })
        .collect()
}

/// Truncate toward zero with fail-closed refusal: missing, non-finite, and
/// out-of-`i32` values read `None` (the reference's `Int32` cast has no
/// slice-representable overflow, so overflow refuses instead of wrapping).
#[must_use]
pub fn comb_int(values: &[f64]) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| {
            if v.is_nan() || !v.is_finite() {
                return None;
            }
            let truncated = v.trunc();
            if truncated < f64::from(i32::MIN) || truncated > f64::from(i32::MAX) {
                return None;
            }
            #[allow(clippy::cast_possible_truncation)]
            let _ = truncated as i32;
            Some(truncated)
        })
        .collect()
}

/// Materialize truthiness (`0 -> 0`, nonzero `-> 1`); missing stays missing.
#[must_use]
pub fn comb_bool(values: &[f64]) -> Vec<Option<f64>> {
    values.iter().map(|v| truth(*v).map(bool_of)).collect()
}

/// Widen through 32-bit float, matching the reference's `Float32` cast
/// rounding (large magnitudes saturate to `inf` through the funnel).
#[must_use]
pub fn comb_float(values: &[f64]) -> Vec<Option<f64>> {
    values
        .iter()
        .map(|v| {
            if v.is_nan() {
                None
            } else {
                #[allow(clippy::cast_possible_truncation)]
                let narrowed = *v as f32;
                fin_or_none(f64::from(narrowed))
            }
        })
        .collect()
}

#[cfg(all(test, feature = "research"))]
#[path = "../tests/wq_comb.rs"]
mod tests;
