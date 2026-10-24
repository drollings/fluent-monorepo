//! Single moving-average truth: every `factorySmoothing` arm named once.
//!
//! The Pine `UTIL.Smoothings` dispatch, the kernel registry, and the terminal
//! arm builders each spelled a subset before; this catalog maps all eleven
//! arms to pass-through, direct-kernel, or recipe evaluation with one
//! parameter spelling. Arguments mirror `factorySmoothing` (`length`, `n1`,
//! `param2` with the same defaults), so a Pine edit that adds an arm fails
//! the dispatch-coverage test here first.

use crate::error::KernelError;
use crate::indicator::{fin_or_none, OwnedOhlcv, ParamSet};

/// The MA period contract: every moving-average length in the workspace —
/// kernel MA schemas, the Fast MAs line lengths — spans this window. One
/// root: the schema metadata, the validators, and the settings sliders all
/// read these bounds, so the slider window and the refusal window are the
/// same window by construction.
pub const MA_PERIOD_MIN: i64 = 2;
/// See [`MA_PERIOD_MIN`].
pub const MA_PERIOD_MAX: i64 = 200;

/// Pine `factorySmoothing` default for `n1` (EIT alpha, ALMA offset).
pub const DEFAULT_N1: f64 = 0.85;
/// Pine `factorySmoothing` default for `param2` (ALMA sigma).
pub const DEFAULT_PARAM2: f64 = 6.0;

/// Which smoothing family an arm selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SmoothingFamily {
    /// Pass-through: the data itself.
    Na,
    /// Arnaud Legoux average (`alma` kernel).
    Alma,
    /// Exponential Indicator Transform (`eit` kernel, alpha-driven).
    Eit,
    /// Exponential average (`ema` kernel).
    Ema,
    /// Hull average (`hma` kernel).
    Hma,
    /// Scalar Kalman filter (`kalman` kernel, kernel defaults).
    Kalman,
    /// Wilder smoothing (`rma` kernel).
    Rma,
    /// Simple average (`sma` kernel).
    Sma,
    /// Volume-weighted average (`vwma` kernel).
    Vwma,
    /// Weighted average (`wma` kernel).
    Wma,
    /// Zero-lag EMA: `ema` over `2*data - data[floor((length-1)/2)]`.
    Zlema,
}

impl SmoothingFamily {
    /// Every family, in Pine-dispatch order.
    #[must_use]
    pub fn all() -> [Self; 11] {
        [
            Self::Na,
            Self::Alma,
            Self::Eit,
            Self::Ema,
            Self::Hma,
            Self::Kalman,
            Self::Rma,
            Self::Sma,
            Self::Vwma,
            Self::Wma,
            Self::Zlema,
        ]
    }

