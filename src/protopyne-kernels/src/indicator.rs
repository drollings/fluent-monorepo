//! Indicator contract, registry, and execution ordering.
//!
//! An [`Indicator`] declares its [`IndicatorSpec`] (identity, warmup floor,
//! dependencies), evaluates whole windows ([`batch`](Indicator::batch)) and
//! single bars ([`update`](Indicator::update)), and is looked up through an
//! [`IndicatorRegistry`] that owns only *kernels*, never run state: a caller may
//! build its own ([`builtin_registry`]) or share the process-wide immutable
//! catalog ([`shared_registry`]). Ordering composes `fluent-dag`'s [`DependencyGraph`](fluent_dag::dep_graph::DependencyGraph);
//! no second topological sort lives here.
//!
//! Series legs are plain `f64` slices where IEEE `NaN` means Pine `na`: it
//! propagates through arithmetic and maps back to `None` at the output
//! boundary via [`fin_or_none`]. Kernels read only the legs they need from
//! [`Ohlcv`]; [`BarTick`] is the matching per-bar input for [`update`](Indicator::update).

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use bon::Builder;
use common_core::registry::KeyedRegistry;
use fluent_dag::dep_graph::GraphError;
use serde::{Deserialize, Serialize};

use crate::ema::EmaIndicator;
use crate::error::KernelError;
use crate::sma::SmaIndicator;
use crate::value::PineValue;

/// Registry and graph key for one indicator (e.g. `"sma"`).
pub type IndicatorId = String;

/// Whole-window input: five aligned legs over the same bars. `close` is
/// authoritative for length; every leg a kernel reads must match it (see
/// [`require`](Ohlcv::require)).
#[derive(Debug, Clone, Copy)]
pub struct Ohlcv<'a> {
    /// Open prices.
    pub open: &'a [f64],
    /// High prices.
    pub high: &'a [f64],
    /// Low prices.
    pub low: &'a [f64],
    /// Close prices (authoritative length).
    pub close: &'a [f64],
    /// Volumes.
    pub volume: &'a [f64],
}

impl<'a> Ohlcv<'a> {
    /// Bundle for single-close kernels; other legs are empty and must not be
    /// read (enforced by [`require`](Ohlcv::require)).
    #[must_use]
    pub fn close_only(close: &'a [f64]) -> Self {
        Self {
            open: &[],
            high: &[],
            low: &[],
            close,
            volume: &[],
        }
    }

    /// Number of bars (length of the close leg).
    #[must_use]
    pub fn len(&self) -> usize {
        self.close.len()
    }

    /// Whether no bars are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.close.is_empty()
    }

    /// Borrow a leg after checking it is present and aligned with `close`.
    pub fn require(&self, name: &str, leg: &'a [f64]) -> Result<&'a [f64], KernelError> {
        if leg.len() != self.close.len() {
            return Err(KernelError::InvalidParam(format!(
                "leg {name:?} has length {}, expected {} (close)",
                leg.len(),
                self.close.len()
            )));
        }
        Ok(leg)
    }
}

/// Owned whole-window input for supervised execution (type-erased handoffs
/// require `'static` data; hot paths borrow [`views`](OwnedOhlcv::views)).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OwnedOhlcv {
    /// Open prices.
    pub open: Vec<f64>,
    /// High prices.
    pub high: Vec<f64>,
    /// Low prices.
    pub low: Vec<f64>,
    /// Close prices.
    pub close: Vec<f64>,
    /// Volumes.
    pub volume: Vec<f64>,
}

impl OwnedOhlcv {
    /// Convert closed bars to float legs. Prices and volumes convert exactly
    /// at market magnitudes; an unrepresentable value becomes `NaN` (na)
    /// rather than failing the whole window.
    #[must_use]
    pub fn from_bars(bars: &[protopyne_types::Bar]) -> Self {
        use rust_decimal::prelude::ToPrimitive as _;
        let conv = |d: rust_decimal::Decimal| nan_hole(d.to_f64());
        Self {
            open: bars.iter().map(|b| conv(b.o)).collect(),
            high: bars.iter().map(|b| conv(b.h)).collect(),
            low: bars.iter().map(|b| conv(b.l)).collect(),
            close: bars.iter().map(|b| conv(b.c)).collect(),
            volume: bars.iter().map(|b| b.volume as f64).collect(),
        }
    }

    /// Borrow all five legs.
    #[must_use]
    pub fn views(&self) -> Ohlcv<'_> {
        Ohlcv {
            open: &self.open,
            high: &self.high,
            low: &self.low,
            close: &self.close,
            volume: &self.volume,
        }
    }

    /// Number of bars.
    #[must_use]
    pub fn len(&self) -> usize {
        self.close.len()
    }

    /// Whether no bars are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.close.is_empty()
    }
}

/// Per-bar input for [`update`](Indicator::update).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarTick {
    /// Open price.
    pub open: f64,
    /// High price.
    pub high: f64,
    /// Low price.
    pub low: f64,
    /// Close price.
    pub close: f64,
    /// Volume.
    pub volume: f64,
}

impl BarTick {
    /// Tick carrying only a close (other legs zero); for single-close tests.
    #[must_use]
    pub fn close_only(close: f64) -> Self {
        Self {
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
        }
    }
}

/// Map a computed value to its output slot: `NaN` (Pine `na`) becomes `None`.
#[must_use]
pub fn fin_or_none(value: f64) -> Option<f64> {
    if value.is_nan() {
        None
    } else {
        Some(value)
    }
}

/// Map an output slot to its hole value: `None` becomes `NaN` (Pine `na`).
/// The one spelling for the hole idiom every kernel's output edge repeats,
/// inverse of [`fin_or_none`].
#[inline]
#[must_use]
pub fn nan_hole(value: Option<f64>) -> f64 {
    value.unwrap_or(f64::NAN)
}

/// One declared kernel parameter.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IndicatorParam {
    /// Parameter name as the kernel reads it.
    pub name: &'static str,
    /// Value kind.
    pub kind: ParamKind,
    /// Declared default.
    pub default: ParamValue,
    /// Optional lower bound.
    pub min: Option<f64>,
    /// Optional upper bound.
    pub max: Option<f64>,
    /// Editor step.
    pub step: f64,
    /// Human description.
    pub description: &'static str,
}

/// Parameter kinds the schema editor understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamKind {
    /// Whole number.
    Int,
    /// Decimal number.
    Float,
    /// Boolean.
    Bool,
}

/// One declared default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ParamValue {
    /// Integer default.
    Int(i64),
    /// Float default.
    Float(f64),
    /// Boolean default.
    Bool(bool),
}

/// Role of one output series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputRole {
    /// Main output, carried by `IndicatorResult::values`.
    Primary,
    /// Named output in `IndicatorResult::secondary`.
    Secondary,
}

/// Display scale policy for one output. This is a display value, never a
/// trading threshold.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ScalePolicy {
    /// Declared display bounds.
    Bounded {
        /// Lower display bound.
        min: f64,
        /// Upper display bound.
        max: f64,
    },
    /// Fit the visible values.
    Auto,
}

