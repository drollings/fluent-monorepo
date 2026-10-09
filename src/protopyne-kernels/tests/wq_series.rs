use super::*;
use crate::parity::assert_expr_parity;
use crate::research::frame_of_values;
use polars::prelude::{
    Expr, FillNullStrategy, IntoLazy, NamedFrom as _, QuantileMethod, RollingFnParams,
    RollingOptionsFixedWindow, RollingVarParams, col, lit, when,
};

/// Synthetic scale series (trend-plus-jitter shape, no bars needed: these
/// combinators run on bare slices).
fn series(n: usize) -> Vec<f64> {
    (0..n).map(|i| 100.0 + i as f64 * 0.7 + ((i * 13) % 5) as f64 * 0.9).collect()
}

fn some_of(values: &[f64]) -> Vec<Option<f64>> {
    values.iter().map(|v| Some(*v)).collect()
}

/// Series funnel mirroring `expr_fixture`: one `frame_of_values` column in,
/// first selected column out with the same null/NaN mapping.
fn series_fixture(values: &[Option<f64>], expr: Expr) -> Vec<Option<f64>> {
    use crate::indicator::fin_or_none;
    let frame = frame_of_values("x", values).expect("frame");
    let selected = frame.lazy().select([expr]).collect().expect("select");
    assert_eq!(selected.height(), values.len(), "height preserved");
    let first = selected.get_columns().first().expect("column").clone();
    let cast = first.cast(&polars::prelude::DataType::Float64).expect("cast");
    let chunked = cast.f64().expect("f64");
    (0..chunked.len()).map(|i| chunked.get(i).and_then(fin_or_none)).collect()
}

fn check(name: &str, got: &[Option<f64>], want: &[Option<f64>], eps: f64) {
    assert_eq!(got.len(), want.len(), "{name} length");
    assert_expr_parity(got, want, eps, name).expect("parity");
}

/// Parity over the warmed tail plus an explicit leading-`None` count: some
/// references carry staging artifacts in the warmup zone (a nested rolling
/// pass costs an extra window; `otherwise` fires on null conditions), while
/// kernels hold the module-wide min-periods convention. Both sides are
/// asserted where each is defined.
fn check_warmed(name: &str, got: &[Option<f64>], want: &[Option<f64>], eps: f64, warmed_from: usize) {
    assert!(
        got[..warmed_from].iter().all(Option::is_none),
        "{name} keeps leading Nones"
    );
    check(name, &got[warmed_from..], &want[warmed_from..], eps);
}

fn strict(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow { window_size: n, min_periods: n, weights: None, center: false, fn_params: None }
}

fn var_opts(n: usize, ddof: u8) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow {
        window_size: n,
        min_periods: n,
        weights: None,
        center: false,
        fn_params: Some(RollingFnParams::Var(RollingVarParams { ddof })),
    }
}

fn weighted(n: usize, weights: Vec<f64>) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow { window_size: n, min_periods: n, weights: Some(weights), center: false, fn_params: None }
}

#[test]
fn delay_delta_and_returns_match_shift_forms() {
    let data = series(20);
    let opt = some_of(&data);
    check("delay-2", &ts_delay(&data, 2, None), &series_fixture(&opt, col("x").shift(lit(2))), 1e-9);
    check("lead-1", &ts_delay(&data, -1, None), &series_fixture(&opt, col("x").shift(lit(-1))), 1e-9);
    check(
        "delay-fill",
        &ts_delay(&data, 2, Some(0.0)),
        &series_fixture(&opt, col("x").shift_and_fill(lit(2), lit(0.0))),
        1e-9,
    );
    check("delta-1", &ts_delta(&data, 1), &series_fixture(&opt, col("x") - col("x").shift(lit(1))), 1e-9);
    check(
        "returns-1",
        &ts_returns(&data, 1),
        &series_fixture(&opt, (col("x") - col("x").shift(lit(1))) / col("x").shift(lit(1))),
        1e-9,
    );
    check(
        "log-diff-1",
        &ts_log_diff(&data, 1),
        &series_fixture(&opt, col("x").log(std::f64::consts::E) - col("x").log(std::f64::consts::E).shift(lit(1))),
        1e-9,
    );
    // Zero-base returns refuse where the reference yields infinities.
    assert_eq!(ts_returns(&[0.0, 1.0, 2.0], 1), vec![None, None, Some(1.0)]);
    // Degenerate lags stay silent.
    assert!(ts_delta(&data, 0).iter().all(Option::is_none));
    assert!(ts_delay(&[1.0], 5, None).iter().all(Option::is_none));
}