    /// Canonical lowercase id.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Na => "na",
            Self::Alma => "alma",
            Self::Eit => "eit",
            Self::Ema => "ema",
            Self::Hma => "hma",
            Self::Kalman => "kalman",
            Self::Rma => "rma",
            Self::Sma => "sma",
            Self::Vwma => "vwma",
            Self::Wma => "wma",
            Self::Zlema => "zlema",
        }
    }

    /// Pine `UTIL.Smoothings` spelling.
    #[must_use]
    pub fn pine_name(self) -> &'static str {
        match self {
            Self::Na => "NA",
            Self::Alma => "ALMA",
            Self::Eit => "EIT",
            Self::Ema => "EMA",
            Self::Hma => "HMA",
            Self::Kalman => "KALMAN",
            Self::Rma => "RMA",
            Self::Sma => "SMA",
            Self::Vwma => "VWMA",
            Self::Wma => "WMA",
            Self::Zlema => "ZLEMA",
        }
    }

    /// Human label for pickers.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Na => "None (pass-through)",
            Self::Alma => "ALMA",
            Self::Eit => "EIT",
            Self::Ema => "EMA",
            Self::Hma => "HMA",
            Self::Kalman => "Kalman",
            Self::Rma => "RMA",
            Self::Sma => "SMA",
            Self::Vwma => "VWMA",
            Self::Wma => "WMA",
            Self::Zlema => "ZLEMA",
        }
    }

    /// Parse a canonical id or Pine spelling (case-insensitive).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "na" | "-" => Some(Self::Na),
            "alma" => Some(Self::Alma),
            "eit" => Some(Self::Eit),
            "ema" => Some(Self::Ema),
            "hma" => Some(Self::Hma),
            "kalman" => Some(Self::Kalman),
            "rma" => Some(Self::Rma),
            "sma" => Some(Self::Sma),
            "vwma" => Some(Self::Vwma),
            "wma" => Some(Self::Wma),
            "zlema" => Some(Self::Zlema),
            _ => None,
        }
    }

    /// Direct kernel id, if the family evaluates through one registry kernel.
    /// `Na` is pass-through (none); `Zlema` names the recipe's kernel (`ema`).
    #[must_use]
    pub fn kernel_id(self) -> Option<&'static str> {
        match self {
            Self::Na => None,
            Self::Alma => Some("alma"),
            Self::Eit => Some("eit"),
            Self::Ema | Self::Zlema => Some("ema"),
            Self::Hma => Some("hma"),
            Self::Kalman => Some("kalman"),
            Self::Rma => Some("rma"),
            Self::Sma => Some("sma"),
            Self::Vwma => Some("vwma"),
            Self::Wma => Some("wma"),
        }
    }

    /// True for pass-through evaluation (no kernel runs).
    #[must_use]
    pub fn is_passthrough(self) -> bool {
        matches!(self, Self::Na)
    }

    /// True for composed evaluation (a derived series through a kernel).
    #[must_use]
    pub fn is_recipe(self) -> bool {
        matches!(self, Self::Zlema)
    }

    /// Whether `length` alone supplies the parameters. Only `Eit` needs an
    /// alpha that length-only arms cannot set.
    #[must_use]
    pub fn takes_length(self) -> bool {
        !matches!(self, Self::Eit)
    }

    /// Parameter specs in `factorySmoothing` order: `length`, then the `n1` /
    /// `param2` slots the family reads. Families ignore slots they do not
    /// name, exactly like the Pine dispatch ignores unused `n1` / `param2`.
    #[must_use]
    pub fn param_specs(self) -> &'static [ParamSpec] {
        match self {
            Self::Alma => &[ParamSpec::Length, ParamSpec::Offset, ParamSpec::Sigma],
            Self::Eit => &[ParamSpec::Length, ParamSpec::Alpha],
            Self::Kalman => &[
                ParamSpec::Length,
                ParamSpec::KalmanAlpha,
                ParamSpec::KalmanBeta,
            ],
            Self::Na
            | Self::Ema
            | Self::Hma
            | Self::Rma
            | Self::Sma
            | Self::Vwma
            | Self::Wma
            | Self::Zlema => &[ParamSpec::Length],
        }
    }

    /// Build kernel parameters from a bare length with Pine defaults for the
    /// rest (ALMA offset/sigma, Kalman alpha/beta). Refuses `Eit`, whose alpha
    /// no length implies, and non-positive lengths, like the Pine gate.
    pub fn params_for_length(&self, length: i64) -> Result<ParamSet, KernelError> {
        if matches!(self, Self::Eit) {
            return Err(KernelError::InvalidParam(
                "eit: alpha must sit within (0, 1]; length-only arms cannot set it".to_owned(),
            ));
        }
        self.params_for_args(&SmoothingArgs::length(length))
    }

    /// Build kernel parameters from full Pine-mirrored arguments.
    pub fn params_for_args(&self, args: &SmoothingArgs) -> Result<ParamSet, KernelError> {
        if args.length <= 0 {
            return Err(KernelError::InvalidParam(format!(
                "{}: smoothing length must be positive, got {}",
                self.id(),
                args.length
            )));
        }
        match self {
            Self::Alma => {
                if !(0.0..=1.0).contains(&args.n1) {
                    return Err(KernelError::InvalidParam(format!(
                        "alma: offset must be within [0, 1], got {}",
                        args.n1
                    )));
                }
                if !args.param2.is_finite() || args.param2 <= 0.0 {
                    return Err(KernelError::InvalidParam(format!(
                        "alma: sigma must be positive and finite, got {}",
                        args.param2
                    )));
                }
                Ok(ParamSet::new()
                    .with_int("length", args.length)
                    .with_float("offset", args.n1)
                    .with_float("sigma", args.param2))
            }
            Self::Eit => {
                if !args.n1.is_finite() || args.n1 <= 0.0 || args.n1 > 1.0 {
                    return Err(KernelError::InvalidParam(format!(
                        "eit: alpha must sit within (0, 1], got {}",
                        args.n1
                    )));
                }
                Ok(ParamSet::new().with_float("alpha", args.n1))
            }
            // Length-driven arms (Kalman reads its kernel defaults, the recipe
            // validates length for the derivation lag, the rest read length).
            Self::Na
            | Self::Ema
            | Self::Hma
            | Self::Kalman
            | Self::Rma
            | Self::Sma
            | Self::Vwma
            | Self::Wma
            | Self::Zlema => Ok(ParamSet::new().with_int("length", args.length)),
        }
    }

    /// Bars before trustworthy output: the kernel's own warmup, or the recipe
    /// composition (`ema` warmup plus the derivation lag) for `Zlema`.
    pub fn warmup_for_length(&self, length: i64) -> Result<usize, KernelError> {
        if length <= 0 {
            return Err(KernelError::InvalidParam(format!(
                "{}: smoothing length must be positive, got {length}",
                self.id()
            )));
        }
        match self {
            Self::Na => Ok(1),
            Self::Zlema => {
                let period = usize::try_from(length).map_err(|_| {
                    KernelError::InvalidParam(format!("zlema: length {length} overflows usize"))
                })?;
                Ok(period + (period.saturating_sub(1)) / 2)
            }
            Self::Eit => {
                let params = ParamSet::new().with_float("alpha", DEFAULT_N1);
                let registry = crate::indicator::shared_registry()?;
                registry
                    .get("eit")
                    .ok_or_else(|| KernelError::UnknownIndicator("eit".to_owned()))?
                    .warmup(&params)
            }
            _ => {
                let params = self.params_for_length(length)?;
                let kernel = self.kernel_id().expect("kernel arm");
                let registry = crate::indicator::shared_registry()?;
                registry
                    .get(kernel)
                    .ok_or_else(|| KernelError::UnknownIndicator(kernel.to_owned()))?
                    .warmup(&params)
            }
        }
    }

    /// Evaluate over a frame with Pine-mirrored arguments. Pass-through
    /// returns the close leg; kernel arms run their batch; `Zlema` composes
    /// `ema` over the derived series. Holes stay holes throughout.
    pub fn evaluate(
        &self,
        frame: &OwnedOhlcv,
        args: &SmoothingArgs,
    ) -> Result<Vec<Option<f64>>, KernelError> {
        if args.length <= 0 {
            return Err(KernelError::InvalidParam(format!(
                "{}: smoothing length must be positive, got {}",
                self.id(),
                args.length
            )));
        }
        if matches!(self, Self::Na) {
            return Ok(frame.close.iter().map(|v| fin_or_none(*v)).collect());
        }
        if matches!(self, Self::Zlema) {
            let period = usize::try_from(args.length).map_err(|_| {
                KernelError::InvalidParam(format!("zlema: length {} overflows usize", args.length))
            })?;
            let lag = period.saturating_sub(1) / 2;
            let derived: Vec<f64> = frame
                .close
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    if i < lag {
                        f64::NAN
                    } else {
                        2.0 * c - frame.close[i - lag]
                    }
                })
                .collect();
            let registry = crate::indicator::shared_registry()?;
            let ema = registry
                .get("ema")
                .ok_or_else(|| KernelError::UnknownIndicator("ema".to_owned()))?;
            let params = ParamSet::new().with_int("length", args.length);
            return Ok(ema
                .batch(&crate::indicator::Ohlcv::close_only(&derived), &params)?
                .values);
        }
        let params = self.params_for_args(args)?;
        let kernel = self.kernel_id().expect("kernel arm");
        let registry = crate::indicator::shared_registry()?;
        Ok(registry
            .get(kernel)
            .ok_or_else(|| KernelError::UnknownIndicator(kernel.to_owned()))?
            .batch(&frame.views(), &params)?
            .values)
    }
}

