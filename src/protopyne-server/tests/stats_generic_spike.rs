//! Feasibility spike: can one generic reproduce both statistics
//! families bit-for-bit?
//!
//! The candidate generic below is test-local on purpose — it is the
//! unification hypothesis under test, NOT a production primitive. It shares
//! exactly the fold skeleton both families use (sum-then-divide mean,
//! mean-of-squared-deviations variance) abstracted over a scalar trait, with
//! each scalar keeping its own arithmetic (Decimal stays checked Decimal,
//! f64 stays IEEE). Agreement tests prove where the skeleton shares;
//! divergence tests pin where the contracts cannot unify and drive the
//! GO/NO-GO decision recorded in the roadmap.

use super::*;
use crate::common_fixtures::money;
use crate::marks::MarkBasis;
use protopyne_kernels::math::{std_pop, variance_pop, window_mean};

/// The unification hypothesis: one fold skeleton over an abstract scalar.
/// Decimal keeps checked ops (overflow reads `None`); f64 keeps IEEE ops
/// (overflow reads `inf`, empty reads through the early `None` below).
trait SpikeScalar: Copy {
    fn zero() -> Self;
    fn checked_add(left: Self, right: Self) -> Option<Self>;
    fn sub(left: Self, right: Self) -> Option<Self>;
    fn mul(left: Self, right: Self) -> Option<Self>;
    fn checked_div(left: Self, right: Self) -> Option<Self>;
    fn root(value: Self) -> Option<Self>;
    fn from_len(len: usize) -> Option<Self>;
}

impl SpikeScalar for Decimal {
    fn zero() -> Self {
        Decimal::ZERO
    }
    fn checked_add(left: Self, right: Self) -> Option<Self> {
        left.checked_add(right)
    }
    fn sub(left: Self, right: Self) -> Option<Self> {
        left.checked_sub(right)
    }
    fn mul(left: Self, right: Self) -> Option<Self> {
        left.checked_mul(right)
    }
    fn checked_div(left: Self, right: Self) -> Option<Self> {
        left.checked_div(right)
    }
    fn root(value: Self) -> Option<Self> {
        value.sqrt()
    }
    fn from_len(len: usize) -> Option<Self> {
        u64::try_from(len).ok().map(Decimal::from)
    }
}

impl SpikeScalar for f64 {
    fn zero() -> Self {
        0.0
    }
    fn checked_add(left: Self, right: Self) -> Option<Self> {
        Some(left + right)
    }
    fn sub(left: Self, right: Self) -> Option<Self> {
        Some(left - right)
    }
    fn mul(left: Self, right: Self) -> Option<Self> {
        Some(left * right)
    }
    fn checked_div(left: Self, right: Self) -> Option<Self> {
        Some(left / right)
    }
    fn root(value: Self) -> Option<Self> {
        Some(value.sqrt())
    }
    fn from_len(len: usize) -> Option<Self> {
        Some(len as f64)
    }
}

fn spike_mean<T: SpikeScalar>(values: &[T]) -> Option<T> {
    if values.is_empty() {
        return None;
    }
    let total = values
        .iter()
        .try_fold(T::zero(), |sum, &value| T::checked_add(sum, value))?;
    T::checked_div(total, T::from_len(values.len())?)
}

fn spike_variance_pop<T: SpikeScalar>(values: &[T]) -> Option<T> {
    let center = spike_mean(values)?;
    let acc = values.iter().try_fold(T::zero(), |sum, &value| {
        let deviation = T::sub(value, center)?;
        T::checked_add(sum, T::mul(deviation, deviation)?)
    })?;
    T::checked_div(acc, T::from_len(values.len())?)
}

fn spike_population_std<T: SpikeScalar>(values: &[T]) -> Option<T> {
    spike_variance_pop(values).and_then(T::root)
}

fn dec(value: &str) -> Decimal {
    value.parse().expect("valid test decimal")
}

fn decs(values: &[&str]) -> Vec<Decimal> {
    values.iter().map(|value| dec(value)).collect()
}

fn f64s(values: &[&str]) -> Vec<f64> {
    values
        .iter()
        .map(|v| v.parse().expect("valid test float"))
        .collect()
}

// ── Agreement: the skeleton shares on non-degenerate finite inputs ──

