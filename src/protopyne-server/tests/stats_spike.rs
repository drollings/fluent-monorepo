use super::*;
use protopyne_kernels::math::{std_pop, variance_pop, window_mean};

fn dec(value: &str) -> Decimal {
    value.parse().expect("valid test decimal")
}

fn decs(values: &[&str]) -> Vec<Decimal> {
    values.iter().map(|v| dec(v)).collect()
}

fn f64s(values: &[&str]) -> Vec<f64> {
    values
        .iter()
        .map(|v| v.parse().expect("valid test float"))
        .collect()
}

#[test]
fn spike_empty_inputs_report_none_both_families() {
    assert_eq!(mean(&[]), None);
    assert_eq!(population_std(&[]), None);
    assert!(
        window_mean(&[]).is_nan(),
        "f64 mean of empty is NaN, not None"
    );
}

#[test]
fn spike_single_element_contracts() {
    assert_eq!(mean(&decs(&["3.0"])), Some(dec("3.0")));
    assert_eq!(population_std(&decs(&["3.0"])), Some(Decimal::ZERO));
    assert_eq!(sharpe(&decs(&["0.01"]), Decimal::ZERO, dec("252")), None);
    assert!((window_mean(&[3.0]) - 3.0).abs() <= f64::EPSILON);
    assert!(variance_pop(&[3.0]) <= f64::EPSILON);
}

#[test]
#[allow(clippy::float_cmp)]
fn spike_constant_series_zero_dispersion() {
    let flat = decs(&["1.0", "1.0", "1.0", "1.0"]);
    assert_eq!(population_std(&flat), Some(Decimal::ZERO));
    assert_eq!(sharpe(&flat, Decimal::ZERO, dec("252")), None);
    assert_eq!(variance_pop(&[1.0, 1.0, 1.0, 1.0]), 0.0);
    assert_eq!(std_pop(&[1.0, 1.0, 1.0, 1.0]), 0.0);
}

#[test]
fn spike_one_sided_books_keep_documented_contract() {
    assert_eq!(profit_factor(&[]), None, "empty book has no ratio");
    assert_eq!(
        profit_factor(&decs(&["1.0", "2.0"])),
        None,
        "all-win book is unbounded: None"
    );
    assert_eq!(
        profit_factor(&decs(&["-1.0", "-2.0"])),
        Some(Decimal::ZERO),
        "all-loss book is Some(0)"
    );
}

#[test]
fn spike_decimal_rounding_edges_are_exact() {
    let third = dec("1").checked_div(dec("3")).expect("divides");
    let mean_of_thirds = mean(&[third, third, third]).expect("mean defined");
    assert_eq!(mean_of_thirds, third, "repeating decimals average exactly");
    let spread = population_std(&decs(&["0.1", "0.2", "0.3"])).expect("defined");
    assert!(spread > Decimal::ZERO);
}

#[test]
#[allow(clippy::float_cmp)]
fn spike_f64_subnormals_and_nan_legs() {
    assert_eq!(variance_pop(&[f64::MIN_POSITIVE, f64::MIN_POSITIVE]), 0.0);
    assert!(
        window_mean(&[1.0, f64::NAN]).is_nan(),
        "NaN propagates in f64 mean"
    );
    assert!(std_pop(&[1.0, f64::NAN]).is_nan());
    assert_eq!(
        mean(&decs(&["1.0"])),
        Some(dec("1.0")),
        "Decimal has no NaN leg"
    );
}

#[test]
fn spike_mean_agrees_on_terminating_values() {
    let words = ["0.5", "1.5", "2.5", "3.5", "4.5", "5.5", "6.5", "7.5"];
    let decimal_mean = mean(&decs(&words)).expect("defined");
    let float_mean = window_mean(&f64s(&words));
    let back: f64 = decimal_mean.to_string().parse().expect("renders");
    assert!(
        (back - float_mean).abs() <= 1e-12,
        "terminating values agree: {back} vs {float_mean}"
    );
}
