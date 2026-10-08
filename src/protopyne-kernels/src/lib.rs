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
pub mod compat;
pub mod context;
pub mod drawing;
pub mod ema;
pub mod error;
pub mod indicator;
pub mod inputs;
pub mod oscillators;
pub mod research;
pub mod runtime;
pub mod timed;
pub mod series;
pub mod sma;
pub mod util;
pub mod value;
pub mod vars;
pub mod volatility;

pub use averages::{AlmaIndicator, HmaIndicator, RmaIndicator, VwmaIndicator, WmaIndicator};
pub use compat::{assert_matrix_covers, compat_for, CompatStatus, KernelCompat, COMPATIBILITY};
pub use context::RunCtx;
pub use drawing::{Drawing, DrawingKind};
pub use ema::EmaIndicator;
pub use error::KernelError;
pub use indicator::{
    fin_or_none, BarTick, FullTick, NamedSeries, NamedValue, Ohlcv, OwnedOhlcv, builtin_registry,
    execution_order, Indicator, IndicatorId, IndicatorRegistry, IndicatorResult, IndicatorSpec,
    IndicatorState, ParamSet,
};
pub use oscillators::{MacdIndicator, RsiIndicator, StochIndicator};
pub use research::{frame_of_bars, frame_of_values};
pub use timed::{instrumented_kernel, KernelUnit};
pub use util::{change, cross_warmup, crossover, crossunder, highest, lowest, mom};
pub use volatility::{true_range, AtrIndicator, BbIndicator, KcIndicator, TrIndicator};
pub use inputs::{InputDef, InputSet};
pub use runtime::{PineRuntime, PineRuntimeConfig};
pub use series::SliceSeries;
pub use sma::SmaIndicator;
pub use value::PineValue;
pub use vars::{Var, VarIp, VarStore};