/// One declared output series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutputSpec {
    /// Output name (`"value"` for the primary).
    pub name: &'static str,
    /// Primary or secondary.
    pub role: OutputRole,
    /// Display scale policy.
    pub policy: ScalePolicy,
}

/// Complete parameter/output declaration for one kernel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IndicatorSchema {
    /// Declared parameters.
    pub params: &'static [IndicatorParam],
    /// Declared outputs, primary first.
    pub outputs: &'static [OutputSpec],
}

const fn int_param(
    name: &'static str,
    default: i64,
    min: f64,
    max: f64,
    description: &'static str,
) -> IndicatorParam {
    IndicatorParam {
        name,
        kind: ParamKind::Int,
        default: ParamValue::Int(default),
        min: Some(min),
        max: Some(max),
        step: 1.0,
        description,
    }
}

const fn float_param(
    name: &'static str,
    default: f64,
    min: f64,
    max: f64,
    step: f64,
    description: &'static str,
) -> IndicatorParam {
    IndicatorParam {
        name,
        kind: ParamKind::Float,
        default: ParamValue::Float(default),
        min: Some(min),
        max: Some(max),
        step,
        description,
    }
}

const fn bool_param(
    name: &'static str,
    default: bool,
    description: &'static str,
) -> IndicatorParam {
    IndicatorParam {
        name,
        kind: ParamKind::Bool,
        default: ParamValue::Bool(default),
        min: None,
        max: None,
        step: 0.0,
        description,
    }
}

const fn output(name: &'static str, role: OutputRole, policy: ScalePolicy) -> OutputSpec {
    OutputSpec { name, role, policy }
}

const PRIMARY: OutputSpec = output("value", OutputRole::Primary, ScalePolicy::Auto);
const PRIMARY_BOUNDED: OutputSpec = output(
    "value",
    OutputRole::Primary,
    ScalePolicy::Bounded {
        min: 0.0,
        max: 100.0,
    },
);

const NO_PARAMS: &[IndicatorParam] = &[];
const LENGTH: &[IndicatorParam] = &[int_param("length", 14, 1.0, 100_000.0, "Lookback length")];

/// The moving-average period definition: the one shared row every MA
/// kernel's `length` reads, rooted in the smoothing catalog's period
/// contract. Non-MA kernels keep the wide [`LENGTH`] lookback; the settings
/// sliders and the validators read the same bounds, so the 2..200 window
/// is one fact, not two.
const MA_LENGTH: &[IndicatorParam] = &[int_param(
    "length",
    14,
    crate::smoothing::MA_PERIOD_MIN as f64,
    crate::smoothing::MA_PERIOD_MAX as f64,
    "Lookback length",
)];
const ONE: &[OutputSpec] = &[PRIMARY];
const UPPER_LOWER: &[OutputSpec] = &[
    PRIMARY,
    output("upper", OutputRole::Secondary, ScalePolicy::Auto),
    output("lower", OutputRole::Secondary, ScalePolicy::Auto),
];

const STOCH_OUT: &[OutputSpec] = &[
    PRIMARY_BOUNDED,
    output(
        "d",
        OutputRole::Secondary,
        ScalePolicy::Bounded {
            min: 0.0,
            max: 100.0,
        },
    ),
];
const STOCHF_PARAMS: &[IndicatorParam] = &[
    int_param("fastk_length", 14, 1.0, 100_000.0, "Fast %K lookback"),
    int_param("fastd_length", 3, 1.0, 100_000.0, "Fast %D smoothing"),
];
const STOCHF_OUT: &[OutputSpec] = &[
    PRIMARY_BOUNDED,
    output(
        "fastd",
        OutputRole::Secondary,
        ScalePolicy::Bounded {
            min: 0.0,
            max: 100.0,
        },
    ),
];
const MACD_PARAMS: &[IndicatorParam] = &[
    int_param("fast_length", 12, 1.0, 100_000.0, "Fast EMA length"),
    int_param("slow_length", 26, 1.0, 100_000.0, "Slow EMA length"),
    int_param("signal_length", 9, 1.0, 100_000.0, "Signal EMA length"),
];
const MACD_OUT: &[OutputSpec] = &[
    PRIMARY,
    output("signal", OutputRole::Secondary, ScalePolicy::Auto),
    output("hist", OutputRole::Secondary, ScalePolicy::Auto),
];
const BB_PARAMS: &[IndicatorParam] = &[
    int_param("length", 20, 1.0, 100_000.0, "Lookback length"),
    float_param("mult", 2.0, 0.0, 100.0, 0.1, "Band deviation multiplier"),
];
const KC_PARAMS: &[IndicatorParam] = &[
    int_param("length", 20, 1.0, 100_000.0, "Lookback length"),
    float_param("mult", 2.0, 0.0, 100.0, 0.1, "Band deviation multiplier"),
    bool_param("use_true_range", true, "Use true range for the span"),
];
const SUPERTREND_PARAMS: &[IndicatorParam] = &[
    int_param("atr_length", 14, 1.0, 100_000.0, "ATR length"),
    float_param("multiplier", 3.0, 0.0, 100.0, 0.1, "ATR multiplier"),
];
const SUPERTREND_OUT: &[OutputSpec] = &[
    PRIMARY,
    output("direction", OutputRole::Secondary, ScalePolicy::Auto),
];
const DONCHIAN_PARAMS: &[IndicatorParam] =
    &[int_param("length", 20, 1.0, 100_000.0, "Channel lookback")];
