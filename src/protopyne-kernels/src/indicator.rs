//! Indicator contract, registry, and execution ordering.
//!
//! An [`Indicator`] declares its [`IndicatorSpec`] (identity, warmup floor,
//! dependencies), evaluates whole windows ([`batch`](Indicator::batch)) and
//! single bars ([`update`](Indicator::update)), and is looked up through an
//! instance [`IndicatorRegistry`] — never a global, so parallel runs stay
//! isolated. Ordering composes `fluent-dag`'s [`DependencyGraph`](fluent_dag::dep_graph::DependencyGraph);
//! no second topological sort lives here.
//!
//! Series legs are plain `f64` slices where IEEE `NaN` means Pine `na`: it
//! propagates through arithmetic and maps back to `None` at the output
//! boundary via [`fin_or_none`]. Kernels read only the legs they need from
//! [`Ohlcv`]; [`BarTick`] is the matching per-bar input for [`update`](Indicator::update).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

use bon::Builder;
use fluent_dag::dep_graph::{DependencyGraph, GraphError};
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
        let conv = |d: rust_decimal::Decimal| d.to_f64().unwrap_or(f64::NAN);
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
        match self.values.get(name).copied().and_then(PineValue::as_float) {
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

/// Incremental evaluation state: recent per-bar ticks, the last emitted
/// value, and child states for composed kernels (MACD's three EMAs, RSI's
/// gain/loss smoothers, …). Single- and multi-output paths share it, so
/// `batch`/`update` agreement covers every emitted series.
#[derive(Debug, Clone, Default)]
pub struct IndicatorState {
    history: VecDeque<BarTick>,
    last: Option<f64>,
    children: Vec<IndicatorState>,
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
    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError>;

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
}

/// Instance registry of indicators keyed by spec id. Deliberately an owned
/// value rather than a process-global, so tests and parallel runs each hold
/// their own registry.
#[derive(Default)]
pub struct IndicatorRegistry {
    indicators: HashMap<IndicatorId, Arc<dyn Indicator>>,
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
        if self.indicators.contains_key(&id) {
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
        let mut ids: Vec<IndicatorId> = self.indicators.keys().cloned().collect();
        ids.sort();
        ids
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
    registry.register(Arc::new(crate::oscillators::MacdIndicator::new()))?;
    registry.register(Arc::new(crate::oscillators::StochIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::TrIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::AtrIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::BbIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::KcIndicator::new()))?;
    registry.register(Arc::new(crate::volatility::NatrIndicator::new()))?;
    registry.register(Arc::new(crate::ta_overlap::BbandsIndicator::new()))?;
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
    registry.register(Arc::new(crate::ta_stat::StddevIndicator::new()))?;
    registry.register(Arc::new(crate::ta_stat::VarIndicator::new()))?;
    registry.register(Arc::new(crate::ta_stat::CorrelIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::AdIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::AdoscIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::ObvIndicator::new()))?;
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
    for id in requested {
        if registry.get(id).is_none() {
            return Err(GraphError::NodeNotFound(id.clone()));
        }
    }
    let mut graph = DependencyGraph::new();
    for (id, indicator) in registry.iter() {
        let spec = indicator.spec();
        for dep in &spec.depends {
            if registry.get(dep).is_none() {
                return Err(GraphError::NodeNotFound(dep.clone()));
            }
        }
        graph.register(id, &spec.depends, &spec.provides())?;
    }
    graph.topo_sort_from(requested)
}

#[cfg(test)]
#[path = "../tests/indicator.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/golden.rs"]
mod golden_tests;