#[test]
fn sums_means_and_products_match_rolling_forms() {
    let data = series(20);
    let opt = some_of(&data);
    let d = 5;
    check("sum", &ts_sum(&data, d), &series_fixture(&opt, col("x").rolling_sum(strict(d))), 1e-9);
    check("mean", &ts_mean(&data, d), &series_fixture(&opt, col("x").rolling_mean(strict(d))), 1e-9);
    check("cum-sum", &ts_cum_sum(&data), &series_fixture(&opt, col("x").cum_sum(false)), 1e-9);
    check("cum-max", &ts_cum_max(&data), &series_fixture(&opt, col("x").cum_max(false)), 1e-9);
    check("cum-min", &ts_cum_min(&data), &series_fixture(&opt, col("x").cum_min(false)), 1e-9);
    check("cum-prod", &ts_cum_prod(&[1.0, 2.0, 3.0, 4.0]), &series_fixture(&some_of(&[1.0, 2.0, 3.0, 4.0]), col("x").cum_prod(false)), 1e-9);
    check("cum-count", &ts_cum_count(&data), &series_fixture(&opt, col("x").cum_count(false)), 0.0);
    // Products skip NaN like the reference's nanprod; all-NaN stays None.
    assert_eq!(ts_product(&[2.0, 3.0, 4.0], 2), vec![None, Some(6.0), Some(12.0)]);
    assert_eq!(ts_product(&[2.0, f64::NAN, 4.0], 2), vec![None, Some(2.0), Some(4.0)]);
    assert_eq!(ts_product(&[f64::NAN, f64::NAN], 2), vec![None, None]);
    // Cumulative legs skip NaN instead of poisoning.
    assert_eq!(ts_cum_sum(&[1.0, f64::NAN, 3.0]), vec![Some(1.0), Some(1.0), Some(4.0)]);
    assert_eq!(ts_cum_count(&[1.0, f64::NAN, 3.0]), vec![Some(1.0), Some(1.0), Some(2.0)]);
}

#[test]
fn extremes_match_rolling_forms() {
    let data = series(20);
    let opt = some_of(&data);
    let d = 5;
    check("max", &ts_max(&data, d), &series_fixture(&opt, col("x").rolling_max(strict(d))), 1e-9);
    check("min", &ts_min(&data, d), &series_fixture(&opt, col("x").rolling_min(strict(d))), 1e-9);
    check(
        "max-diff",
        &ts_max_diff(&data, d),
        &series_fixture(&opt, col("x") - col("x").rolling_max(strict(d))),
        1e-9,
    );
    check(
        "min-diff",
        &ts_min_diff(&data, d),
        &series_fixture(&opt, col("x") - col("x").rolling_min(strict(d))),
        1e-9,
    );
    let f = 2.0;
    check(
        "min-max-cps",
        &ts_min_max_cps(&data, d, f),
        &series_fixture(
            &opt,
            col("x").rolling_min(strict(d)) + col("x").rolling_max(strict(d)) - lit(f) * col("x"),
        ),
        1e-9,
    );
    check(
        "min-max-diff",
        &ts_min_max_diff(&data, d, 0.5),
        &series_fixture(
            &opt,
            col("x") - lit(0.5) * (col("x").rolling_min(strict(d)) + col("x").rolling_max(strict(d))),
        ),
        1e-9,
    );
}