impl fluent_wvr::Describable for SmoothingFamily {
    /// One family's settings record: ids plus its parameter specs, so a
    /// picker renders knobs from the catalog without a UI edit per family.
    fn describe(&self) -> serde_json::Value {
        let specs: Vec<serde_json::Value> = self
            .param_specs()
            .iter()
            .map(|spec| {
                serde_json::json!({
                    "name": spec.name(),
                    "min": spec.min(),
                    "max": spec.max(),
                    "default": spec.default(),
                })
            })
            .collect();
        serde_json::json!({
            "id": self.id(),
            "pine": self.pine_name(),
            "label": self.label(),
            "kernel": self.kernel_id(),
            "passthrough": self.is_passthrough(),
            "recipe": self.is_recipe(),
            "params": specs,
        })
    }
}

/// One parameter slot in `factorySmoothing` order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamSpec {
    /// Window length, every arm validates it positive.
    Length,
    /// ALMA peak offset within `[0, 1]`.
    Offset,
    /// ALMA spread divisor, positive and finite.
    Sigma,
    /// EIT smoothing factor within `(0, 1]`.
    Alpha,
    /// Kalman process-noise coupling, non-negative.
    KalmanAlpha,
    /// Kalman error injection, non-negative.
    KalmanBeta,
}

impl ParamSpec {
    /// Slot name matching the Pine `factorySmoothing` signature.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Length => "length",
            Self::Offset => "offset",
            Self::Sigma => "sigma",
            Self::Alpha | Self::KalmanAlpha => "alpha",
            Self::KalmanBeta => "beta",
        }
    }

    /// Inclusive lower bound.
    #[must_use]
    pub fn min(self) -> f64 {
        match self {
            Self::Length => 1.0,
            Self::Offset | Self::KalmanAlpha | Self::KalmanBeta => 0.0,
            Self::Sigma | Self::Alpha => f64::MIN_POSITIVE,
        }
    }

    /// Inclusive upper bound (`f64::MAX` where the reference sets none).
    #[must_use]
    pub fn max(self) -> f64 {
        match self {
            Self::Offset | Self::Alpha => 1.0,
            _ => f64::MAX,
        }
    }

    /// Pine-mirrored default.
    #[must_use]
    pub fn default(self) -> f64 {
        match self {
            Self::Length => 14.0,
            Self::Offset | Self::Alpha => DEFAULT_N1,
            Self::Sigma => DEFAULT_PARAM2,
            Self::KalmanAlpha => crate::filters::KalmanIndicator::DEFAULT_ALPHA,
            Self::KalmanBeta => crate::filters::KalmanIndicator::DEFAULT_BETA,
        }
    }
}

