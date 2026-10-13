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
pub mod bars;
pub mod buffers;
#[cfg(feature = "research")]
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
pub mod market_context;
pub mod mtf;
pub use market_context::{
    MarketContext, MarketContextKey, MarketRegime, detect_regime, detect_regime_stable, fetch_all,
    health_mas, update, CCI_SCALE, DIRECTION_BAND, HEALTH_AMPLITUDE, HEALTH_DEADBAND, HEALTH_K,
    HEALTH_LOWER_BASE, HEALTH_MID_WEIGHT, HEALTH_MIXED_BASE, HEALTH_MIXED_SPAN, HEALTH_UPPER_BASE,
    REGIME_HYSTERESIS, RSI_CENTER, RSI_SCALE, TREND_NORM, TREND_SCORE_FLOOR, VOLATILITY_SCALE,
    VOLATILITY_SCORE_FLOOR, VOLUME_SCALE, VOLUME_SCORE_FLOOR,
};
#[cfg(feature = "pine")]
pub mod native_libs;
pub mod oscillators;
#[cfg(feature = "research")]
pub mod parity;
#[cfg(feature = "pine")]
pub mod pine;
#[cfg(feature = "research")]
pub mod research;
#[cfg(feature = "pine")]
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
pub use bars::{
    BUFFER_PADDING, DRIFT_TOLERANCE_PCT, MAX_EXACT_VOLUME, NEUTRAL_BUY_RATIO, VOLUME_EPSILON,
    BarParts, BarRing, BarStateFlags, BarsConfig, BarsHistory, ClosedBar, ConfirmOutcome,
    ConfirmedBar, DeltaConfidence, DeltaEstimate, DeltaMethod, DriftDecision, DriftReport, FetchCheck, FetchNativeOut, FetchSignal, LtfOutcome,
    LtfRow, NativeFetchTick, RawTick, ReconcileOutcome, ResampleSource, RingCursor, TickClock, TickIngest,
    TradingSession, choose_source, drift_pct,
    estimate_buy_volume, estimate_delta_volume, et_offset_ms, fast_tanh, is_confirmed_bar,
    is_dst_et, is_valid_volume, project_bars, tick_bar_state, tick_resample, validate_ohlcv,
    volume_deltas,
};
#[cfg(feature = "research")]
pub use calibration::{Control, ControlViolation, ParityDashboard, ParityRow, canonical_params, null_alignment_diffs, run_control};
pub use candles::{
    candle_color, doji, dragonfly, efficiency_ratio, four_price_doji, gap_down, gap_up,
    gravestone, high_low_range, lower_body, lower_shadow, real_body, real_body_gap_down,
    real_body_gap_up, shadows, upper_body, upper_shadow, TA_EPSILON,
};
pub use compat::{assert_matrix_covers, compat_for, is_native_row, native_row_id, CompatStatus, KernelCompat, COMPATIBILITY, NATIVE_MATRIX_ROWS};
#[cfg(feature = "pine")]
pub use compat::assert_native_matrix_covers;
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
#[cfg(feature = "research")]
pub use parity::{assert_expr_parity, expr_fixture};
#[cfg(feature = "pine")]
pub use pine::{
    AssignItem, AssignOp, BarSnapshot, BinaryOp, BuiltinVar, CallArg, ClosureEntry, EguiParityRow,
    EnumMember, FieldDecl, FieldPersistence, ImportDecl, LibraryId, LinkedModule, LtfDataset,
    ModuleGraph, ModuleId, ModuleInput, MultiDecl, Param, ParityVerdict, PineEngine, PineExpr, PineLexer,
    PineParseError, PineParser, PineProgram, PineSource, PineStmt, PineToken, PineType,
    ResolvedModule, RunOutcome, SourceKind, SwitchArm, SwitchStmt, TokenKind, UnaryOp, VersionWarn,
    EGUI_PARITY, INDICATOR_PATH, MAX_LOOP_ITERATIONS, REAL_LIBRARIES, SUPPORTED_PINE_VERSION,
    TASK_INDICATORS, closure_data_root, display_program, indicator_library_id, lex,
    load_closure_sources, load_listed_closure,
    load_pine_source, parity_for, parse_pine_source, parse_program, resolve_imports,
};
#[cfg(feature = "pine")]
pub use pine::builtins_ta::TaBridge;
#[cfg(feature = "research")]
pub use research::{fractal_dimension, frame_of_bars, frame_of_values, price_density, rsrs};
pub use timed::{instrumented_kernel, KernelUnit};
pub use util::{add_expr, argmax_window, argmin_window, change, cross_warmup, crossover, crossunder, div_expr, highest, lowest, max_window, min_window, mom, mul_expr, sub_expr, sum_window};
pub use volatility::{true_range, AtrIndicator, BbIndicator, KcIndicator, NatrIndicator, SupertrendIndicator, TrIndicator};
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
pub use mtf::{
    BucketSpec, Lookahead, MtfStore, SeriesCache, SeriesKey, aligned_geq, bucket_has_gap,
    bucket_ids, build_graph, floor_to_grid, floor_to_grid_checked, fully_formed, grid_roundtrip,
    interleave_sorted, interleave_sorted_by, merge_sorted, merge_sorted_by, order_for,
    pairs_from_sorted, validate_split, FRAME_KERNEL,
};
#[cfg(feature = "pine")]
pub use native_libs::{
    BarContextServe, FetchCounters, LeasedRing, LeaseKey, LibApexProvider, LibBarsProvider,
    LibBuffersProvider, MethodCtx, NativeAudit, NativeLibraryEntry, NativeLibraryId,
    NativeOptions, NativeOverlay, NativeProvider, NativeResolution, NativeServed, TickLegs,
    NATIVE_LIBRARIES, apply_overrides, fetch_tick_from_frame, find_native, pine_sha_of,
    version_is_ignored,
};
#[cfg(feature = "pine")]
pub use runtime::{PineRuntime, PineRuntimeConfig};
pub use series::SliceSeries;
pub use sma::SmaIndicator;
pub use ta_overlap::{BbandsIndicator, DonchianIndicator, IchimokuIndicator, MidpointIndicator, MidpriceIndicator, PsarIndicator, TrimaIndicator};
pub use ta_price::{AvgPriceIndicator, MedPriceIndicator, TypPriceIndicator, WclPriceIndicator};
pub use ta_momentum::{AdxIndicator, ApoIndicator, AroonIndicator, MomIndicator, PpoIndicator, RocIndicator, RocpIndicator, Rocr100Indicator, RocrIndicator, RsvIndicator, StochfIndicator, TrixIndicator, WillrIndicator};
pub use ta_stat::{CorrelIndicator, StddevIndicator, VarIndicator};
pub use ta_volume::{AdIndicator, AdoscIndicator, CmfIndicator, MfiIndicator, ObvIndicator, VwapIndicator};
pub use value::{ColorHandle, EnumHandle, FieldTable, NativeCall, PineValue, UserInstance, fixnan, map_key_of, nz};
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
