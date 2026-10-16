//! Trading chart topology: the standard nine-pane DAG plus kernel bridges.
//!
//! The generic [`PanePlan`](fluent_egui::PanePlan) owns ordering, readiness,
//! cancellation scope, and checkpoints; this module owns the domain shape
//! poured into it — which panes exist and how the confirmed-bar dataflow
//! wires them (smoothing pair, then oscillators, then the ribbon band, then
//! health). Kernel waves keep the kernel registry's own ordering through
//! [`kernel_order`]; cache keys bind pane names to kernel parameter sets
//! through [`cache_key_for`].

use fluent_egui::{EguiError, PanePlan, ReplayController, cache_key};
use protopyne_chart_model::{ChartRow, DisplayOnly};
use protopyne_kernels::indicator::{IndicatorId, IndicatorRegistry, ParamSet, execution_order};
use protopyne_kernels::{OwnedOhlcv, builtin_registry};

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

/// Fenced full-OHLC preview frame for kernel batch evaluation.
///
/// Display provenance is fenced: every leg enters as [`DisplayOnly`] and is
/// revealed exactly here — the auditable escape for a display-descended
/// preview frame. This is the single construction every kernel-reading
/// terminal module shares, so the fence is named once and no module forks a
/// second display-to-kernel bridge.
pub(crate) fn kernel_preview_frame(rows: &[ChartRow]) -> OwnedOhlcv {
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