/// Pine-mirrored smoothing arguments: `factorySmoothing(data, smoothing,
/// length, n1 = 0.85, param2 = 6)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmoothingArgs {
    /// Window length (validated positive by every arm, like the Pine gate).
    pub length: i64,
    /// EIT alpha / ALMA offset.
    pub n1: f64,
    /// ALMA sigma.
    pub param2: f64,
}

impl SmoothingArgs {
    /// Bare length with Pine defaults for the rest.
    #[must_use]
    pub fn length(length: i64) -> Self {
        Self {
            length,
            n1: DEFAULT_N1,
            param2: DEFAULT_PARAM2,
        }
    }

    /// Override the `n1` slot (EIT alpha / ALMA offset).
    #[must_use]
    pub fn with_n1(mut self, n1: f64) -> Self {
        self.n1 = n1;
        self
    }

    /// Override the `param2` slot (ALMA sigma).
    #[must_use]
    pub fn with_param2(mut self, param2: f64) -> Self {
        self.param2 = param2;
        self
    }
}

/// Length-only kernel parameters, smoothing-aware: smoothing families resolve
/// through the catalog (ALMA carries its defaults, Kalman its kernel
/// defaults); any other registered kernel takes a bare length. Refuses
/// unknown smoothing names and `Eit`, whose alpha no length implies.
pub fn params_for_kernel(kernel: &str, length: i64) -> Result<ParamSet, KernelError> {
    match SmoothingFamily::parse(kernel) {
        Some(SmoothingFamily::Eit) => Err(KernelError::InvalidParam(
            "eit: alpha must sit within (0, 1]; length-only arms cannot set it".to_owned(),
        )),
        Some(family) => family.params_for_length(length),
        None => {
            if length <= 0 {
                return Err(KernelError::InvalidParam(format!(
                    "{kernel}: kernel length must be positive, got {length}"
                )));
            }
            let registry = crate::indicator::shared_registry()?;
            let indicator = registry.get(kernel).ok_or_else(|| {
                KernelError::InvalidParam(format!("{kernel}: kernel is not registered"))
            })?;
            let spec = indicator.spec();
            let mut params = spec.default_params();
            // Legacy callers only name a length; declared defaults supply
            // every other parameter the kernel actually reads.
            params = params.with_int("length", length);
            Ok(params)
        }
    }
}