#[test]
fn moments_match_rolling_forms() {
    let data = series(30);
    let opt = some_of(&data);
    let d = 5;
    check("std-pop", &ts_std_dev(&data, d, 0), &series_fixture(&opt, col("x").rolling_std(var_opts(d, 0))), 1e-9);
    check("std-sample", &ts_std_dev(&data, d, 1), &series_fixture(&opt, col("x").rolling_std(var_opts(d, 1))), 1e-9);
    check(
        "skew",
        &ts_skewness(&data, d, false),
        &series_fixture(
            &opt,
            col("x").rolling_skew(RollingOptionsFixedWindow {
                window_size: d,
                min_periods: d,
                weights: None,
                center: false,
                fn_params: Some(RollingFnParams::Skew { bias: false }),
            }),
        ),
        1e-9,
    );
    check(
        "kurtosis",
        &ts_kurtosis(&data, d, false),
        &series_fixture(
            &opt,
            col("x").rolling_kurtosis(RollingOptionsFixedWindow {
                window_size: d,
                min_periods: d,
                weights: None,
                center: false,
                fn_params: Some(RollingFnParams::Kurtosis { fisher: true, bias: false }),
            }),
        ),
        1e-9,
    );
    // Central moment in shift-expanded single-pass form: the nested
    // `rolling_mean(rolling(...))` staging would cost a second window of
    // warmup, so the mean is written as an explicit lag sum instead.
    let lagged: Vec<Expr> = (0..d).map(|k| col("x").shift(lit(k as i64))).collect();
    let mean = lagged.iter().cloned().reduce(|a, b| a + b).expect("lags") / lit(d as f64);
    let moment = lagged
        .iter()
        .map(|lag| (lag.clone() - mean.clone()) * (lag.clone() - mean.clone()))
        .reduce(|a, b| a + b)
        .expect("lags")
        / lit(d as f64);
    check("moment-2", &ts_moment(&data, d, 2), &series_fixture(&opt, moment), 1e-9);
    assert!(ts_moment(&data, d, 0).iter().skip(d - 1).all(|v| *v == Some(1.0)));
    check(
        "l2-norm",
        &ts_l2_norm(&data, d),
        &series_fixture(&opt, (col("x") * col("x")).rolling_sum(strict(d)).sqrt()),
        1e-9,
    );
    check(
        "ir",
        &ts_ir(&data, d),
        &series_fixture(&opt, col("x").rolling_mean(strict(d)) / col("x").rolling_std(var_opts(d, 0))),
        1e-9,
    );
    // Degenerate denominators refuse on both sides of the parity.
    let flat = vec![5.0; 10];
    let flat_opt = some_of(&flat);
    check("std-flat", &ts_std_dev(&flat, d, 0), &series_fixture(&flat_opt, col("x").rolling_std(var_opts(d, 0))), 0.0);
    assert!(ts_std_dev(&flat, d, 5).iter().all(Option::is_none));
}

// Rank-tie identity is exact equality: a tie means identical prints.
#[allow(clippy::float_cmp)]
#[test]
fn median_quantile_and_rank_match_rolling_forms() {
    let data = series(20);
    let opt = some_of(&data);
    let d = 5;
    check("median", &ts_median(&data, d), &series_fixture(&opt, col("x").rolling_median(strict(d))), 1e-9);
    check(
        "percentage",
        &ts_percentage(&data, d, 0.5),
        &series_fixture(&opt, col("x").rolling_quantile(QuantileMethod::Nearest, 0.5, strict(d))),
        1e-9,
    );
    // No native rolling-rank expression exists: the reference stages it
    // through a window map whose closure is pinned by the hand table below.
    let rank_ref = col("x").rolling_map_float(d, move |window: &mut polars::prelude::Float64Chunked| {
        let values: Vec<Option<f64>> = window.into_iter().collect();
        let current = values.last().copied().flatten()?;
        if !current.is_finite() {
            return None;
        }
        let mut present: Vec<f64> = values.into_iter().flatten().filter(|v| v.is_finite()).collect();
        if present.len() < d {
            return None;
        }
        present.sort_by(f64::total_cmp);
        let less = present.iter().filter(|v| **v < current).count() as f64;
        let equal = present.iter().filter(|v| **v == current).count() as f64;
        Some((less + (equal - 1.0) / 2.0 + 1.0) / present.len() as f64)
    });
    check("rank", &ts_rank(&data, d), &series_fixture(&opt, rank_ref), 1e-12);
    // Hand table: average ranks over ties, normalized by the window count.
    assert_eq!(ts_rank(&[3.0, 1.0, 2.0], 3), vec![None, None, Some(2.0 / 3.0)]);
    assert_eq!(ts_rank(&[2.0, 2.0, 2.0], 3), vec![None, None, Some(2.0 / 3.0)]);
    assert_eq!(ts_rank(&[1.0, 2.0, 3.0], 3), vec![None, None, Some(1.0)]);
}

