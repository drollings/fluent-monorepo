//! Trading chart topology: the standard nine-pane DAG plus kernel bridges.
//!
//! The generic [`PanePlan`](fluent_egui::PanePlan) owns ordering, readiness,
//! cancellation scope, and checkpoints; this module owns the domain shape
//! poured into it — which panes exist and how the confirmed-bar dataflow
//! wires them (smoothing pair, then oscillators, then the ribbon band, then
//! health). Kernel waves keep the kernel registry's own ordering through
//! [`kernel_order`]; cache keys bind pane names to kernel parameter sets
//! through [`cache_key_for`].

use common_core::hash::fingerprint_framed;
use fluent_egui::{EguiError, PanePlan, ReplayController, cache_key};
use protopyne_chart_model::{ChartRow, DisplayOnly};
use protopyne_kernels::indicator::{IndicatorId, IndicatorRegistry, ParamSet, execution_order};
use protopyne_kernels::{OwnedOhlcv, PriceSource, builtin_registry, nan_hole};

/// Price pane: converted rows, no upstream pane.
pub const PRICE_NODE: &str = "price";
/// Volume strip: converted volumes, no upstream pane.
pub const VOLUME_NODE: &str = "volume";
/// Adaptive smoothing of the price pane.
pub const ADAPTIVE_NODE: &str = "eit:adaptive";
/// Signal smoothing of the adaptive leg.
pub const SIGNAL_NODE: &str = "eit:signal";
/// Fast normalized oscillator.
pub const OSC_FAST_NODE: &str = "osc:fast";
/// Slow normalized oscillator.
pub const OSC_SLOW_NODE: &str = "osc:slow";
/// Filled band between the smoothing pair.
pub const RIBBON_NODE: &str = "ribbon";
/// Ribbon-health oscillator off the band.
pub const HEALTH_RIBBON_NODE: &str = "health:ribbon";
/// MA-health oscillator off the price pane.
pub const HEALTH_MA_NODE: &str = "health:ma";

/// Carry a graph refusal across the crate boundary without a second error type.
fn describe_graph(error: &fluent_dag::dep_graph::GraphError) -> EguiError {
    EguiError::Dependency { reason: error.to_string() }
}

/// Build the standard pane topology: price/volume roots, the smoothing pair,
/// both oscillator slots, the ribbon band, and both health slots, with edges
/// following the confirmed-bar dataflow.
pub fn standard_plan() -> Result<PanePlan, EguiError> {
    let mut plan = PanePlan::new();
    plan.register_node(PRICE_NODE, &[], 0)?;
    plan.register_node(VOLUME_NODE, &[], 0)?;
    plan.register_node(ADAPTIVE_NODE, &[PRICE_NODE], 0)?;
    plan.register_node(SIGNAL_NODE, &[ADAPTIVE_NODE], 0)?;
    plan.register_node(OSC_FAST_NODE, &[ADAPTIVE_NODE], 0)?;
    plan.register_node(OSC_SLOW_NODE, &[ADAPTIVE_NODE], 0)?;
    plan.register_node(RIBBON_NODE, &[ADAPTIVE_NODE, SIGNAL_NODE], 0)?;
    plan.register_node(HEALTH_RIBBON_NODE, &[RIBBON_NODE], 0)?;
    plan.register_node(HEALTH_MA_NODE, &[PRICE_NODE], 0)?;
    Ok(plan)
}

/// Build a replay controller preloaded with the standard topology.
pub fn standard_replay() -> Result<ReplayController, EguiError> {
    Ok(ReplayController::with_plan(standard_plan()?))
}

/// Order kernel waves through the kernel registry's own ordering, adapting
/// the graph error into the paint edge. Pane waves use
/// [`PanePlan::order`](fluent_egui::PanePlan::order); kernel waves use this.
pub fn kernel_order(
    registry: &IndicatorRegistry,
    requested: &[IndicatorId],
) -> Result<Vec<IndicatorId>, EguiError> {
    execution_order(registry, requested).map_err(|e| describe_graph(&e))
}

/// Cache key directly from a pane's parameter set.
#[must_use]
pub fn cache_key_for(node: &str, params: &ParamSet) -> u64 {
    cache_key(node, params.fingerprint())
}

/// Carry a kernel refusal across the crate boundary without a second error type.
pub(crate) fn describe_kernel(error: &protopyne_kernels::KernelError) -> EguiError {
    EguiError::Dependency { reason: error.to_string() }
}

/// One indicator input: raw bars or a published series.
///
/// `Bars` reads an OHLC-derived column (the M1 registry); `Series` names a
/// published node (`fast_mas:first`, `kernel:sma`, ...). Spellings are
/// `close`-style names for bars and `series:<node>` for series; anything
/// else refuses. A bare node without the prefix refuses — node ids must be
/// explicit, never guessed from a bare word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeriesSource {
    /// An OHLC-derived column over raw bars.
    Bars(PriceSource),
    /// A published series by node id.
    Series(String),
}

