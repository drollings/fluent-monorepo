//! Declarative, multi-line kernel signal.
//!
//! A [`KernelSignal`] names one or more **lines** and one [`DirectionRule`].
//! A line is either a registered kernel output (primary or a named secondary
//! such as MACD's `signal` or stochastics' `d`) over a [`PriceSource`], or a
//! raw price column. The rule reads lines by index, so the ordinary signal
//! families are all expressible without a second rule engine:
//!
//! - fast MA crossing slow MA — `CrossesLine { fast, slow }`;
//! - MACD line crossing its signal line — a `Named` secondary on one line;
//! - oscillator crossing a level — `CrossesLevel` (event) or `Above`/`Below`
//!   (state);
//! - momentum zero-cross / band — `Sign` / `Band`.
//!
//! Every rule refuses rather than substitutes: an unknown indicator, an
//! unknown output name, insufficient warmup, and a non-finite latest value
//! are all [`StrategyError`]s. Cross rules emit only on the bar the crossing
//! happens; a line without two finite samples has no crossing.
//!
//! Cross detection compares the last two finite samples of a stateless
//! `batch`, so evaluation recomputes over the view window. A bound instance
//! reads the same window; any incremental form must agree with this reference
//! crossing for crossing.

use std::sync::Arc;

use protopyne_kernels::util::{crossed, crossed_between, Crossing};
use protopyne_kernels::{
    builtin_registry, fin_or_none, IndicatorId, IndicatorRegistry, OwnedOhlcv, ParamSet,
    PriceSource,
};

use crate::elements::{Signal, SignalConfig, SignalInstance, SignalOutput};
use crate::error::StrategyError;
use crate::proposal::Direction;
use crate::view::MarketView;

/// Which output of a multi-output kernel a line reads.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Which output of a multi-output kernel a line reads.
pub enum OutputSelector {
    /// The kernel's primary series.
    Primary,
    /// A named secondary series (`signal`, `d`, `upper`, `down`, …).
    Named(String),
}

/// One line of a signal: a registered kernel output or a raw price column.
#[derive(Clone)]
/// One line of a signal: a registered kernel output or a raw price column.
pub enum SeriesRef {
    /// A kernel output over `source`.
    Indicator {
        /// Registry key.
        id: IndicatorId,
        /// Indicator parameters.
        params: ParamSet,
        /// Primary or a named secondary output.
        output: OutputSelector,
        /// Which OHLC-derived column the kernel reads.
        source: PriceSource,
    },
    /// A raw price column, e.g. close against its own moving average.
    Price {
        /// Which OHLC-derived column to read.
        source: PriceSource,
    },
}

impl SeriesRef {
    /// A kernel line reading `source`, primary output.
    #[must_use]
    pub fn indicator(id: impl Into<String>, params: ParamSet, source: PriceSource) -> Self {
        Self::Indicator {
            id: id.into(),
            params,
            output: OutputSelector::Primary,
            source,
        }
    }

    /// A kernel line reading a named secondary output.
    #[must_use]
    pub fn named(
        id: impl Into<String>,
        output: impl Into<String>,
        params: ParamSet,
        source: PriceSource,
    ) -> Self {
        Self::Indicator {
            id: id.into(),
            params,
            output: OutputSelector::Named(output.into()),
            source,
        }
    }

    /// A raw price line.
    #[must_use]
    pub fn price(source: PriceSource) -> Self {
        Self::Price { source }
    }
}

/// Which side of a threshold is `Long`: breakout or mean-reversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which side of a threshold is `Long`: breakout or mean-reversion.
pub enum Polarity {
    /// Above/positive ⇒ `Long` (breakout, trend).
    Momentum,
    /// Above/positive ⇒ `Short` (oscillator reversion).
    Reversion,
}

/// Which crossing a cross rule accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which crossing a cross rule accepts.
pub enum CrossDir {
    /// Only an upward crossing.
    Above,
    /// Only a downward crossing.
    Below,
    /// Either direction.
    Either,
}