#[test]
fn spike_decimal_mean_matches_exactly_including_thirds() {
    for fixture in [
        vec!["1.0", "2.0", "3.0", "4.0"],
        vec!["0.1", "0.2", "0.3"],
        vec!["-5.5", "2.25", "100.75"],
        vec!["3.0"],
        vec!["1.0", "1.0", "1.0", "1.0"],
    ] {
        let values = decs(&fixture);
        assert_eq!(
            spike_mean(&values),
            mean(&values),
            "mean drift on {fixture:?}"
        );
    }
    let third = dec("1").checked_div(dec("3")).expect("divides");
    assert_eq!(
        spike_mean(&[third, third, third]),
        mean(&[third, third, third]),
        "banker's thirds stay exact under the generic"
    );
    assert_eq!(
        spike_mean(&[third, third, third]),
        Some(third),
        "repeating decimals average exactly"
    );
}

#[test]
fn spike_f64_mean_matches_bitwise_on_nonempty_finite() {
    for fixture in [
        vec!["1.0", "2.0", "3.0", "4.0"],
        vec!["0.5", "1.5", "2.5", "3.5", "4.5", "5.5", "6.5", "7.5"],
        vec!["-5.5", "2.25", "100.75"],
        vec!["3.0"],
    ] {
        let values = f64s(&fixture);
        let spike = spike_mean(&values).expect("nonempty finite has a mean");
        assert_eq!(
            spike.to_bits(),
            window_mean(&values).to_bits(),
            "mean drift on {fixture:?}"
        );
    }
    let subnormals = [f64::MIN_POSITIVE, f64::MIN_POSITIVE];
    assert_eq!(
        spike_mean(&subnormals).expect("defined").to_bits(),
        window_mean(&subnormals).to_bits(),
        "subnormals agree bitwise"
    );
}

#[test]
fn spike_variance_matches_both_families() {
    let decimals = decs(&["1.0", "2.0", "3.0", "4.0"]);
    assert_eq!(
        spike_variance_pop(&decimals),
        Some(dec("1.25")),
        "decimal variance is exact on terminating fixtures"
    );
    assert_eq!(
        spike_population_std(&decimals),
        population_std(&decimals),
        "decimal std shares the sqrt of the same variance"
    );
    for fixture in [
        vec![2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0],
        vec![1.0, 2.0, 3.0, 4.0],
        vec![3.0],
        vec![1.0, 1.0, 1.0, 1.0],
        vec![f64::MIN_POSITIVE, f64::MIN_POSITIVE],
    ] {
        let spike = spike_variance_pop(&fixture).expect("nonempty has variance");
        assert_eq!(
            spike.to_bits(),
            variance_pop(&fixture).to_bits(),
            "variance drift on {fixture:?}"
        );
        let std_spike = spike_population_std(&fixture).expect("defined");
        assert_eq!(
            std_spike.to_bits(),
            std_pop(&fixture).to_bits(),
            "std drift on {fixture:?}"
        );
    }
}

// ── Divergence: contracts that cannot unify without drift or API breakage ──

#[test]
fn spike_empty_contract_diverges_none_vs_nan() {
    // The Decimal world reports absence as `None`; the f64 world reports it
    // as NaN data. One return type cannot spell both: `Option<T>` forces
    // every f64 call site onto a `None → NaN` mapping shim, while a raw `T`
    // cannot spell absence for Decimal at all (no NaN exists there).
    assert_eq!(spike_mean::<Decimal>(&[]), None);
    assert_eq!(mean(&[]), None, "decimal agrees: absence is None");
    assert_eq!(spike_mean::<f64>(&[]), None, "generic can only say None");
    assert!(
        window_mean(&[]).is_nan(),
        "f64 says NaN — unifying the signature drifts this to None, \
         or keeps NaN behind a per-type branch (two impls with a shim)"
    );
    assert_eq!(spike_variance_pop::<f64>(&[]), None);
    assert!(
        variance_pop(&[]).is_nan(),
        "same divergence one level up: variance of empty is NaN in f64"
    );
}

#[test]
fn spike_nan_leg_has_no_decimal_vocabulary() {
    // NaN propagation is load-bearing f64 behavior (pinned in stats_spike):
    // the generic preserves it per-type, but Decimal cannot even construct
    // the fixture — there is no Decimal NaN to propagate.
    let legged = [1.0, f64::NAN];
    let spike = spike_mean(&legged).expect("generic never refuses IEEE");
    assert!(
        spike.is_nan(),
        "f64 NaN propagates through the generic, matching window_mean"
    );
    assert!(
        window_mean(&legged).is_nan(),
        "current f64 behavior: NaN contaminates the window"
    );
    assert!(
        Decimal::from_f64_retain(f64::NAN).is_none(),
        "Decimal has no NaN leg"
    );
}

