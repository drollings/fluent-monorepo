//! Token-budgeted context packs and impact reports.
//!
//! Packs select excerpts through the shared `ContextPacker::ffd_pack`
//! budget-fit core and restore caller-priority order, so rank order
//! survives packing. Impact joins the graph closure with the component
//! gate map: which dependents a change touches, who owns them, and the
//! narrowest gate that covers them.

use fluent_llm::context_packer::ContextPacker;

use crate::component_map::{component_gate_for_path, ComponentGate};
use crate::graph_index::GraphIndex;

/// Default pack budget in tokens.
pub const DEFAULT_PACK_TOKENS: usize = 8_000;

/// One packable excerpt in caller-priority order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    /// Excerpt text (ranked content, signature, or section).
    pub text: String,
    /// Source path for provenance.
    pub source: String,
}

/// A budget-fitted pack in caller-priority order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPack {
    /// Items that fit the budget, caller-priority order preserved.
    pub items: Vec<ContextItem>,
    /// Items dropped by the budget.
    pub dropped: usize,
}

/// Fit excerpts to a token budget through the shared packer core.
/// Deterministic: selection is size-driven, order is caller-priority.
#[must_use]
pub fn pack_context(items: &[ContextItem], max_tokens: usize) -> ContextPack {
    let packer = ContextPacker::new(max_tokens);
    let refs: Vec<(&str, &ContextItem)> =
        items.iter().map(|item| (item.text.as_str(), item)).collect();
    let kept = packer.ffd_pack(&refs);
    let kept_set: Vec<*const ContextItem> =
        kept.into_iter().map(std::ptr::from_ref::<ContextItem>).collect();
    let mut packed = Vec::new();
    for item in items {
        if kept_set.contains(&std::ptr::from_ref::<ContextItem>(item)) {
            packed.push(item.clone());
        }
    }
    ContextPack {
        dropped: items.len().saturating_sub(packed.len()),
        items: packed,
    }
}

/// What a change touches: dependents, owners, and narrowest gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactReport {
    /// Changed paths as given.
    pub changed: Vec<String>,
    /// Changed paths plus transitive dependents, sorted.
    pub dependents: Vec<String>,
    /// Owning gate per touched file, sorted by path.
    pub owners: Vec<(String, ComponentGate)>,
    /// Distinct narrowest gates covering the change, sorted.
    pub gates: Vec<String>,
}

/// Join the graph closure with the component gate map. Pure: no I/O.
#[must_use]
pub fn impact_report(graph: &GraphIndex, changed: &[String]) -> ImpactReport {
    let dependents = graph.dependents_closure(changed);
    let mut owners: Vec<(String, ComponentGate)> = dependents
        .iter()
        .map(|path| (path.clone(), component_gate_for_path(path)))
        .collect();
    owners.sort_by(|left, right| left.0.cmp(&right.0));
    let mut gates: Vec<String> =
        owners.iter().map(|(_, gate)| gate.gate.clone()).collect();
    gates.sort();
    gates.dedup();
    ImpactReport {
        changed: changed.to_vec(),
        dependents,
        owners,
        gates,
    }
}

#[cfg(test)]
#[path = "../../tests/context_pack.rs"]
mod context_pack_tests;

#[cfg(test)]
#[path = "../../tests/impact.rs"]
mod impact_tests;