#[test]
fn arg_extremes_follow_the_pinned_tables() {
    // Reference docstring series: NaN poisons only windows that end on it.
    let data = vec![0.0, 1.0, 0.0, f64::NAN, 1.0, 0.0];
    assert_eq!(
        ts_arg_max(&data, 3, true),
        vec![None, None, Some(1.0), None, None, None]
    );
    assert_eq!(
        ts_arg_min(&data, 3, true),
        vec![None, None, Some(0.0), None, None, None]
    );
    // Unreversed search measures from the window start.
    assert_eq!(ts_arg_max(&[0.0, 1.0, 0.0], 3, false), vec![None, None, Some(1.0)]);
    assert_eq!(ts_arg_min(&[0.0, 1.0, 0.0], 3, false), vec![None, None, Some(0.0)]);
    // Most-recent tie-break matches the shared argmax helper on clean data.
    let clean = series(20);
    let to_float = |positions: Vec<Option<usize>>| positions.into_iter().map(|p| p.map(|v| v as f64)).collect::<Vec<_>>();
    assert_eq!(ts_arg_max(&clean, 5, true), to_float(crate::util::argmax_window(&clean, 5)));
    assert_eq!(ts_arg_min(&clean, 5, true), to_float(crate::util::argmin_window(&clean, 5)));
}

#[test]
fn corr_and_covariance_match_moment_forms() {
    let xs = series(30);
    let ys: Vec<f64> = xs.iter().map(|v| v * 0.5 + 3.0).collect();
    let d = 5;
    let frame = polars::prelude::DataFrame::new(vec![
        polars::prelude::Series::new("x".into(), some_of(&xs)).into(),
        polars::prelude::Series::new("y".into(), some_of(&ys)).into(),
    ])
    .expect("pair frame");
    let slope_parts = |a: &str, b: &str| {
        let mx = col(a).rolling_mean(strict(d));
        let my = col(b).rolling_mean(strict(d));
        (mx, my)
    };
    let (mx, my) = slope_parts("x", "y");
    // ddof cancels out of the correlation, so all three legs stay in the
    // same unscaled moment form (scaling only the covariance skewed the
    // ratio by n/(n-1) — caught here, not shipped).
    let sxy = (col("x") * col("y")).rolling_mean(strict(d)) - mx.clone() * my.clone();
    let sxx = (col("x") * col("x")).rolling_mean(strict(d)) - mx.clone() * mx.clone();
    let syy = (col("y") * col("y")).rolling_mean(strict(d)) - my.clone() * my.clone();
    let cov = sxy.clone() * lit(d as f64 / (d - 1) as f64);
    let corr = sxy / (sxx * syy).sqrt();
    let selected = frame.lazy().select([cov.alias("cov"), corr.alias("corr")]).collect().expect("select");
    let col_of = |name: &str| {
        let s = selected.column(name).expect("col").cast(&polars::prelude::DataType::Float64).expect("cast");
        let c = s.f64().expect("f64");
        (0..c.len()).map(|i| c.get(i).and_then(crate::indicator::fin_or_none)).collect::<Vec<_>>()
    };
    // ddof cancels out of the correlation; covariance keeps the n/(n-1) scale.
    check("corr", &ts_corr(&xs, &ys, d, 1), &col_of("corr"), 1e-9);
    check("cov", &ts_covariance(&xs, &ys, d, 1), &col_of("cov"), 1e-9);
    // Flat legs carry no correlation on either side.
    let flat = vec![4.0; 10];
    assert!(ts_corr(&flat, &flat, d, 1).iter().all(Option::is_none));
}