#[test]
fn spike_overflow_contracts_differ_per_type_by_design() {
    // Checked Decimal refuses overflow (None); IEEE f64 produces inf. The
    // generic preserves each — which proves the sharing stops at the fold
    // skeleton: any cross-type statement about overflow is false for one side.
    let huge = [Decimal::MAX, Decimal::MAX];
    assert_eq!(spike_mean(&huge), None);
    assert_eq!(mean(&huge), None, "decimal overflow is absence");
    let inf_pair = [1e308f64, 1e308];
    let spike = spike_mean(&inf_pair).expect("IEEE never refuses");
    assert_eq!(spike.to_bits(), window_mean(&inf_pair).to_bits());
    assert_eq!(
        spike.to_bits(),
        f64::INFINITY.to_bits(),
        "f64 overflow is inf data, not absence"
    );
}

#[test]
fn spike_ratio_and_curve_helpers_have_no_second_family() {
    // Only mean + population variance overlap. Every helper below exists in
    // exactly one scalar world, so a shared `stats` module would carry each
    // as a single-consumer wrapper — below the two-consumer promotion bar,
    // and each wrapper would still need its own scalar's edge contract.
    let moves = decs(&["10", "-5", "15"]);
    assert!(sharpe(
        &decs(&["0.01", "0.02", "-0.01", "0.03"]),
        Decimal::ZERO,
        dec("252")
    )
    .is_some());
    assert!(sortino(
        &decs(&["0.01", "0.02", "-0.01", "0.03"]),
        Decimal::ZERO,
        dec("252")
    )
    .is_some());
    assert!(calmar(
        &decs(&["100", "110", "105", "120"]),
        Decimal::ZERO,
        dec("252")
    )
    .is_some());
    assert!(win_rate(&moves).is_some());
    assert!(profit_factor(&moves).is_some());
    assert!(!step_returns(&decs(&["0", "0", "100", "110"])).is_empty());
    assert!(drawdown_fraction(&decs(&["100", "110", "105", "120"])).is_some());
    assert_eq!(
        protopyne_kernels::math::window_sum(&[1.0, 2.0, 3.0]).to_bits(),
        6.0f64.to_bits(),
        "window_sum is f64-only: no Decimal counterpart exists"
    );
}

// ── Report goldens under the spike: no production edit, so byte-identical ──

fn manifest() -> crate::report::RunManifest {
    crate::report::RunManifest {
        data_version: "m7-spike-v1".to_owned(),
        code_version: env!("CARGO_PKG_VERSION").to_owned(),
        config_json: "{}".to_owned(),
        bar_count: 6,
        first_ts: String::new(),
        last_ts: String::new(),
        seed: 0,
        data_digest: None,
        data_first_ts: None,
        data_last_ts: None,
        data_bar_count: None,
        data_provenance: None,
    }
}

fn report_for(curve: &[&str]) -> crate::report::BacktestReport {
    crate::report::build_report(
        manifest(),
        Vec::new(),
        Vec::new(),
        0,
        0,
        money("0"),
        money("0"),
        money("0"),
        curve.iter().map(|value| money(value)).collect(),
        None,
        None,
        MarkBasis::FillPrice,
    )
}

#[test]
fn spike_report_and_tearsheet_rerun_byte_identical() {
    // No production edit lands in M7.1, so the report surfaces the metrics
    // family exactly as the checked-in goldens see it: rerun equality plus
    // a populated five-metric report on a moving curve and `None` metrics
    // on a flat curve.
    let curve = ["0", "0", "100", "110", "105", "120"];
    let first = report_for(&curve);
    let second = report_for(&curve);
    assert_eq!(first.to_canonical_json(), second.to_canonical_json());
    assert!(first.sharpe.is_some(), "sharpe wired through build_report");
    assert!(first.sortino.is_some());
    assert!(first.calmar.is_some());
    assert!(first.win_rate.is_some());
    assert!(first.profit_factor.is_some());
    assert_eq!(
        crate::report::render_tearsheet(&first),
        crate::report::render_tearsheet(&second),
        "tearsheet renders from the same structs as the JSON report"
    );
    let flat = report_for(&["100", "100", "100"]);
    assert_eq!(flat.sharpe, None, "flat curve carries absence, never zero");
}