/// How a signal turns its lines into a direction.
#[derive(Debug, Clone, Copy, PartialEq)]
/// How a signal turns its lines into a direction.
pub enum DirectionRule {
    /// Zero-cross with a normalizing scale: `line / scale` sets conviction.
    Sign {
        /// Line index.
        line: usize,
        /// Positive normalization; must be finite and `> 0`.
        scale: f64,
        /// Threshold polarity.
        polarity: Polarity,
    },
    /// Oscillator band: outside `[lower, upper]` emits, inside refuses.
    Band {
        /// Line index.
        line: usize,
        /// Lower bound (inclusive).
        lower: f64,
        /// Upper bound (inclusive).
        upper: f64,
        /// Threshold polarity.
        polarity: Polarity,
    },
    /// Emits while the line is strictly above `level`.
    Above {
        /// Line index.
        line: usize,
        /// Threshold.
        level: f64,
        /// Threshold polarity.
        polarity: Polarity,
    },
    /// Emits while the line is strictly below `level`.
    Below {
        /// Line index.
        line: usize,
        /// Threshold.
        level: f64,
        /// Threshold polarity.
        polarity: Polarity,
    },
    /// Emits only on the bar `line` crosses `level`.
    CrossesLevel {
        /// Line index.
        line: usize,
        /// Threshold.
        level: f64,
        /// Which crossing fires.
        direction: CrossDir,
        /// Threshold polarity.
        polarity: Polarity,
    },
    /// Emits only on the bar `fast` crosses `slow`.
    CrossesLine {
        /// Fast line index.
        fast: usize,
        /// Slow line index.
        slow: usize,
        /// Which crossing fires.
        direction: CrossDir,
        /// Threshold polarity.
        polarity: Polarity,
    },
}

/// The parts of a kernel signal that do not change per run. Shared by the
/// stateless signal and every bound instance, so there is one evaluator.
#[derive(Clone)]
/// The parts of a kernel signal that do not change per run. Shared by the
/// stateless signal and every bound instance, so there is one evaluator.
struct KernelCore {
    lines: Vec<SeriesRef>,
    rule: DirectionRule,
    activity_score: f64,
    registry: Arc<IndicatorRegistry>,
}

/// A multi-line kernel signal: lines plus one rule.
pub struct KernelSignal {
    name: String,
    core: Arc<KernelCore>,
}

/// One run's kernel signal: the shared lines and rule, plus the warmup the
/// registry declares. Evaluation reads the view window, so the instance keeps
/// no cursor of its own and cannot drift from the stateless reference.
pub struct KernelSignalInstance {
    core: Arc<KernelCore>,
    warmup: usize,
}

impl std::fmt::Debug for KernelSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelSignal")
            .field("name", &self.name)
            .field("lines", &self.core.lines.len())
            .field("rule", &self.core.rule)
            .field("activity_score", &self.core.activity_score)
            .finish_non_exhaustive()
    }
}

impl KernelSignal {
    /// Build a signal over `lines`. Refuses at construction: no lines, an
    /// unknown indicator id, an out-of-range rule line, a non-positive `Sign`
    /// scale, or an inverted `Band`.
    pub fn new(lines: Vec<SeriesRef>, rule: DirectionRule) -> Result<Self, StrategyError> {
        Self::with_registry(
            lines,
            rule,
            Arc::new(builtin_registry().map_err(|e| kernel_error(&e))?),
        )
    }

    /// As [`new`](Self::new) but against a caller registry (tests, custom
    /// kernels).
    pub fn with_registry(
        lines: Vec<SeriesRef>,
        rule: DirectionRule,
        registry: Arc<IndicatorRegistry>,
    ) -> Result<Self, StrategyError> {
        if lines.is_empty() {
            return Err(StrategyError::InvalidElement(
                "signal needs at least one line".to_owned(),
            ));
        }
        for line in &lines {
            if let SeriesRef::Indicator { id, .. } = line {
                if registry.get(id).is_none() {
                    return Err(StrategyError::InvalidElement(format!(
                        "unknown indicator id {id:?}"
                    )));
                }
            }
        }
        validate_rule(&rule, lines.len())?;
        Ok(Self {
            name: "kernel".to_owned(),
            core: Arc::new(KernelCore {
                lines,
                rule,
                activity_score: 1.0,
                registry,
            }),
        })
    }

    /// Override the attribution name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the options-activity score carried on every output (no chain feed
    /// exists here, so the default is `1.0`: unfiltered).
    #[must_use]
    pub fn with_activity(mut self, activity_score: f64) -> Self {
        Arc::make_mut(&mut self.core).activity_score = activity_score;
        self
    }