/// The whole catalog as one settings record for pickers.
pub struct SmoothingCatalog;

impl fluent_wvr::Describable for SmoothingCatalog {
    /// Every family with its parameter specs, in Pine-dispatch order.
    fn describe(&self) -> serde_json::Value {
        let families: Vec<serde_json::Value> = SmoothingFamily::all()
            .iter()
            .map(fluent_wvr::Describable::describe)
            .collect();
        serde_json::json!({ "families": families })
    }
}

/// One smoothing row for catalog-backed pickers: the catalog id plus the
/// UI-relevant payload, so pickers list the catalog without re-spelling it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmoothingOption {
    /// Canonical catalog id (the config value).
    pub id: String,
    /// Human label (the picker row title).
    pub label: String,
    /// Whether a length-only arm may select it (alpha-driven families
    /// cannot).
    pub takes_length: bool,
    /// Registry kernel the family evaluates through (`ema` for the `zlema`
    /// recipe); `None` for pass-through rows, which run no kernel.
    pub kernel: Option<String>,
}

/// Every smoothing family in Pine-dispatch order: the smoothing picker.
#[must_use]
pub fn smoothing_options() -> Vec<SmoothingOption> {
    SmoothingFamily::all()
        .iter()
        .map(|family| SmoothingOption {
            id: family.id().to_owned(),
            label: family.label().to_owned(),
            takes_length: family.takes_length(),
            kernel: family.kernel_id().map(str::to_owned),
        })
        .collect()
}

/// Ids a length-only arm may hold: exactly what arm validation accepts, so
/// selector schemas and validators read one list.
#[must_use]
pub fn length_smoothing_ids() -> Vec<String> {
    SmoothingFamily::all()
        .iter()
        .filter(|family| family.takes_length())
        .map(|family| family.id().to_owned())
        .collect()
}

#[cfg(test)]
#[path = "../tests/smoothing.rs"]
mod tests;