impl Default for SeriesSource {
    fn default() -> Self {
        Self::Bars(PriceSource::Close)
    }
}

impl SeriesSource {
    /// Parse a source spelling: a price-source name or `series:<node>`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(source) = PriceSource::parse(text) {
            return Some(Self::Bars(source));
        }
        let node = text.strip_prefix("series:")?;
        if node.is_empty() || node.trim().is_empty() {
            return None;
        }
        Some(Self::Series(node.to_owned()))
    }

    /// The published node this source reads, if any.
    #[must_use]
    pub fn series_node(&self) -> Option<&str> {
        match self {
            Self::Bars(_) => None,
            Self::Series(node) => Some(node),
        }
    }
}

impl std::fmt::Display for SeriesSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bars(source) => write!(f, "{}", source.name()),
            Self::Series(node) => write!(f, "series:{node}"),
        }
    }
}

/// Published indicator outputs by stable node id: the series catalog a
/// source picker lists. A producer's warmup is its leading-hole count —
/// holes are producer self-knowledge, so composition reads them instead of
/// asking each producer for a number.
#[derive(Debug, Clone, Default)]
pub struct SeriesCatalog {
    series: std::collections::HashMap<String, PublishedSeries>,
}

/// One published series: its cache key (carrying the producer fingerprint)
/// and its per-bar values.
#[derive(Debug, Clone)]
struct PublishedSeries {
    key: u64,
    values: Vec<Option<f64>>,
}

impl SeriesCatalog {
    /// An empty catalog: no producer has published yet.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Publish (or replace) one node's series under its cache key.
    pub fn publish(&mut self, node: &str, key: u64, values: Vec<Option<f64>>) {
        self.series.insert(node.to_owned(), PublishedSeries { key, values });
    }

    /// Drop every published series (a removal or a fresh recompute).
    pub fn clear(&mut self) {
        self.series.clear();
    }

    /// The values published under `node`, if any.
    #[must_use]
    pub fn resolve(&self, node: &str) -> Option<&[Option<f64>]> {
        self.series.get(node).map(|series| series.values.as_slice())
    }

    /// The cache key the published series carries, if any. A consumer folds
    /// this into its own key, so a producer edit invalidates downstream.
    #[must_use]
    pub fn series_key(&self, node: &str) -> Option<u64> {
        self.series.get(node).map(|series| series.key)
    }

    /// Leading holes of the published series: the producer's warmup in
    /// bars. `None` when the node never published.
    #[must_use]
    pub fn leading_holes(&self, node: &str) -> Option<usize> {
        self.resolve(node).map(|values| values.iter().take_while(|value| value.is_none()).count())
    }
}

/// Single-column frame over a published series: every OHLC leg carries the
/// series (holes as `NaN`, the kernel na-policy marker), volume stays real.
/// Refuses a length mismatch — a series that does not align with the rows
/// is investigated, never zipped.
pub fn series_frame(rows: &[ChartRow], values: &[Option<f64>]) -> Result<OwnedOhlcv, EguiError> {
    if values.len() != rows.len() {
        return Err(EguiError::InvalidParam {
            what: "series".to_owned(),
            reason: format!("series length {} does not match {} rows", values.len(), rows.len()),
        });
    }
    let column: Vec<f64> = values.iter().map(|value| nan_hole(*value)).collect();
    let volume: Vec<f64> = rows.iter().map(|row| row.volume as f64).collect();
    Ok(OwnedOhlcv {
        open: column.clone(),
        high: column.clone(),
        low: column.clone(),
        close: column,
        volume,
    })
}

/// The default input spelling: raw close bars.
#[must_use]
pub fn default_source_spelling() -> String {
    SeriesSource::default().to_string()
}

/// The input column for a source: bar columns read the rows (producer
/// warmup zero — kernels emit their own holes), series columns resolve from
/// the catalog or refuse naming the node.
pub fn resolve_input_column(
    source: &SeriesSource,
    rows: &[ChartRow],
    series: &SeriesCatalog,
) -> Result<(Vec<Option<f64>>, usize), EguiError> {
    match source {
        SeriesSource::Bars(price) => {
            let frame = kernel_preview_frame_with_source(rows, *price);
            Ok((frame.close.iter().map(|value| Some(*value)).collect(), 0))
        }
        SeriesSource::Series(node) => {
            let values = series.resolve(node).ok_or_else(|| EguiError::InvalidParam {
                what: "series".to_owned(),
                reason: format!("unknown input series {node:?}"),
            })?;
            let warmup = series.leading_holes(node).unwrap_or(0);
            Ok((values.to_vec(), warmup))
        }
    }
}

/// Cache key for a series consumed from another node: the node's own key
/// framing, but the fingerprint folds the consumer config together with the
/// producer's series key — so a producer edit invalidates every downstream
/// key transitively, while an untouched pair stays stable.
#[must_use]
pub fn consumer_cache_key(node: &str, config_fingerprint: u64, producer_key: u64) -> u64 {
    cache_key(
        node,
        fingerprint_framed(&[
            ("config", &config_fingerprint.to_le_bytes()),
            ("producer", &producer_key.to_le_bytes()),
        ]),
    )
}