    /// The rule this signal evaluates.
    #[must_use]
    pub fn rule(&self) -> &DirectionRule {
        &self.core.rule
    }
}

impl KernelCore {
    fn series_of(
        &self,
        line: &SeriesRef,
        view: &MarketView<'_>,
    ) -> Result<Vec<Option<f64>>, StrategyError> {
        let mut owned = OwnedOhlcv::from_bars(view.data.bars);
        match line {
            SeriesRef::Price { source } => Ok(source
                .column_of(&owned)
                .into_iter()
                .map(fin_or_none)
                .collect()),
            SeriesRef::Indicator {
                id,
                params,
                output,
                source,
            } => {
                if *source != PriceSource::Close {
                    owned.close = source.column_of(&owned);
                }
                let indicator = self.registry.get(id).ok_or_else(|| {
                    StrategyError::InvalidElement(format!("unknown indicator id {id:?}"))
                })?;
                let result = indicator
                    .batch(&owned.views(), params)
                    .map_err(|e| StrategyError::InvalidElement(format!("indicator {id:?}: {e}")))?;
                match output {
                    OutputSelector::Primary => Ok(result.values),
                    OutputSelector::Named(name) => result
                        .secondary
                        .iter()
                        .find(|series| &series.name == name)
                        .map(|series| series.values.clone())
                        .ok_or_else(|| {
                            StrategyError::InvalidElement(format!(
                                "indicator {id:?} has no output {name:?}"
                            ))
                        }),
                }
            }
        }
    }

    fn apply(&self, series: &[Vec<Option<f64>>]) -> Result<SignalOutput, StrategyError> {
        apply_rule(self.rule, series, self.activity_score)
    }
}

/// Apply one direction rule to already-evaluated lines.
///
/// Shared by the kernel signal and the Pine signal so the rule vocabulary has
/// exactly one implementation: every rule refuses rather than substitutes.
pub(crate) fn apply_rule(
    rule: DirectionRule,
    series: &[Vec<Option<f64>>],
    activity_score: f64,
) -> Result<SignalOutput, StrategyError> {
    let (direction, conviction) = match rule {
        DirectionRule::Sign {
            line,
            scale,
            polarity,
        } => {
            let value = latest(series, line)?;
            let normalized = (value.abs() / scale).clamp(0.0, 1.0);
            (direction_for(value >= 0.0, polarity), normalized)
        }
        DirectionRule::Band {
            line,
            lower,
            upper,
            polarity,
        } => {
            let value = latest(series, line)?;
            let half_band = (upper - lower) / 2.0;
            if value >= upper {
                ((direction_for(true, polarity)), (value - upper) / half_band)
            } else if value <= lower {
                (
                    (direction_for(false, polarity)),
                    (lower - value) / half_band,
                )
            } else {
                return Err(StrategyError::InvalidElement(format!(
                    "line {line} value {value} is inside the band [{lower}, {upper}]: no signal"
                )));
            }
        }
        DirectionRule::Above {
            line,
            level,
            polarity,
        } => {
            let value = latest(series, line)?;
            if value > level {
                (direction_for(true, polarity), 1.0)
            } else {
                return Err(StrategyError::InvalidElement(format!(
                    "line {line} value {value} is not above {level}: no signal"
                )));
            }
        }
        DirectionRule::Below {
            line,
            level,
            polarity,
        } => {
            let value = latest(series, line)?;
            if value < level {
                (direction_for(false, polarity), 1.0)
            } else {
                return Err(StrategyError::InvalidElement(format!(
                    "line {line} value {value} is not below {level}: no signal"
                )));
            }
        }
        DirectionRule::CrossesLevel {
            line,
            level,
            direction,
            polarity,
        } => {
            let (previous, current) = last_two(series, line)?;
            let crossed = crossed(Some(previous), Some(current), level)
                .map(|crossing| crossing == Crossing::Up);
            let (high, crossed) = match (crossed, direction) {
                (Some(high), CrossDir::Above) if high => (high, true),
                (Some(high), CrossDir::Below) if !high => (high, true),
                (Some(high), CrossDir::Either) => (high, true),
                _ => (false, false),
            };
            if !crossed {
                return Err(StrategyError::InvalidElement(format!(
                    "line {line} did not cross {level} in the requested direction"
                )));
            }
            (direction_for(high, polarity), 1.0)
        }
        DirectionRule::CrossesLine {
            fast,
            slow,
            direction,
            polarity,
        } => {
            let (previous_fast, current_fast) = last_two(series, fast)?;
            let (previous_slow, current_slow) = last_two(series, slow)?;
            let crossed = crossed_between(
                Some(previous_fast),
                Some(current_fast),
                Some(previous_slow),
                Some(current_slow),
            )
            .map(|crossing| crossing == Crossing::Up);
            let (high, crossed) = match (crossed, direction) {
                (Some(high), CrossDir::Above) if high => (high, true),
                (Some(high), CrossDir::Below) if !high => (high, true),
                (Some(high), CrossDir::Either) => (high, true),
                _ => (false, false),
            };
            if !crossed {
                return Err(StrategyError::InvalidElement(format!(
                    "line {fast} did not cross line {slow} in the requested direction"
                )));
            }
            (direction_for(high, polarity), 1.0)
        }
    };
    if !conviction.is_finite() {
        return Err(StrategyError::InvalidElement(
            "rule produced a non-finite conviction".to_owned(),
        ));
    }
    Ok(SignalOutput {
        direction,
        conviction: conviction.clamp(0.0, 1.0),
        activity_score,
        // Measured in `evaluate`, which owns the view (and the registry).
        volatility: None,
        // The kernel signal estimates no edge or odds: a Kelly-style sizer
        // reads absence as zero, never a default.
        edge: None,
        odds: None,
    })
}