const ICHIMOKU_PARAMS: &[IndicatorParam] = &[
    int_param("tenkan_length", 9, 1.0, 100_000.0, "Tenkan-sen lookback"),
    int_param("kijun_length", 26, 1.0, 100_000.0, "Kijun-sen lookback"),
    int_param(
        "senkou_b_length",
        52,
        1.0,
        100_000.0,
        "Senkou span B lookback",
    ),
];
const ICHIMOKU_OUT: &[OutputSpec] = &[
    PRIMARY,
    output("kijun", OutputRole::Secondary, ScalePolicy::Auto),
    output("senkou_a", OutputRole::Secondary, ScalePolicy::Auto),
    output("senkou_b", OutputRole::Secondary, ScalePolicy::Auto),
    output("chikou", OutputRole::Secondary, ScalePolicy::Auto),
];
const SAR_OUT: &[OutputSpec] = &[
    PRIMARY,
    output("trend", OutputRole::Secondary, ScalePolicy::Auto),
];
const AROON_OUT: &[OutputSpec] = &[
    output(
        "value",
        OutputRole::Primary,
        ScalePolicy::Bounded { min: 0.0, max: 1.0 },
    ),
    output(
        "down",
        OutputRole::Secondary,
        ScalePolicy::Bounded { min: 0.0, max: 1.0 },
    ),
];
const ADX_OUT: &[OutputSpec] = &[
    output(
        "value",
        OutputRole::Primary,
        ScalePolicy::Bounded {
            min: 0.0,
            max: 100.0,
        },
    ),
    output(
        "plus_di",
        OutputRole::Secondary,
        ScalePolicy::Bounded {
            min: 0.0,
            max: 100.0,
        },
    ),
    output(
        "minus_di",
        OutputRole::Secondary,
        ScalePolicy::Bounded {
            min: 0.0,
            max: 100.0,
        },
    ),
];
const VOSS_OUT: &[OutputSpec] = &[
    PRIMARY,
    output("filter", OutputRole::Secondary, ScalePolicy::Auto),
];
const SAR_PARAMS: &[IndicatorParam] = &[
    float_param("start", 0.02, 0.0, 1.0, 0.01, "Initial acceleration"),
    float_param("increment", 0.02, 0.0, 1.0, 0.01, "Acceleration increment"),
    float_param("maximum", 0.2, 0.0, 1.0, 0.01, "Maximum acceleration"),
];
const APO_PARAMS: &[IndicatorParam] = &[
    int_param("fast_length", 12, 1.0, 100_000.0, "Fast smoothing length"),
    int_param("slow_length", 26, 1.0, 100_000.0, "Slow smoothing length"),
    float_param(
        "matype",
        0.0,
        0.0,
        1.0,
        1.0,
        "Smoothing type (0 SMA, 1 EMA)",
    ),
];
const ALMA_PARAMS: &[IndicatorParam] = &[
    int_param(
        "length",
        14,
        crate::smoothing::MA_PERIOD_MIN as f64,
        crate::smoothing::MA_PERIOD_MAX as f64,
        "Lookback length",
    ),
    float_param("offset", 0.85, 0.0, 1.0, 0.01, "ALMA offset"),
    float_param("sigma", 6.0, 0.0, 100.0, 0.1, "ALMA sigma"),
];
const ADOSC_PARAMS: &[IndicatorParam] = &[
    int_param("fast_length", 3, 1.0, 100_000.0, "Fast EMA length"),
    int_param("slow_length", 10, 1.0, 100_000.0, "Slow EMA length"),
];
const EIT_PARAMS: &[IndicatorParam] = &[float_param(
    "alpha",
    0.14,
    0.0,
    1.0,
    0.01,
    "Smoothing alpha",
)];
const KALMAN_PARAMS: &[IndicatorParam] = &[
    int_param(
        "length",
        14,
        crate::smoothing::MA_PERIOD_MIN as f64,
        crate::smoothing::MA_PERIOD_MAX as f64,
        "Lookback length",
    ),
    float_param("alpha", 0.012, 0.0, 1.0, 0.001, "Process coupling"),
    float_param("beta", 0.1, 0.0, 1.0, 0.001, "Measurement coupling"),
];
const VOSS_PARAMS: &[IndicatorParam] = &[
    int_param("length", 14, 1.0, 100_000.0, "Lookback length"),
    int_param("predict", 3, 1.0, 1_000.0, "Prediction order"),
    float_param("band", 0.25, 0.0, 100.0, 0.01, "Bandwidth"),
];
const STDDEV_PARAMS: &[IndicatorParam] = &[
    int_param("length", 20, 1.0, 100_000.0, "Lookback length"),
    float_param("nbdev", 2.0, 0.0, 100.0, 0.1, "Deviation multiplier"),
];

/// The one schema table every kernel and the terminal read.
#[must_use]
pub fn indicator_schema(id: &str) -> Option<IndicatorSchema> {
    let (params, outputs): (&'static [IndicatorParam], &'static [OutputSpec]) = match id {
        "sma" | "ema" | "rma" | "wma" | "hma" | "vwma" | "dema" | "tema" | "trima" => {
            (MA_LENGTH, ONE)
        }
        "rsi" | "cci" | "cmo" | "tr" | "atr" | "natr" | "midpoint" | "midprice" | "mom" | "roc"
        | "rocp" | "rocr" | "rocr100" | "rsv" | "willr" | "trix" | "correl" | "trendsi" | "mfi"
        | "cmf" => (LENGTH, ONE),
        "aroon" => (LENGTH, AROON_OUT),
        "adx" => (LENGTH, ADX_OUT),
        "stoch" | "srsi" => (LENGTH, STOCH_OUT),
        "stochf" => (STOCHF_PARAMS, STOCHF_OUT),
        "macd" => (MACD_PARAMS, MACD_OUT),
        "bb" => (BB_PARAMS, UPPER_LOWER),
        "kc" => (KC_PARAMS, UPPER_LOWER),
        "supertrend" => (SUPERTREND_PARAMS, SUPERTREND_OUT),
        "donchian" => (DONCHIAN_PARAMS, UPPER_LOWER),
        "ichimoku" => (ICHIMOKU_PARAMS, ICHIMOKU_OUT),
        "sar" => (SAR_PARAMS, SAR_OUT),
        "apo" | "ppo" => (APO_PARAMS, ONE),
        "alma" => (ALMA_PARAMS, ONE),
        "adosc" => (ADOSC_PARAMS, ONE),
        "eit" => (EIT_PARAMS, ONE),
        "kalman" => (KALMAN_PARAMS, ONE),
        "voss" => (VOSS_PARAMS, VOSS_OUT),
        "stddev" | "var" => (STDDEV_PARAMS, ONE),
        "ad" | "obv" | "vwap" | "avgprice" | "medprice" | "typprice" | "wclprice" => {
            (NO_PARAMS, ONE)
        }
        _ => return None,
    };
    Some(IndicatorSchema { params, outputs })
}