/// Fenced full-OHLC preview frame for kernel batch evaluation.
///
/// Display provenance is fenced: every leg enters as [`DisplayOnly`] and is
/// revealed exactly here — the auditable escape for a display-descended
/// preview frame. This is the single construction every kernel-reading
/// terminal module shares, so the fence is named once and no module forks a
/// second display-to-kernel bridge.
pub(crate) fn kernel_preview_frame(rows: &[ChartRow]) -> OwnedOhlcv {
    kernel_preview_frame_with_source(rows, PriceSource::Close)
}

/// Full-OHLC preview frame with an explicit default source.
///
/// The frame carries every leg, so output is byte-identical for all sources
/// today; the argument names the default single-leg read downstream
/// single-column builders resolve, keeping one spelling for the default.
pub fn kernel_preview_frame_with_source(rows: &[ChartRow], _source: PriceSource) -> OwnedOhlcv {
    let open: Vec<DisplayOnly<f64>> = rows.iter().map(|row| DisplayOnly::new(row.open)).collect();
    let high: Vec<DisplayOnly<f64>> = rows.iter().map(|row| DisplayOnly::new(row.high)).collect();
    let low: Vec<DisplayOnly<f64>> = rows.iter().map(|row| DisplayOnly::new(row.low)).collect();
    let close: Vec<DisplayOnly<f64>> = rows.iter().map(|row| DisplayOnly::new(row.close)).collect();
    let volume: Vec<DisplayOnly<f64>> =
        rows.iter().map(|row| DisplayOnly::new(row.volume as f64)).collect();
    OwnedOhlcv {
        open: open.into_iter().map(DisplayOnly::reveal).collect(),
        high: high.into_iter().map(DisplayOnly::reveal).collect(),
        low: low.into_iter().map(DisplayOnly::reveal).collect(),
        close: close.into_iter().map(DisplayOnly::reveal).collect(),
        volume: volume.into_iter().map(DisplayOnly::reveal).collect(),
    }
}

/// Single-column frame for single-leg kernels: every OHLC leg carries the
/// selected source column, volume stays real. `Close` output is byte-identical
/// to the former close-only fill.
pub fn source_frame(rows: &[ChartRow], source: PriceSource) -> OwnedOhlcv {
    let full = kernel_preview_frame_with_source(rows, source);
    let column = source.column_of(&full);
    OwnedOhlcv {
        open: column.clone(),
        high: column.clone(),
        low: column.clone(),
        close: column,
        volume: full.volume,
    }
}

/// Run one kernel batch over a frame, or refuse a missing kernel.
pub(crate) fn kernel_series(
    kernel_id: &str,
    params: &ParamSet,
    frame: &OwnedOhlcv,
) -> Result<Vec<Option<f64>>, EguiError> {
    let registry = builtin_registry().map_err(|e| describe_kernel(&e))?;
    let kernel = registry.get(kernel_id).ok_or_else(|| EguiError::Dependency {
        reason: format!("kernel not registered: {kernel_id}"),
    })?;
    kernel.batch(&frame.views(), params).map(|result| result.values).map_err(|e| describe_kernel(&e))
}

/// A kernel's declared warmup for parameters: bars needed before the first
/// trustworthy output. The evaluation window below sizes itself from this,
/// never a magic number; unknown kernels refuse instead of warming up by
/// zero (which would fabricate a value where a hole belongs).
pub fn kernel_warmup(kernel_id: &str, params: &ParamSet) -> Result<usize, EguiError> {
    let registry = builtin_registry().map_err(|e| describe_kernel(&e))?;
    let kernel = registry.get(kernel_id).ok_or_else(|| EguiError::Dependency {
        reason: format!("kernel not registered: {kernel_id}"),
    })?;
    kernel.warmup(params).map_err(|e| describe_kernel(&e))
}

/// Bar slice for visible-window-plus-warmup assessment: the visible bars
/// plus the warmup bars before them, clamped to history.
///
/// Returns `(eval_start, eval_len)` with `eval_start + eval_len` clamped to
/// `total_bars`, so the caller slices `bars[eval_start..eval_start + eval_len]`
/// without a second bounds check. A visible window past the data (or no
/// bars at all) evaluates nothing; a shortfall evaluates what exists, and
/// leading holes stay holes downstream — the kernels emit `None` where
/// history runs short, never a fabricated value.
#[must_use]
pub fn evaluation_window(
    total_bars: usize,
    visible_start: usize,
    visible_len: usize,
    warmup: usize,
) -> (usize, usize) {
    let eval_start = visible_start.saturating_sub(warmup).min(total_bars);
    let eval_end = visible_start.saturating_add(visible_len).min(total_bars);
    (eval_start, eval_end.saturating_sub(eval_start))
}

#[cfg(test)]
#[path = "../tests/topology.rs"]
mod tests;