/// Bars the kernel realised volatility measures over: twenty returns
/// capture a regime without lagging a break by a quarter. Intentionally
/// separate from the lite estimator's `LITE_VOLATILITY_LENGTH` (kernels
/// `lite`, manual mean/var over closes): different math over a same-sized
/// window — the names now say which is which (M4.2). Do not unify the
/// constants.
pub const KERNEL_VOLATILITY_WINDOW: usize = 20;

impl KernelCore {
    fn evaluate(&self, view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        if view.data.bars.is_empty() {
            return Err(StrategyError::InvalidElement(
                "kernel signal needs at least one bar".to_owned(),
            ));
        }
        let series = self
            .lines
            .iter()
            .map(|line| self.series_of(line, view))
            .collect::<Result<Vec<_>, _>>()?;
        let volatility = self.realised_volatility(view);
        Ok(self.apply(&series)?.with_volatility(volatility))
    }

    /// Realised per-bar volatility of simple returns at decision time.
    ///
    /// Read through the registered `stddev` kernel over a returns series the
    /// signal builds from the view's closes, so the signal adds no second
    /// statistics path: the kernel's population deviation is the one number a
    /// sizer sees. `None` is *absence* — too few bars, a non-positive close, or
    /// a kernel refusal — never a zero, because a zero σ would size **larger**,
    /// the opposite of prudent.
    fn realised_volatility(&self, view: &MarketView<'_>) -> Option<f64> {
        let bars = view.data.bars;
        if bars.len() < KERNEL_VOLATILITY_WINDOW + 1 {
            return None;
        }
        use rust_decimal::prelude::ToPrimitive as _;
        let mut returns = Vec::with_capacity(bars.len() - 1);
        for pair in bars.windows(2) {
            let previous = pair[0].c.to_f64()?;
            let current = pair[1].c.to_f64()?;
            if !(previous.is_finite() && previous > 0.0 && current.is_finite()) {
                return None;
            }
            returns.push((current - previous) / previous);
        }
        let data = OwnedOhlcv {
            open: Vec::new(),
            high: Vec::new(),
            low: Vec::new(),
            close: returns,
            volume: Vec::new(),
        };
        let params = ParamSet::new()
            .with_int("length", KERNEL_VOLATILITY_WINDOW as i64)
            .with_float("nbdev", 1.0);
        let indicator = self.registry.get("stddev")?;
        let result = indicator.batch(&data.views(), &params).ok()?;
        result
            .values
            .last()
            .copied()
            .flatten()
            .filter(|sigma| sigma.is_finite() && *sigma >= 0.0)
    }