#[test]
fn normalized_and_decayed_forms_match() {
    let data = series(20);
    let opt = some_of(&data);
    let d = 5;
    check(
        "zscore",
        &ts_zscore(&data, d),
        &series_fixture(&opt, (col("x") - col("x").rolling_mean(strict(d))) / col("x").rolling_std(var_opts(d, 0))),
        1e-9,
    );
    // The reference `otherwise` fires on null conditions, so it reads 0.0
    // while warming; the kernel holds the module-wide leading-`None`
    // convention instead (short inputs must stay silent per milestone).
    check_warmed(
        "scale",
        &ts_scale(&data, d),
        &series_fixture(
            &opt,
            when(col("x").rolling_min(strict(d)).neq(col("x").rolling_max(strict(d))))
                .then((col("x") - col("x").rolling_min(strict(d))) / (col("x").rolling_max(strict(d)) - col("x").rolling_min(strict(d))))
                .otherwise(lit(0.0)),
        ),
        1e-9,
        d - 1,
    );
    let linear: Vec<f64> = (1..=d).map(|k| k as f64).collect();
    check("decay-linear", &ts_decay_linear(&data, d), &series_fixture(&opt, col("x").rolling_mean(weighted(d, linear))), 1e-9);
    let factor = 0.5;
    // Oldest gets the smallest power: [f^(d-1), ..., f, 1].
    let exp: Vec<f64> = std::iter::successors(Some(1.0), |w| Some(w * factor)).take(d).collect::<Vec<_>>().into_iter().rev().collect();
    check("decay-exp", &ts_decay_exp_window(&data, d, factor), &series_fixture(&opt, col("x").rolling_mean(weighted(d, exp))), 1e-9);
    check(
        "weighted-decay",
        &ts_weighted_decay(&data, 0.5),
        &series_fixture(&opt, col("x").rolling_sum(weighted(2, vec![0.5, 0.5]))),
        1e-9,
    );
    // Full-length weight leg: the reference windows `w` exactly like `x`.
    let ramp: Vec<f64> = (0..data.len()).map(|i| 1.0 + (i % d) as f64).collect();
    check(
        "weighted-mean",
        &ts_weighted_mean(&data, &ramp, d),
        &series_fixture_pair(&data, &ramp, |x, w| {
            (x.clone() * w.clone()).rolling_sum(strict(d)) / w.rolling_sum(strict(d))
        }),
        1e-9,
    );
    // The decay docstring pins oldest-gets-least: [6,5,4,5,30] ends at 13.2.
    let decayed = ts_decay_linear(&[0.0, 6.0, 5.0, 4.0, 5.0, 30.0], 5);
    assert_eq!(decayed.last().copied().flatten(), Some(13.2));
    // Flat scaling reads zero, not None (correctness axis, like min-max).
    assert_eq!(ts_scale(&[7.0, 7.0, 7.0, 7.0], 4).last().copied().flatten(), Some(0.0));
}

/// Two-column funnel for pair expressions over `x` and `w`.
fn series_fixture_pair(x: &[f64], w: &[f64], expr: impl Fn(Expr, Expr) -> Expr) -> Vec<Option<f64>> {
    use crate::indicator::fin_or_none;
    use polars::prelude::{DataFrame, Series};
    let frame = DataFrame::new(vec![
        Series::new("x".into(), some_of(x)).into(),
        Series::new("w".into(), some_of(w)).into(),
    ])
    .expect("pair frame");
    let selected = frame.lazy().select([expr(col("x"), col("w"))]).collect().expect("select");
    let first = selected.get_columns().first().expect("column").clone();
    let cast = first.cast(&polars::prelude::DataType::Float64).expect("cast");
    let chunked = cast.f64().expect("f64");
    (0..chunked.len()).map(|i| chunked.get(i).and_then(fin_or_none)).collect()
}

#[test]
fn counts_and_fill_match() {
    let data = vec![1.0, 0.0, 3.0, 0.0, 5.0, 0.0];
    let opt = some_of(&data);
    let d = 3;
    check(
        "count",
        &ts_count(&data, d),
        &series_fixture(&opt, col("x").neq(lit(0.0)).cast(polars::prelude::DataType::UInt32).rolling_sum(strict(d))),
        0.0,
    );
    check(
        "count-eq",
        &ts_count_eq(&data, 4, 2),
        &series_fixture(
            &opt,
            (col("x").neq(lit(0.0)).cast(polars::prelude::DataType::UInt32).rolling_sum(strict(2)).eq(lit(2)))
                .and(col("x").neq(lit(0.0)).cast(polars::prelude::DataType::UInt32).rolling_sum(strict(4)).eq(lit(2))),
        ),
        0.0,
    );
    check(
        "count-ge",
        &ts_count_ge(&data, 4, 2),
        &series_fixture(
            &opt,
            (col("x").neq(lit(0.0)).cast(polars::prelude::DataType::UInt32).rolling_sum(strict(2)).eq(lit(2)))
                .and(col("x").neq(lit(0.0)).cast(polars::prelude::DataType::UInt32).rolling_sum(strict(4)).gt_eq(lit(2))),
        ),
        0.0,
    );
    check(
        "count-nans",
        &ts_count_nans(&[1.0, f64::NAN, 3.0, f64::NAN, 5.0], 3),
        &series_fixture(
            &[Some(1.0), Some(f64::NAN), Some(3.0), Some(f64::NAN), Some(5.0)],
            col("x").is_nan().cast(polars::prelude::DataType::UInt32).rolling_sum(strict(3)),
        ),
        0.0,
    );
    // Forward fill honors the limit; leading gaps stay missing.
    assert_eq!(
        ts_fill_null(&[f64::NAN, 1.0, f64::NAN, f64::NAN, 2.0], None),
        vec![None, Some(1.0), Some(1.0), Some(1.0), Some(2.0)]
    );
    assert_eq!(
        ts_fill_null(&[f64::NAN, 1.0, f64::NAN, f64::NAN, 2.0], Some(1)),
        vec![None, Some(1.0), Some(1.0), None, Some(2.0)]
    );
    check(
        "fill-null",
        &ts_fill_null(&[f64::NAN, 1.0, f64::NAN, 2.0], None),
        &series_fixture(
            &[None, Some(1.0), None, Some(2.0)],
            col("x").fill_null_with_strategy(FillNullStrategy::Forward(None)),
        ),
        0.0,
    );
}