impl IndicatorSpec {
    /// Declared parameters: the per-kernel field when populated, else the
    /// central schema table.
    #[must_use]
    pub fn params(&self) -> &'static [IndicatorParam] {
        indicator_schema(&self.id).map_or(&[], |schema| schema.params)
    }

    /// Declared outputs, from the central schema table. There is deliberately
    /// no per-spec override: [`indicator_schema`] is the one table.
    #[must_use]
    pub fn outputs(&self) -> &'static [OutputSpec] {
        indicator_schema(&self.id).map_or(&[], |schema| schema.outputs)
    }

    /// Build the declared defaults for one invocation.
    #[must_use]
    pub fn default_params(&self) -> ParamSet {
        let mut params = ParamSet::new();
        for param in self.params() {
            match param.default {
                ParamValue::Int(value) => params = params.with_int(param.name, value),
                ParamValue::Float(value) => params = params.with_float(param.name, value),
                ParamValue::Bool(value) => params = params.with_bool(param.name, value),
            }
        }
        params
    }

    /// Validate a parameter set against the declared schema: unknown names
    /// refuse, declared ints/floats respect their bounds.
    pub fn validate_params(&self, params: &ParamSet) -> Result<(), KernelError> {
        for name in params.names() {
            let declared = self
                .params()
                .iter()
                .find(|param| param.name == name)
                .ok_or_else(|| {
                    KernelError::InvalidParam(format!("{}: unknown parameter {name:?}", self.id))
                })?;
            let value = match (declared.kind, params.get(name)) {
                (ParamKind::Int, Some(PineValue::Int(value))) => Some(*value as f64),
                (ParamKind::Float, Some(value)) => value.as_float(),
                (ParamKind::Bool, Some(PineValue::Bool(_))) | (_, None) => None,
                (kind, Some(got)) => {
                    return Err(KernelError::InvalidParam(format!(
                        "{}: parameter {name:?} expects {kind:?}, got {got}",
                        self.id
                    )));
                }
            };
            if let Some(value) = value {
                if !value.is_finite()
                    || declared.min.is_some_and(|min| value < min)
                    || declared.max.is_some_and(|max| value > max)
                {
                    return Err(KernelError::InvalidParam(format!(
                        "{}: parameter {name:?} is out of bounds: {value}",
                        self.id
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Static description of one indicator: identity, warmup floor, and
/// dependencies. The per-invocation refinement lives in
/// [`Indicator::warmup`], which sees the actual parameters.
#[derive(Debug, Clone, Builder, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct IndicatorSpec {
    /// Stable registry key.
    pub id: IndicatorId,
    /// Human-readable name.
    pub display_name: String,
    /// Minimum bars before output is trustworthy; parameters only raise it.
    pub warmup_bars: usize,
    /// Ids that must execute before this indicator.
    #[builder(default)]
    pub depends: Vec<IndicatorId>,
}

impl IndicatorSpec {
    /// Assets this indicator provides: itself, mirroring concrete-target
    /// self-provision in the dependency graph.
    #[must_use]
    pub fn provides(&self) -> Vec<IndicatorId> {
        vec![self.id.clone()]
    }
}

/// Typed parameters for one indicator invocation.
#[derive(Debug, Clone, Default)]
pub struct ParamSet {
    values: HashMap<String, PineValue>,
}

impl ParamSet {
    /// Create an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an int parameter.
    #[must_use]
    pub fn with_int(mut self, name: impl Into<String>, value: i64) -> Self {
        self.values.insert(name.into(), PineValue::Int(value));
        self
    }

    /// Add a float parameter.
    #[must_use]
    pub fn with_float(mut self, name: impl Into<String>, value: f64) -> Self {
        self.values.insert(name.into(), PineValue::Float(value));
        self
    }

    /// Add a bool parameter.
    #[must_use]
    pub fn with_bool(mut self, name: impl Into<String>, value: bool) -> Self {
        self.values.insert(name.into(), PineValue::Bool(value));
        self
    }

    /// Read one raw parameter value.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PineValue> {
        self.values.get(name)
    }

    /// Parameter names in sorted order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.values.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Read a required int parameter.
    pub fn int(&self, indicator: &str, name: &str) -> Result<i64, KernelError> {
        match self.values.get(name) {
            Some(PineValue::Int(v)) => Ok(*v),
            Some(got) => Err(KernelError::InvalidParam(format!(
                "{indicator}: int parameter {name:?} got {got}"
            ))),
            None => Err(KernelError::InvalidParam(format!(
                "{indicator}: missing int parameter {name:?}"
            ))),
        }
    }

    /// Read a required float parameter (ints auto-cast, as in Pine).
    pub fn float(&self, indicator: &str, name: &str) -> Result<f64, KernelError> {
        match self.values.get(name).and_then(PineValue::as_float) {
            Some(v) => Ok(v),
            None => Err(KernelError::InvalidParam(format!(
                "{indicator}: missing or non-numeric parameter {name:?}"
            ))),
        }
    }

    /// Read a required bool parameter.
    pub fn bool(&self, indicator: &str, name: &str) -> Result<bool, KernelError> {
        match self.values.get(name) {
            Some(PineValue::Bool(v)) => Ok(*v),
            Some(got) => Err(KernelError::InvalidParam(format!(
                "{indicator}: bool parameter {name:?} got {got}"
            ))),
            None => Err(KernelError::InvalidParam(format!(
                "{indicator}: missing bool parameter {name:?}"
            ))),
        }
    }

    /// Read a bool parameter, falling back to `default` when undeclared.
    pub fn bool_or(&self, indicator: &str, name: &str, default: bool) -> Result<bool, KernelError> {
        match self.values.get(name) {
            None => Ok(default),
            Some(PineValue::Bool(v)) => Ok(*v),
            Some(got) => Err(KernelError::InvalidParam(format!(
                "{indicator}: bool parameter {name:?} got {got}"
            ))),
        }
    }

    /// Canonical process-local fingerprint of the parameter map: entries
    /// sorted by name, each a length-prefixed `(name, tag, payload)` frame
    /// hashed once through the shared FNV-1a helper. Tags name the variant
    /// (`i`nt, `f`loat, `b`ool, `s`tr, `c`olor, `e`num, `a`rray, int`m`ap,
    /// ma`p`, `o`bject, `d`rawing, matri`x`); ints encode by value, floats
    /// by bits (so `0.0` and `-0.0` key distinctly, like every distinct bit
    /// pattern), and composite values recurse in sorted order. Two sets
    /// fingerprint equal iff they carry the same names with equal values
    /// (insertion order never matters). `Int` vs `Float` stay distinct —
    /// they already behave distinctly in `period()`. Identity for
    /// in-memory caches only — never persisted, so no stability promise
    /// beyond the process; the golden suite pins the current values so any
    /// change is a deliberate cache-invalidation event. Thin domain name
    /// over the shared length-framed builder: each value pre-encodes
    /// through [`encode_param_value`] (self-delimiting tag frames), so the
    /// bytes — and the golden digests — never change.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut names: Vec<&String> = self.values.keys().collect();
        names.sort();
        let mut encoded: Vec<(&str, Vec<u8>)> = Vec::with_capacity(names.len());
        for name in names {
            if let Some(value) = self.values.get(name) {
                let mut payload = Vec::new();
                encode_param_value(&mut payload, value);
                encoded.push((name.as_str(), payload));
            }
        }
        let frames: Vec<(&str, &[u8])> = encoded
            .iter()
            .map(|(name, payload)| (*name, payload.as_slice()))
            .collect();
        common_core::hash::fingerprint_framed(&frames)
    }
}

/// Append one canonical `(tag, payload)` frame for a parameter value.
/// Length-prefixes keep frames unambiguous; sorting at every map level
/// keeps the encoding independent of hash-map iteration order.
///
/// Public so tape-data identity (server `report`) frames with the same
/// idiom parameter identity uses: one framing implementation, not two.
pub fn encode_param_value(bytes: &mut Vec<u8>, value: &PineValue) {
    fn push_str(bytes: &mut Vec<u8>, text: &str) {
        bytes.extend_from_slice(&(text.len() as u64).to_le_bytes());
        bytes.extend_from_slice(text.as_bytes());
    }
    fn push_opt(bytes: &mut Vec<u8>, slot: Option<&PineValue>) {
        match slot {
            None => bytes.push(0),
            Some(present) => {
                bytes.push(1);
                encode_param_value(bytes, present);
            }
        }
    }
    match value {
        PineValue::Int(v) => {
            bytes.push(b'i');
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        PineValue::Float(v) => {
            bytes.push(b'f');
            bytes.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        PineValue::Bool(v) => {
            bytes.push(b'b');
            bytes.push(u8::from(*v));
        }
        PineValue::Str(v) => {
            bytes.push(b's');
            push_str(bytes, v);
        }
        PineValue::ColorHandle(v) => {
            bytes.push(b'c');
            bytes.extend_from_slice(&v.raw().to_le_bytes());
        }
        PineValue::EnumHandle(v) => {
            bytes.push(b'e');
            push_str(bytes, &v.enum_name);
            push_str(bytes, &v.member);
        }
        PineValue::Array(items) => {
            bytes.push(b'a');
            bytes.extend_from_slice(&(items.len() as u64).to_le_bytes());
            for item in items {
                push_opt(bytes, item.as_ref());
            }
        }
        PineValue::IntMap(entries) => {
            bytes.push(b'm');
            let mut keys: Vec<i64> = entries.keys().copied().collect();
            keys.sort_unstable();
            bytes.extend_from_slice(&(keys.len() as u64).to_le_bytes());
            for key in keys {
                let Some(mapped) = entries.get(&key) else {
                    continue;
                };
                bytes.extend_from_slice(&key.to_le_bytes());
                bytes.extend_from_slice(&mapped.to_le_bytes());
            }
        }
        PineValue::Map(entries) => {
            bytes.push(b'p');
            bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
            for (key, (mapped_key, mapped_value)) in entries {
                push_str(bytes, key);
                encode_param_value(bytes, mapped_key);
                push_opt(bytes, mapped_value.as_ref());
            }
        }
        PineValue::Object(instance) => {
            bytes.push(b'o');
            push_str(bytes, &instance.type_name);
            let mut fields: Vec<&String> = instance.fields.keys().collect();
            fields.sort();
            bytes.extend_from_slice(&(fields.len() as u64).to_le_bytes());
            for field in fields {
                let Some(slot) = instance.fields.get(field) else {
                    continue;
                };
                push_str(bytes, field);
                push_opt(bytes, slot.as_ref());
            }
        }
        PineValue::Drawing(handle) => {
            bytes.push(b'd');
            let kind = match handle.kind {
                crate::value::DrawingKind::Label => 0,
                crate::value::DrawingKind::Table => 1,
                crate::value::DrawingKind::Box => 2,
                crate::value::DrawingKind::Line => 3,
            };
            bytes.push(kind);
            bytes.extend_from_slice(&handle.id.to_le_bytes());
        }
        PineValue::Matrix(grid) => {
            bytes.push(b'x');
            bytes.extend_from_slice(&(grid.rows as u64).to_le_bytes());
            bytes.extend_from_slice(&(grid.cols as u64).to_le_bytes());
            bytes.extend_from_slice(&(grid.cells.len() as u64).to_le_bytes());
            for cell in &grid.cells {
                push_opt(bytes, cell.as_ref());
            }
        }
    }
}

/// One named secondary output series (e.g. `upper` for Bollinger Bands).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedSeries {
    /// Output name.
    pub name: String,
    /// One entry per input bar.
    pub values: Vec<Option<f64>>,
}

impl NamedSeries {
    /// Pair a name with its per-bar values.
    #[must_use]
    pub fn new(name: impl Into<String>, values: Vec<Option<f64>>) -> Self {
        Self {
            name: name.into(),
            values,
        }
    }
}

/// Output of one [`batch`](Indicator::batch) evaluation, aligned with the
/// input bars: `None` marks warming or otherwise uncomputable positions.
/// `values` is the primary output; multi-output kernels (MACD, bands,
/// stochastics) attach the rest as [`secondary`](Self::secondary).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndicatorResult {
    /// Id of the producing indicator.
    pub spec_id: IndicatorId,
    /// Primary per-bar output, one entry per input bar.
    pub values: Vec<Option<f64>>,
    /// Secondary per-bar outputs, each aligned with the input bars.
    #[serde(default)]
    pub secondary: Vec<NamedSeries>,
}

impl IndicatorResult {
    /// Pair an id with its primary per-bar values.
    #[must_use]
    pub fn new(spec_id: IndicatorId, values: Vec<Option<f64>>) -> Self {
        Self {
            spec_id,
            values,
            secondary: Vec::new(),
        }
    }

    /// Attach a secondary output series.
    #[must_use]
    pub fn with_secondary(mut self, series: NamedSeries) -> Self {
        self.secondary.push(series);
        self
    }

    /// Borrow a secondary series by name.
    #[must_use]
    pub fn secondary(&self, name: &str) -> Option<&[Option<f64>]> {
        self.secondary
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.values.as_slice())
    }

    /// Number of per-bar values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no values were produced.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// One named secondary value from [`update_full`](Indicator::update_full).
#[derive(Debug, Clone, PartialEq)]
pub struct NamedValue {
    /// Output name.
    pub name: String,
    /// Value for this bar.
    pub value: Option<f64>,
}

/// One bar of multi-output evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct FullTick {
    /// Primary value for this bar.
    pub primary: Option<f64>,
    /// Secondary values for this bar.
    pub secondary: Vec<NamedValue>,
}

/// One kernel's resolved scalars, memoised on its [`IndicatorState`].
///
/// A state belongs to **one run of one kernel**, and a run's parameters are
/// fixed for its lifetime, so the first resolution is the run's: later bars read
/// a copy instead of re-reading the boundary [`ParamSet`] (a `HashMap` of
/// `PineValue`s) once per bar. `T` is whatever the kernel's own resolver returns
/// — a `usize` or a small tuple it already defines — never a shared bag of
/// untyped slots.
///
/// The cache is pure, so it never has to be kept in step: cloning a state drops
/// it (the clone re-resolves on its first bar) and a failed resolution stores
/// nothing (the next bar retries). Asking one state for a *different* `T` than
/// it cached is a caller bug — two kernels sharing a state — and refuses rather
/// than silently rebuilding under the first kernel's feet.
#[derive(Default)]
pub struct ResolvedCache {
    value: Option<Box<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for ResolvedCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.value.is_some() {
            "ResolvedCache(<resolved>)"
        } else {
            "ResolvedCache(empty)"
        })
    }
}

impl Clone for ResolvedCache {
    /// A clone starts empty: the memo is a pure value, and a cloned state is a
    /// fresh run window that re-resolves from its own first bar.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl ResolvedCache {
    /// Read the cached value, or build it once with `build`.
    ///
    /// # Errors
    ///
    /// The build's own refusal (nothing is stored, so the caller retries on the
    /// next bar), or a refusal when this state already holds another `T`.
    pub fn get<T: Copy + Send + Sync + 'static>(
        &mut self,
        build: impl FnOnce() -> Result<T, KernelError>,
    ) -> Result<T, KernelError> {
        if let Some(cached) = self.value.as_ref() {
            return cached.downcast_ref::<T>().copied().ok_or_else(|| {
                KernelError::InvalidParam(
                    "indicator state was reused across kernels: its resolved scalars are \
                     a different type"
                        .to_owned(),
                )
            });
        }
        let built = build()?;
        self.value = Some(Box::new(built));
        Ok(built)
    }
}