    /// Largest declared warmup across the indicator lines; a registered
    /// kernel owns its own bound, so the signal never guesses.
    fn warmup(&self) -> Result<usize, StrategyError> {
        let mut warmup = 0;
        for line in &self.lines {
            if let SeriesRef::Indicator { id, params, .. } = line {
                let indicator = self.registry.get(id).ok_or_else(|| {
                    StrategyError::InvalidElement(format!("unknown indicator id {id:?}"))
                })?;
                let declared = indicator
                    .warmup(params)
                    .map_err(|e| StrategyError::InvalidElement(format!("indicator {id:?}: {e}")))?;
                warmup = warmup.max(declared);
            }
        }
        Ok(warmup)
    }
}

impl Signal for KernelSignal {
    fn name(&self) -> &str {
        &self.name
    }

    fn bind(&self, _cfg: &SignalConfig) -> Result<Box<dyn SignalInstance>, StrategyError> {
        Ok(Box::new(KernelSignalInstance {
            core: Arc::clone(&self.core),
            warmup: self.core.warmup()?,
        }))
    }

    fn evaluate(&self, view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        self.core.evaluate(view)
    }
}

impl SignalInstance for KernelSignalInstance {
    fn evaluate(&mut self, view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        self.core.evaluate(view)
    }

    fn warmup(&self) -> usize {
        self.warmup
    }
}

fn kernel_error(error: &protopyne_kernels::KernelError) -> StrategyError {
    StrategyError::InvalidElement(format!("kernel registry: {error}"))
}

fn direction_for(high: bool, polarity: Polarity) -> Direction {
    let long = match polarity {
        Polarity::Momentum => high,
        Polarity::Reversion => !high,
    };
    if long {
        Direction::Long
    } else {
        Direction::Short
    }
}

fn latest(series: &[Vec<Option<f64>>], line: usize) -> Result<f64, StrategyError> {
    let values = series
        .get(line)
        .ok_or_else(|| StrategyError::InvalidElement(format!("rule names missing line {line}")))?;
    values.last().copied().flatten().ok_or_else(|| {
        StrategyError::InvalidElement(format!("line {line} has no finite latest value (warmup)"))
    })
}

fn last_two(series: &[Vec<Option<f64>>], line: usize) -> Result<(f64, f64), StrategyError> {
    let values = series
        .get(line)
        .ok_or_else(|| StrategyError::InvalidElement(format!("rule names missing line {line}")))?;
    let current = values.last().copied().flatten().ok_or_else(|| {
        StrategyError::InvalidElement(format!("line {line} has no finite latest value (warmup)"))
    })?;
    let previous = values[..values.len().saturating_sub(1)]
        .iter()
        .rev()
        .find_map(|value| *value)
        .ok_or_else(|| {
            StrategyError::InvalidElement(format!(
                "line {line} has fewer than two finite samples (warmup)"
            ))
        })?;
    Ok((previous, current))
}

pub(crate) fn validate_rule(rule: &DirectionRule, lines: usize) -> Result<(), StrategyError> {
    let check_line = |line: usize| {
        if line < lines {
            Ok(())
        } else {
            Err(StrategyError::InvalidElement(format!(
                "rule names line {line}, but the signal has {lines}"
            )))
        }
    };
    match rule {
        DirectionRule::Sign { line, scale, .. } => {
            check_line(*line)?;
            if !scale.is_finite() || *scale <= 0.0 {
                return Err(StrategyError::InvalidElement(format!(
                    "sign scale must be finite and positive, got {scale}"
                )));
            }
        }
        DirectionRule::Band {
            line, lower, upper, ..
        } => {
            check_line(*line)?;
            if !lower.is_finite() || !upper.is_finite() || lower >= upper {
                return Err(StrategyError::InvalidElement(format!(
                    "band bounds must be finite with lower < upper, got [{lower}, {upper}]"
                )));
            }
        }
        DirectionRule::Above { line, level, .. }
        | DirectionRule::Below { line, level, .. }
        | DirectionRule::CrossesLevel { line, level, .. } => {
            check_line(*line)?;
            if !level.is_finite() {
                return Err(StrategyError::InvalidElement(format!(
                    "threshold {level} must be finite"
                )));
            }
        }
        DirectionRule::CrossesLine { fast, slow, .. } => {
            check_line(*fast)?;
            check_line(*slow)?;
            if fast == slow {
                return Err(StrategyError::InvalidElement(
                    "crossing needs two distinct lines".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/signals_kernel.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/signal_calibration.rs"]
mod calibration_tests;