#[test]
fn reset_and_seeded_cums_follow_the_pinned_tables() {
    // Verbatim reference example: zero/NaN emit zero, sign flips restart.
    assert_eq!(
        ts_cum_sum_reset(&[1.0, 0.0, 1.0, 2.0, f64::NAN, 3.0, -2.0, -3.0]),
        vec![
            Some(1.0),
            Some(0.0),
            Some(1.0),
            Some(3.0),
            Some(0.0),
            Some(3.0),
            Some(-2.0),
            Some(-5.0)
        ]
    );
    // Seeded accumulation: explicit seeds win, gaps compound from the last.
    let gains = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let seeds = vec![f64::NAN, f64::NAN, 6.0, f64::NAN, f64::NAN, 12.0];
    assert_eq!(
        ts_cum_prod_by(&gains, &seeds),
        vec![None, None, Some(6.0), Some(24.0), Some(120.0), Some(12.0)]
    );
    assert_eq!(
        ts_cum_sum_by(&gains, &seeds),
        vec![None, None, Some(6.0), Some(10.0), Some(15.0), Some(12.0)]
    );
}

#[test]
fn realized_volatility_tracks_log_spread() {
    let data = series(20);
    let d = 5;
    let log_diff = col("x").log(std::f64::consts::E) - col("x").log(std::f64::consts::E).shift(lit(1));
    check(
        "realized-vol",
        &ts_realized_volatility(&data, d),
        &series_fixture(&some_of(&data), log_diff.rolling_std(var_opts(d, 0))),
        1e-9,
    );
}

#[test]
fn short_inputs_emit_leading_nones_never_shifted_values() {
    // Every windowed family preserves length and keeps the first d-1 slots
    // empty instead of shrinking the window silently.
    let data = series(4);
    let d = 6;
    for (name, got) in [
        ("sum", ts_sum(&data, d)),
        ("mean", ts_mean(&data, d)),
        ("median", ts_median(&data, d)),
        ("std", ts_std_dev(&data, d, 0)),
        ("max", ts_max(&data, d)),
        ("min", ts_min(&data, d)),
        ("zscore", ts_zscore(&data, d)),
        ("rank", ts_rank(&data, d)),
        ("corr", ts_corr(&data, &data, d, 1)),
        ("cov", ts_covariance(&data, &data, d, 1)),
        ("decay", ts_decay_linear(&data, d)),
        ("scale", ts_scale(&data, d)),
        ("count", ts_count(&data, d)),
        ("moment", ts_moment(&data, d, 2)),
        ("skew", ts_skewness(&data, d, false)),
        ("l2", ts_l2_norm(&data, d)),
        ("percentage", ts_percentage(&data, d, 0.5)),
        ("product", ts_product(&data, d)),
    ] {
        assert_eq!(got.len(), data.len(), "{name} preserves length");
        assert!(got.iter().all(Option::is_none), "{name} stays None when short");
    }
    // Degenerate widths refuse everywhere.
    assert!(ts_sum(&data, 0).iter().all(Option::is_none));
    assert!(ts_corr(&data, &data, 0, 1).iter().all(Option::is_none));
}
