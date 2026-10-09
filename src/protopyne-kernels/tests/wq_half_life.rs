use super::*;
use crate::parity::assert_expr_parity;
use crate::research::frame_of_values;
use polars::prelude::{IntoLazy, RollingOptionsFixedWindow, col};

fn series(n: usize) -> Vec<f64> {
    (0..n).map(|i| 100.0 + i as f64 * 0.7 + ((i * 13) % 5) as f64 * 0.9).collect()
}

fn some_of(values: &[f64]) -> Vec<Option<f64>> {
    values.iter().map(|v| Some(*v)).collect()
}

fn series_fixture(values: &[Option<f64>], expr: polars::prelude::Expr) -> Vec<Option<f64>> {
    use crate::indicator::fin_or_none;
    let frame = frame_of_values("x", values).expect("frame");
    let selected = frame.lazy().select([expr]).collect().expect("select");
    let first = selected.get_columns().first().expect("column").clone();
    let cast = first.cast(&polars::prelude::DataType::Float64).expect("cast");
    let chunked = cast.f64().expect("f64");
    (0..chunked.len()).map(|i| chunked.get(i).and_then(fin_or_none)).collect()
}

fn weighted(n: usize, weights: Vec<f64>) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow { window_size: n, min_periods: n, weights: Some(weights), center: false, fn_params: None }
}

#[test]
fn exponent_weights_decay_toward_the_past() {
    // Newest bar carries weight one; older bars decay geometrically.
    let weights = hl_weights(4, 2);
    let ratio = 0.5f64.powf(0.5);
    for (got, want) in weights.iter().zip([ratio.powi(3), ratio.powi(2), ratio, 1.0]) {
        assert!((got - want).abs() < 1e-12, "weight {got} vs {want}");
    }
}

#[test]
fn half_life_forms_match_weighted_rolling() {
    let data = series(20);
    let opt = some_of(&data);
    let (d, half_life) = (5, 2);
    let weights = hl_weights(d, half_life);
    let check = |name: &str, got: &[Option<f64>], want: Vec<Option<f64>>| {
        assert_eq!(got.len(), want.len(), "{name} length");
        assert_expr_parity(got, &want, 1e-9, name).expect("parity");
    };
    check("mean-hl", &ts_mean_hl(&data, d, half_life), series_fixture(&opt, col("x").rolling_mean(weighted(d, weights.clone()))));
    check("sum-hl", &ts_sum_hl(&data, d, half_life), series_fixture(&opt, col("x").rolling_sum(weighted(d, weights.clone()))));
    check("std-hl", &ts_std_hl(&data, d, half_life), series_fixture(&opt, col("x").rolling_std(weighted(d, weights.clone()))));
    check("var-hl", &ts_var_hl(&data, d, half_life), series_fixture(&opt, col("x").rolling_var(weighted(d, weights.clone()))));
    // A NaN anywhere in the window voids it, like the reference's null-fill.
    let gappy = vec![1.0, 2.0, f64::NAN, 4.0, 5.0, 6.0];
    assert_eq!(ts_mean_hl(&gappy, 3, 1)[2], None);
    assert_eq!(ts_mean_hl(&gappy, 3, 1)[4], None);
    // Short inputs stay silent.
    assert!(ts_var_hl(&[1.0, 2.0], d, half_life).iter().all(Option::is_none));
}