/// Incremental evaluation state: recent per-bar ticks, the last emitted
/// value, and child states for composed kernels (MACD's three EMAs, RSI's
/// gain/loss smoothers, …). Single- and multi-output paths share it, so
/// `batch`/`update` agreement covers every emitted series.
#[derive(Debug, Clone, Default)]
pub struct IndicatorState {
    history: VecDeque<BarTick>,
    last: Option<f64>,
    children: Vec<IndicatorState>,
    /// Composed kernels' child `ParamSet`s, index-aligned with `children`:
    /// each is built once per slot from the validated parameters and reused
    /// across bars, so `update_full` reads a reference instead of
    /// re-constructing a parameter map every bar.
    child_params: Vec<Option<ParamSet>>,
    /// Clear-and-refill window scratch: every `scratch_*` method refills
    /// this one buffer instead of collecting a fresh `Vec` per bar, so the
    /// incremental path allocates once per state lifetime, never once per
    /// bar. Pair/triple fills stack their legs back-to-back in the same
    /// buffer and split it — still one buffer, still no per-bar allocation.
    scratch: Vec<f64>,
    /// The kernel's scalars, resolved once from its parameters (see
    /// [`ResolvedCache`]).
    scalars: ResolvedCache,
}

impl IndicatorState {
    /// Create empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a tick, keeping at most `period` recent bars.
    pub fn push(&mut self, tick: BarTick, period: usize) {
        self.history.push_back(tick);
        while self.history.len() > period.max(1) {
            self.history.pop_front();
        }
    }

