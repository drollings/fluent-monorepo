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
use protopyne_kernels::indicator::{IndicatorId, IndicatorRegistry, ParamSet, execution_order};

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

#[cfg(test)]
#[path = "../tests/topology.rs"]
mod tests;
