#![forbid(unsafe_code)]

//! Pine-compatible evaluation core: explicit run context, series views, and
//! the bar-by-bar runtime driver.
//!
//! One [`context::RunCtx`] per run replaces every process-global singleton:
//! bar cursor, `var`/`varip` stores, validated inputs, and drawings travel
//! together and die with the run, so parallel backtests stay isolated.
//! Indicator kernels arrive in the next milestone; the series, variable, and
//! runtime contracts they build on live here.

pub mod averages;
pub mod calibration;
pub mod candles;
pub mod compat;
pub mod component;
pub mod context;
pub mod drawing;
pub mod ema;
pub mod error;
pub mod filters;
pub mod indicator;
pub mod inputs;
pub mod math;
pub mod oscillators;
pub mod parity;
pub mod pine;
pub mod research;
pub mod runtime;
pub mod timed;
pub mod series;
pub mod sma;
pub mod ta_overlap;
pub mod ta_price;
pub mod ta_momentum;
pub mod ta_stat;
pub mod ta_volume;
pub mod util;
pub mod value;
pub mod vars;
pub mod volatility;
pub mod wq_comb;
pub mod wq_half_life;
pub mod wq_series;

pub use averages::{AlmaIndicator, DemaIndicator, HmaIndicator, RmaIndicator, TemaIndicator, VwmaIndicator, WmaIndicator};
pub use calibration::{Control, ControlViolation, ParityDashboard, ParityRow, canonical_params, null_alignment_diffs, run_control};
pub use candles::{
    candle_color, doji, dragonfly, efficiency_ratio, four_price_doji, gap_down, gap_up,
    gravestone, high_low_range, lower_body, lower_shadow, real_body, real_body_gap_down,
    real_body_gap_up, shadows, upper_body, upper_shadow, TA_EPSILON,
};
pub use compat::{assert_matrix_covers, compat_for, CompatStatus, KernelCompat, COMPATIBILITY};
pub use component::{dynamic_indicator, indicator_double, ComponentRegistry, IndicatorComponent};
pub use context::RunCtx;
pub use drawing::{Drawing, DrawingKind};
pub use ema::EmaIndicator;
pub use error::KernelError;
pub use filters::{EitIndicator, KalmanIndicator, VossIndicator};
pub use indicator::{
    fin_or_none, BarTick, FullTick, NamedSeries, NamedValue, Ohlcv, OwnedOhlcv, builtin_registry,
    execution_order, Indicator, IndicatorId, IndicatorRegistry, IndicatorResult, IndicatorSpec,
    IndicatorState, ParamSet,
};
pub use oscillators::{CciIndicator, CmoIndicator, MacdIndicator, RsiIndicator, StochIndicator};
pub use parity::{assert_expr_parity, expr_fixture};
pub use pine::{
    BarSnapshot, BinaryOp, BuiltinVar, CallArg, EnumMember, FieldDecl, ImportDecl, LibraryId,
    LinkedModule, ModuleGraph, ModuleId, ModuleInput, Param, PineEngine, PineExpr, PineLexer,
    PineParseError, PineParser, PineProgram, PineSource, PineStmt, PineToken, PineType,
    ResolvedModule, RunOutcome, SourceKind, SwitchArm, SwitchStmt, TokenKind, UnaryOp,
    VersionWarn, MAX_LOOP_ITERATIONS, SUPPORTED_PINE_VERSION, lex, load_pine_source,
    parse_pine_source, parse_program, resolve_imports,
};
pub use pine::builtins_ta::TaBridge;
pub use research::{fractal_dimension, frame_of_bars, frame_of_values, price_density, rsrs};
pub use timed::{instrumented_kernel, KernelUnit};
pub use util::{add_expr, argmax_window, argmin_window, change, cross_warmup, crossover, crossunder, div_expr, highest, lowest, max_window, min_window, mom, mul_expr, sub_expr, sum_window};
pub use volatility::{true_range, AtrIndicator, BbIndicator, KcIndicator, NatrIndicator, TrIndicator};
pub use wq_comb::{
    comb_abs, comb_add, comb_and, comb_arc_cos, comb_arc_sin, comb_arc_tan, comb_arc_tan2, comb_bool,
    comb_cbrt, comb_ceiling, comb_clamp, comb_clamp_inverse, comb_cos, comb_cosh, comb_cot, comb_cube,
    comb_cut, comb_degrees, comb_div, comb_divide, comb_equal, comb_exp, comb_expm1, comb_fill_null,
    comb_float, comb_floor, comb_fraction, comb_int, comb_inverse, comb_is_finite, comb_is_nan,
    comb_is_not_finite, comb_is_not_nan, comb_is_not_null, comb_is_null, comb_less, comb_log,
    comb_log1p, comb_log2, comb_log10, comb_logit, comb_max, comb_mean, comb_min, comb_mod,
    comb_multiply, comb_negate, comb_nop, comb_not, comb_or, comb_power, comb_radians, comb_reverse,
    comb_right_tail, comb_round, comb_round_down, comb_s_log_1p, comb_sign, comb_sigmoid,
    comb_signed_power, comb_sin, comb_sinh, comb_softsign, comb_sqrt, comb_square, comb_std,
    comb_subtract, comb_tail, comb_tan, comb_tanh, comb_var, comb_xor, comb_left_tail, comb_if_else,
};
pub use inputs::{InputDef, InputSet};
pub use math::{corr_pearson, div_or_none, ema_factor, ema_step, ewm_span_step, linreg_slope_intercept, midpoint_of, period_len, rolling_max, rolling_min, rolling_min_periods_mask, std_pop, std_sample, variance_pop, variance_sample, warmup_none, wilder_step, window_mean, window_sum};
pub use runtime::{PineRuntime, PineRuntimeConfig};
pub use series::SliceSeries;
pub use sma::SmaIndicator;
pub use ta_overlap::{BbandsIndicator, MidpointIndicator, MidpriceIndicator, TrimaIndicator};
pub use ta_price::{AvgPriceIndicator, MedPriceIndicator, TypPriceIndicator, WclPriceIndicator};
pub use ta_momentum::{ApoIndicator, AroonIndicator, MomIndicator, PpoIndicator, RocIndicator, RocpIndicator, Rocr100Indicator, RocrIndicator, RsvIndicator, StochfIndicator, TrixIndicator, WillrIndicator};
pub use ta_stat::{CorrelIndicator, StddevIndicator, VarIndicator};
pub use ta_volume::{AdIndicator, AdoscIndicator, MfiIndicator, ObvIndicator, VwapIndicator};
pub use value::{ColorHandle, EnumHandle, PineValue, UserInstance, fixnan, nz};
pub use vars::{Var, VarIp, VarStore};

#[cfg(test)]
#[path = "../tests/validate.rs"]
mod validate_tests;
pub use wq_half_life::{hl_weights, ts_mean_hl, ts_std_hl, ts_sum_hl, ts_var_hl};
pub use wq_series::{
    ts_arg_max, ts_arg_min, ts_corr, ts_count, ts_count_eq, ts_count_ge, ts_count_nans,
    ts_covariance, ts_cum_count, ts_cum_max, ts_cum_min, ts_cum_prod, ts_cum_prod_by, ts_cum_sum,
    ts_cum_sum_by, ts_cum_sum_reset, ts_decay_exp_window, ts_decay_linear, ts_delay, ts_delta,
    ts_fill_null, ts_ir, ts_l2_norm, ts_log_diff, ts_max, ts_max_diff, ts_mean, ts_median, ts_min,
    ts_min_diff, ts_min_max_cps, ts_min_max_diff, ts_moment, ts_percentage, ts_product, ts_rank,
    ts_realized_volatility, ts_returns, ts_scale, ts_skewness, ts_std_dev, ts_sum, ts_weighted_decay,
    ts_weighted_mean, ts_weighted_sum, ts_zscore,
};