    /// Bars retained, oldest first.
    #[must_use]
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// Whether no bars are retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// Retained closes, oldest first.
    pub fn closes(&self) -> impl Iterator<Item = f64> + '_ {
        self.history.iter().map(|t| t.close)
    }

    /// Retained highs, oldest first.
    pub fn highs(&self) -> impl Iterator<Item = f64> + '_ {
        self.history.iter().map(|t| t.high)
    }

    /// Retained lows, oldest first.
    pub fn lows(&self) -> impl Iterator<Item = f64> + '_ {
        self.history.iter().map(|t| t.low)
    }

    /// Retained volumes, oldest first.
    pub fn volumes(&self) -> impl Iterator<Item = f64> + '_ {
        self.history.iter().map(|t| t.volume)
    }

    /// Retained closes as a reused slice, oldest first: refills the owned
    /// scratch buffer (clear-and-refill, no allocation past the first fill
    /// at a given capacity) instead of collecting a fresh `Vec` per bar.
    /// The slice borrows the state, so child reads and `set_last` go after
    /// its last use — data movement only, the arithmetic op order never
    /// moves. Empty history yields an empty slice, never a refusal.
    pub fn scratch_closes(&mut self) -> &[f64] {
        self.scratch.clear();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.close));
        &self.scratch
    }

    /// Retained volumes as a reused slice, oldest first. Same reuse
    /// contract as [`scratch_closes`](Self::scratch_closes).
    pub fn scratch_volumes(&mut self) -> &[f64] {
        self.scratch.clear();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.volume));
        &self.scratch
    }

    /// Retained closes then volumes as reused slices, oldest first: one
    /// buffer, two legs stacked back-to-back. Same reuse contract as
    /// [`scratch_closes`](Self::scratch_closes).
    pub fn scratch_closes_volumes(&mut self) -> (&[f64], &[f64]) {
        self.scratch.clear();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.close));
        let mid = self.scratch.len();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.volume));
        let (closes, volumes) = self.scratch.split_at(mid);
        (closes, volumes)
    }

    /// Retained highs then lows as reused slices, oldest first: one buffer,
    /// two legs stacked back-to-back. Same reuse contract as
    /// [`scratch_closes`](Self::scratch_closes).
    pub fn scratch_highs_lows(&mut self) -> (&[f64], &[f64]) {
        self.scratch.clear();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.high));
        let mid = self.scratch.len();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.low));
        let (highs, lows) = self.scratch.split_at(mid);
        (highs, lows)
    }

    /// Retained closes then bar-relative indices as reused slices, oldest
    /// first: one buffer, two legs stacked back-to-back. The indices read
    /// `0.0` oldest — correlation against them is shift-invariant, so a
    /// window-relative index reproduces the absolute `bar_index` the Pine
    /// arm correlates against, without carrying a run counter in the state.
    /// Same reuse contract as [`scratch_closes`](Self::scratch_closes).
    pub fn scratch_closes_indices(&mut self) -> (&[f64], &[f64]) {
        self.scratch.clear();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.close));
        let mid = self.scratch.len();
        self.scratch.extend((0..mid).map(|index| index as f64));
        let (closes, indices) = self.scratch.split_at(mid);
        (closes, indices)
    }

    /// Retained highs, lows, then closes as reused slices, oldest first:
    /// one buffer, three legs stacked back-to-back. Same reuse contract as
    /// [`scratch_closes`](Self::scratch_closes).
    pub fn scratch_highs_lows_closes(&mut self) -> (&[f64], &[f64], &[f64]) {
        self.scratch.clear();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.high));
        let highs_end = self.scratch.len();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.low));
        let lows_end = self.scratch.len();
        self.scratch
            .extend(self.history.iter().map(|tick| tick.close));
        let (head, closes) = self.scratch.split_at(lows_end);
        let (highs, lows) = head.split_at(highs_end);
        (highs, lows, closes)
    }

    /// Previous close (the bar before the one just pushed), if retained.
    #[must_use]
    pub fn prev_close(&self) -> Option<f64> {
        let n = self.history.len();
        if n >= 2 {
            Some(self.history[n - 2].close)
        } else {
            None
        }
    }

    /// Borrow the `index`-th child state, growing the family on demand.
    pub fn child(&mut self, index: usize) -> &mut IndicatorState {
        while self.children.len() <= index {
            self.children.push(IndicatorState::new());
        }
        &mut self.children[index]
    }

    /// Borrow a composed leg's child state together with its cached child
    /// parameters. `build` runs only the first time a slot is requested;
    /// later bars reuse the same `ParamSet`, so the incremental path reads a
    /// reference instead of allocating a parameter map per bar. Parameters
    /// are fixed for a state's lifetime, exactly like its window.
    pub fn child_with_params(
        &mut self,
        index: usize,
        build: impl FnOnce() -> ParamSet,
    ) -> (&mut IndicatorState, &ParamSet) {
        while self.children.len() <= index {
            self.children.push(IndicatorState::new());
        }
        if self.child_params.len() <= index {
            self.child_params.resize_with(index + 1, || None);
        }
        let params = self.child_params[index].get_or_insert_with(build);
        (&mut self.children[index], params)
    }

    /// The kernel's resolved scalars for this state: built once from the first
    /// bar's parameters, then read as a copy on every later bar.
    ///
    /// Parameters are fixed for a state's lifetime (exactly like its window),
    /// so this is a memo of a pure value, never a second source of truth.
    ///
    /// # Errors
    ///
    /// The kernel's own parameter refusal, or a refusal when the state already
    /// holds a different scalar type (two kernels sharing one state).
    pub fn resolved<T: Copy + Send + Sync + 'static>(
        &mut self,
        build: impl FnOnce() -> Result<T, KernelError>,
    ) -> Result<T, KernelError> {
        self.scalars.get(build)
    }

    /// Last emitted value, if any.
    #[must_use]
    pub fn last(&self) -> Option<f64> {
        self.last
    }

    /// Record the last emitted value.
    pub fn set_last(&mut self, value: f64) {
        self.last = Some(value);
    }
}

