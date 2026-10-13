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
    /// change is a deliberate cache-invalidation event.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut names: Vec<&String> = self.values.keys().collect();
        names.sort();
        let mut bytes = Vec::new();
        for name in names {
            let Some(value) = self.values.get(name) else {
                continue;
            };
            bytes.extend_from_slice(&(name.len() as u64).to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
            encode_param_value(&mut bytes, value);
        }
        common_core::hash::fnv1a64(&bytes)
    }
}

/// Append one canonical `(tag, payload)` frame for a parameter value.
/// Length-prefixes keep frames unambiguous; sorting at every map level
/// keeps the encoding independent of hash-map iteration order.
fn encode_param_value(bytes: &mut Vec<u8>, value: &PineValue) {
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
    registry.register(Arc::new(crate::oscillators::CciIndicator::new()))?;
    registry.register(Arc::new(crate::oscillators::CmoIndicator::new()))?;
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
    registry.register(Arc::new(crate::ta_volume::MfiIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::VwapIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::AdoscIndicator::new()))?;
    registry.register(Arc::new(crate::ta_volume::ObvIndicator::new()))?;
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
#[path = "../tests/param_fingerprint.rs"]
mod param_fingerprint_tests;

#[cfg(test)]
#[path = "../tests/golden.rs"]
mod golden_tests;

#[cfg(test)]
#[path = "../tests/tv_spot.rs"]
mod tv_spot_tests;