/// The indicator contract: identity, warmup honesty, whole-window batch
/// evaluation, and single-bar incremental update. `batch` and `update` must
/// agree position by position over the same input, including every
/// secondary series (checked through [`update_full`](Indicator::update_full)).
pub trait Indicator: Send + Sync {
    /// Static description of this indicator.
    fn spec(&self) -> &IndicatorSpec;

    /// Bars needed before the first trustworthy output for these parameters.
    ///
    /// The static [`IndicatorSpec::warmup_bars`] is the default, and
    /// [`refine_warmup`](Self::refine_warmup) is the *only* way a kernel may
    /// raise it: one parameter-dependent channel, combined here, so no kernel
    /// can carry a second answer beside its spec.
    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(self
            .refine_warmup(params)?
            .unwrap_or(self.spec().warmup_bars))
    }

    /// Bars before the first output when the parameters say more than the
    /// static spec does: `None` (the default) means the static
    /// [`warmup_bars`](IndicatorSpec::warmup_bars) applies unchanged.
    fn refine_warmup(&self, _params: &ParamSet) -> Result<Option<usize>, KernelError> {
        Ok(None)
    }

    /// Evaluate over whole windows. Output length always equals input length;
    /// positions without enough history are `None`, never an error.
    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError>;

    /// Evaluate one bar, advancing `state`. Must agree with [`batch`](Self::batch)
    /// position by position over the same input.
    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError>;

    /// Evaluate one bar with every secondary output. The default derives
    /// from [`update`](Self::update); multi-output kernels override so their
    /// secondary series enjoy the same batch/update identity.
    fn update_full(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<FullTick, KernelError> {
        Ok(FullTick {
            primary: self.update(state, bar, params)?,
            secondary: Vec::new(),
        })
    }

    /// Frame-computable sub-series this kernel composes, with real
    /// parameters derived from `params`: each `(id, sub-params)` pair names a
    /// series the shared store can compute once and share. The default is
    /// empty (leaf kernels compose nothing); composers override for the
    /// passes that read frame legs directly (first TRIMA pass, DEMA/TEMA
    /// first pass, MACD fast/slow legs, the AD line under ADOSC). Passes over
    /// *intermediate* series (second TRIMA pass, MACD signal line) stay
    /// inline and are never declared here — they read no shared frame, so
    /// sharing them would need a second data flow, not an edge. Every
    /// returned id must also appear in the static [`spec`](Self::spec)
    /// `depends` list (the id-level ordering channel); invalid `params`
    /// declare nothing and surface at compute time instead.
    fn deps_with_params(&self, _params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        Vec::new()
    }
}

/// Registry of indicators keyed by spec id.
///
/// What the ownership rule actually forbids is **per-run state** in a shared
/// place: accumulators, cursors, "last bar" — anything two concurrent runs could
/// tread on. This type holds none. Its entries are `Arc<dyn Indicator>` over
/// stateless kernels (each `batch`/`update` call carries its own state —
/// [`IndicatorState`] for the incremental path, `params` for both), so a registry
/// is a pure value: two of them built from [`builtin_registry`] are
/// interchangeable, and caching one is not a second source of truth.
///
/// That is why [`shared_registry`] may keep one in a `static`: the static is an
/// **immutable cache of a pure value**, not shared mutable state. The owned
/// constructor stays [`builtin_registry`], and a test that wants isolation can
/// still build its own — nothing requires the shared handle.
///
/// Duplicate policy: first-wins, duplicates refuse `Err(DuplicateIndicator)`.
/// `ids()` returns sorted order via [`KeyedRegistry`].
#[derive(Default)]
pub struct IndicatorRegistry {
    indicators: KeyedRegistry<IndicatorId, Arc<dyn Indicator>>,
}

impl IndicatorRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an indicator. Duplicate ids are refused.
    pub fn register(&mut self, indicator: Arc<dyn Indicator>) -> Result<(), KernelError> {
        let id = indicator.spec().id.clone();
        if self.indicators.contains(&id) {
            return Err(KernelError::DuplicateIndicator(id));
        }
        self.indicators.insert(id, indicator);
        Ok(())
    }

    /// Look up an indicator by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<dyn Indicator>> {
        self.indicators.get(id)
    }

    /// Iterate over registered entries.
    pub fn iter(&self) -> impl Iterator<Item = (&IndicatorId, &Arc<dyn Indicator>)> {
        self.indicators.iter()
    }

    /// Registered ids in sorted order.
    #[must_use]
    pub fn ids(&self) -> Vec<IndicatorId> {
        self.indicators.keys().cloned().collect()
    }

    /// Number of registered indicators.
    #[must_use]
    pub fn len(&self) -> usize {
        self.indicators.len()
    }

    /// Whether no indicators are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indicators.is_empty()
    }
}

/// Registry preloaded with the shipped kernels.
pub fn builtin_registry() -> Result<IndicatorRegistry, KernelError> {
    #[cfg(test)]
    OWNED_BUILDS.with(|count| count.set(count.get() + 1));
    let mut registry = IndicatorRegistry::new();
    registry.register(Arc::new(SmaIndicator::new()))?;
    registry.register(Arc::new(EmaIndicator::new()))?;
    registry.register(Arc::new(crate::averages::RmaIndicator::new()))?;
    registry.register(Arc::new(crate::averages::WmaIndicator::new()))?;
    registry.register(Arc::new(crate::averages::HmaIndicator::new()))?;
    registry.register(Arc::new(crate::averages::VwmaIndicator::new()))?;
    registry.register(Arc::new(crate::averages::AlmaIndicator::new()))?;
    registry.register(Arc::new(crate::averages::DemaIndicator::new()))?;
    registry.register(Arc::new(crate::averages::TemaIndicator::new()))?;
    registry.register(Arc::new(crate::oscillators::RsiIndicator::new()))?;
    registry.register(Arc::new(crate::oscillators::CciIndicator::new()))?;
    registry.register(Arc::new(crate::oscillators::CmoIndicator::new()))?;
    registry.register(Arc::new(crate::oscillators::MacdIndicator::new()))?;
    registry.register(Arc::new(crate::oscillators::StochIndicator::new()))?;
    registry.register(Arc::new(crate::oscillators::SrsiIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::TrIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::AtrIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::BbIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::KcIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::NatrIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::SupertrendIndicator::new()))?;
    registry.register(Arc::new(crate::ta_overlap::DonchianIndicator::new()))?;
    registry.register(Arc::new(crate::ta_overlap::IchimokuIndicator::new()))?;
    registry.register(Arc::new(crate::ta_overlap::PsarIndicator::new()))?;
    registry.register(Arc::new(crate::ta_overlap::MidpointIndicator::new()))?;
    registry.register(Arc::new(crate::ta_overlap::MidpriceIndicator::new()))?;
    registry.register(Arc::new(crate::ta_overlap::TrimaIndicator::new()))?;
    registry.register(Arc::new(crate::ta_price::AvgPriceIndicator::new()))?;
    registry.register(Arc::new(crate::ta_price::MedPriceIndicator::new()))?;
    registry.register(Arc::new(crate::ta_price::TypPriceIndicator::new()))?;
    registry.register(Arc::new(crate::ta_price::WclPriceIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::ApoIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::MomIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::PpoIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::RocIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::RocpIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::RocrIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::Rocr100Indicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::RsvIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::StochfIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::TrixIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::WillrIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::AroonIndicator::new()))?;
    registry.register(Arc::new(crate::ta_momentum::AdxIndicator::new()))?;
    registry.register(Arc::new(crate::ta_stat::StddevIndicator::new()))?;
    registry.register(Arc::new(crate::ta_stat::VarIndicator::new()))?;
    registry.register(Arc::new(crate::ta_stat::CorrelIndicator::new()))?;
    registry.register(Arc::new(crate::ta_stat::TrendsiIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::AdIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::MfiIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::VwapIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::AdoscIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::ObvIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::CmfIndicator::new()))?;
    registry.register(Arc::new(crate::filters::EitIndicator::new()))?;
    registry.register(Arc::new(crate::filters::KalmanIndicator::new()))?;
    registry.register(Arc::new(crate::filters::VossIndicator::new()))?;
    Ok(registry)
}

/// Order `requested` indicators so dependencies execute first, using the
/// canonical dependency graph. Unknown requested ids and dependencies on
/// unregistered indicators are [`GraphError::NodeNotFound`]; dependency
/// loops surface as [`GraphError::CircularDependency`].
pub fn execution_order(
    registry: &IndicatorRegistry,
    requested: &[IndicatorId],
) -> Result<Vec<IndicatorId>, GraphError> {
    crate::order::execution_order_from(registry, requested)
}

impl crate::order::Edged for IndicatorRegistry {
    fn edges(&self) -> Vec<crate::order::KernelEdges> {
        self.iter()
            .map(|(id, indicator)| {
                let spec = indicator.spec();
                crate::order::KernelEdges {
                    id: id.clone(),
                    depends: spec.depends.clone(),
                    provides: spec.provides(),
                }
            })
            .collect()
    }

    fn holds(&self, id: &IndicatorId) -> bool {
        self.get(id).is_some()
    }
}

/// The process-wide immutable kernel catalog: [`builtin_registry`] built once
/// and shared by every reader.
///
/// This is the first non-zero-sized `static` in this crate, and it is not a
/// violation of the instance-ownership rule: the value holds stateless kernels
/// only (see [`IndicatorRegistry`]), so there is nothing for one run to tread on
/// in another. Callers that mutate the catalog (registering a test kernel) build
/// their own with [`builtin_registry`] and keep it local.
///
/// # Errors
///
/// The same construction refusals [`builtin_registry`] can report (a duplicate
/// registration). A failed build installs nothing, so a later call retries
/// instead of inheriting a poisoned registry.
pub fn shared_registry() -> Result<&'static IndicatorRegistry, KernelError> {
    if let Some(registry) = SHARED_REGISTRY.get() {
        return Ok(registry);
    }
    let built = builtin_registry()?;
    // Two readers may race here; the loser's registry is dropped and both read
    // the winner, so every caller in the process still shares one address.
    if SHARED_REGISTRY.set(built).is_ok() {
        SHARED_BUILDS.fetch_add(1, Ordering::Relaxed);
    }
    Ok(SHARED_REGISTRY
        .get()
        .expect("the shared registry was installed above"))
}

/// Successful shared-registry installations. At most one per process; the
/// counter exists so the tests can prove a migrated hot path never rebuilds the
/// catalog, instead of trusting a benchmark nobody reads.
static SHARED_BUILDS: AtomicUsize = AtomicUsize::new(0);

static SHARED_REGISTRY: OnceLock<IndicatorRegistry> = OnceLock::new();

/// How many times the shared registry has been *installed*, for the cache
/// tests. Always at most one; a larger value means the `OnceLock` was replaced
/// by a second mechanism.
#[cfg(test)]
#[must_use]
pub(crate) fn shared_registry_builds() -> usize {
    SHARED_BUILDS.load(Ordering::Relaxed)
}

// `builtin_registry` calls on this thread, for the hot-path pin. Thread-local
// on purpose: tests run in parallel, so a process-wide counter would see other
// tests' legitimate owned builds and turn the pin into a flake. The hot path is
// synchronous on the calling thread, which is exactly what this measures.
#[cfg(test)]
thread_local! {
    static OWNED_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Owned-constructor calls observed on **this thread**, for the cache tests.
/// A hot path that reads [`shared_registry`] never moves this count.
#[cfg(test)]
#[must_use]
pub(crate) fn owned_registry_builds() -> usize {
    OWNED_BUILDS.with(std::cell::Cell::get)
}

/// Give stateless kernel types a process-wide instance.
///
/// A kernel's only field is its static [`IndicatorSpec`]; per-run state lives in
/// the caller's [`IndicatorState`]. Composed kernels therefore hold `&'static`
/// children (`EmaIndicator::shared()`, …) and never build — or allocate — a spec
/// per bar, while keeping static dispatch in the per-bar loop.
macro_rules! shared_kernels {
    ($($kernel:ty),+ $(,)?) => {
        $(
            impl $kernel {
                /// Process-wide instance: the kernel is immutable, so one is
                /// enough for every caller. Per-run state stays in the
                /// caller's [`IndicatorState`].
                #[must_use]
                pub fn shared() -> &'static Self {
                    static SHARED: OnceLock<$kernel> = OnceLock::new();
                    SHARED.get_or_init(<$kernel>::new)
                }
            }
        )+
    };
}

shared_kernels!(
    SmaIndicator,
    EmaIndicator,
    crate::averages::RmaIndicator,
    crate::averages::WmaIndicator,
    crate::ta_volume::AdIndicator,
    crate::volatility::AtrIndicator,
);

/// Bit patterns of a series: `None` stays `None`, a value compares by every
/// bit. The one home for the exact batch/update folds — a float comparison at
/// an epsilon would let a recurrence drift while the fold still "agreed".
#[cfg(test)]
#[must_use]
pub(crate) fn bits_of(values: &[Option<f64>]) -> Vec<Option<u64>> {
    values.iter().map(|value| value.map(f64::to_bits)).collect()
}

#[cfg(test)]
#[path = "../tests/indicator.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/schema.rs"]
mod schema_tests;

/// All-kernel `batch`/`update_full` identity fold and the fixture it derives.
#[cfg(test)]
#[path = "../tests/kernel_identity.rs"]
mod kernel_identity_tests;

/// One-build/shared-address/concurrent-read pins for the process-wide catalog.
#[cfg(test)]
#[path = "../tests/shared_registry.rs"]
mod shared_registry_tests;

/// Memo-of-a-pure-value pins for the per-state resolved-scalar cache.
#[cfg(test)]
#[path = "../tests/resolved_cache.rs"]
mod resolved_cache_tests;

/// The shell-built families answer exactly at their declared warmup.
#[cfg(test)]
#[path = "../tests/warmup.rs"]
mod warmup_tests;

#[cfg(test)]
#[path = "../tests/param_fingerprint.rs"]
mod param_fingerprint_tests;

#[cfg(test)]
#[path = "../tests/golden.rs"]
mod golden_tests;

#[cfg(test)]
#[path = "../tests/tv_spot.rs"]
mod tv_spot_tests;
