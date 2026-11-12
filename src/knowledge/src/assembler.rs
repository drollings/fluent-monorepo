//! The deterministic prompt assembler — a pure function object.
//!
//! Moved mechanics from `fluent-router`'s `ledger/prompt.rs` (M10.2).
//! Turns a worker's `speciality`/`instructions` plus a [`LedgerView`] into an
//! optimized context prompt, selecting a fidelity [`Lod`] per node.
//!
//! It has **no I/O, no model calls, and no background dependency** — it is a
//! pure function object, fully unit-testable over the in-memory double. It
//! renders through the single text-exit (seam `lod_text` / `LedgerView::render`
//! semantics) and never triggers LOD derivation beyond what already exists on
//! a node (assembling a prompt must not pay a summarization cost).
//!
//! # Algorithm
//!
//! 1. **Always include first and last node at LOD0**, even if that overruns
//!    the reserved head/tail budget — they are the highest-priority anchors.
//! 2. **Intermediates, greedy by relevance:** with a `RelevanceSignal`, sort
//!    intermediate nodes by cosine distance to the query embedding — more
//!    relevant nodes render finer (closer to `LodSpec::min`), less relevant
//!    coarser (closer to `LodSpec::max`). Without a signal, a uniform
//!    mid-tier default is used.
//! 3. **Respect the budget:** each intermediate starts at its relevance-assigned
//!    `Lod` and degrades toward coarser tiers until it fits the remaining
//!    budget; if even the coarsest tier does not fit, the node is dropped.
//!    First/last LOD0 are guaranteed regardless of budget.
//!
//! The [`AssembledPrompt::node_plan`] output is the key testable artifact: the
//! per-node fidelity decision, independent of the rendered text.
//!
//! Adaptation: the view's `store()` is the concept seam (`Arc<dyn
//! ContentNodeStore>`); node names/roles read from seam snapshots. Rendering
//! is byte-identical to the router's assembler given the same store content.

use std::collections::HashMap;

use fluent_concept::content_store::ContentNodeStore;
use fluent_types::NodeId;

use crate::lod::{Lod, LOD0_FULL_TEXT, LOD5_LABEL};
use crate::lod_policy::{LodSpec, PromptBudget, RelevanceSignal, WorkerContext};
use crate::selector::{CosineRankSelector, FidelitySelector, UniformSelector};
use crate::views::LedgerView;

/// The assembled prompt output.
#[derive(Debug, Clone)]
/// The assembled prompt output.
pub struct AssembledPrompt {
    /// The worker's composed system prompt.
    pub system: String,
    /// The ordered body lines (one per included node), rendered via the single
    /// text-exit.
    pub body: String,
    /// The number of characters consumed by the rendered body (node text +
    /// separators).
    pub budget_used: usize,
    /// The per-node fidelity plan in render order — the key testable artifact.
    pub node_plan: Vec<(NodeId, Lod)>,
}

/// Render a node at the requested `Lod`, degrading to the nearest **already
/// cached** coarser tier, then to LOD0 — without ever triggering lazy LOD
/// derivation (assembling a prompt must not pay a summarization cost).
fn render_node(store: &dyn ContentNodeStore, id: NodeId, requested: Lod) -> String {
    let requested_level = requested.as_u8();
    // Eager tiers (LOD0/LOD5) are always present — short-circuit to the exit.
    if requested_level == LOD0_FULL_TEXT || requested_level == LOD5_LABEL {
        return store.lod_text(id, requested_level).unwrap_or_default();
    }
    // A lazy tier is only rendered if already cached on the node (never
    // triggers derivation). The double and the sqlite home both read
    // cached-or-absent, so this branch is seam-true on both.
    let cached_level = store.snapshot(id).and_then(|n| {
        (requested_level..=LOD5_LABEL)
            .find(|l| n.lod.get(*l as usize).is_some_and(|t| !t.is_empty()))
    });
    let level = cached_level.unwrap_or(LOD0_FULL_TEXT);
    store.lod_text(id, level).unwrap_or_default()
}

/// Estimate a node's rendered size at a given `Lod` (what actually fits the
/// budget) without triggering derivation.
fn estimate(store: &dyn ContentNodeStore, id: NodeId, lod: Lod) -> usize {
    render_node(store, id, lod).chars().count()
}

/// The deterministic prompt assembler — a pure function object.
#[derive(Debug, Clone, Copy, Default)]
/// The deterministic prompt assembler — a pure function object.
pub struct LedgerPromptAssembler;

impl LedgerPromptAssembler {
    /// Assemble a worker's optimized context from a ledger view.
    ///
    /// # Arguments
    /// - `view` — the read surface over the shared store (any `LedgerView`).
    /// - `worker` — the worker's speciality + instructions.
    /// - `budget` — the context-window budget.
    /// - `relevance` — an optional relevance signal (`None`/`Some(None)` =
    ///   uniform fidelity; `Some(Query)` = relevance-ranked).
    /// - `lod_spec` — the fidelity band for intermediate nodes.
    ///
    /// First/last nodes always render at LOD0; intermediates are
    /// relevance/budget-selected within the band.
    pub fn assemble(
        &self,
        view: &dyn LedgerView,
        worker: &WorkerContext,
        budget: &PromptBudget,
        relevance: Option<&RelevanceSignal>,
        lod_spec: &LodSpec,
    ) -> AssembledPrompt {
        // Legacy signal paths delegate to the selector seam (byte-identical):
        // `Query` ranks by cosine, anything else is uniform.
        match relevance.and_then(RelevanceSignal::embedding) {
            Some(query) => {
                let selector = CosineRankSelector::new(query.to_vec());
                self.assemble_with_selector(view, worker, budget, Some(&selector), lod_spec)
            }
            None => {
                self.assemble_with_selector(view, worker, budget, Some(&UniformSelector), lod_spec)
            }
        }
    }

    /// Assemble with an explicit [`FidelitySelector`]: the interchangeable
    /// relevance seam (`None` = uniform fidelity, as the legacy no-signal
    /// path). Budget fitting, anchor guarantees, and rendering are identical
    /// to [`Self::assemble`].
    pub fn assemble_with_selector(
        &self,
        view: &dyn LedgerView,
        worker: &WorkerContext,
        budget: &PromptBudget,
        selector: Option<&dyn FidelitySelector>,
        lod_spec: &LodSpec,
    ) -> AssembledPrompt {
        let store = view.store();
        let ids: Vec<NodeId> = view
            .node_ids()
            .into_iter()
            .filter(|id| !view.exclude(*id))
            .collect();

        if ids.is_empty() {
            return AssembledPrompt {
                system: worker.system_prompt(),
                body: String::new(),
                budget_used: 0,
                node_plan: Vec::new(),
            };
        }

        let first = ids[0];
        let last = ids[ids.len() - 1];

        // Intermediates, in render order.
        let intermediates: Vec<NodeId> = if ids.len() > 2 {
            ids[1..ids.len() - 1].to_vec()
        } else {
            Vec::new()
        };

        // Assign each intermediate a target Lod within the band (most
        // relevant first → finest). Priority order for fitting below.
        // `None` is uniform fidelity, as the legacy no-signal path.
        let uniform = UniformSelector;
        let selector: &dyn FidelitySelector = match selector {
            Some(selector) => selector,
            None => &uniform,
        };
        let targets: Vec<(NodeId, Lod)> = selector.select(store.as_ref(), &intermediates, lod_spec);

        // Budget-fit the intermediates: degrade each toward coarser tiers until
        // it fits; drop it if even the coarsest does not fit. Priority order for
        // fitting is relevance order (most relevant kept first).
        let head_tail_cost =
            estimate(store.as_ref(), first, Lod::LOD0) + estimate(store.as_ref(), last, Lod::LOD0);
        let inter_budget = budget.intermediate_budget(head_tail_cost);
        let mut used = head_tail_cost;
        let mut fitted: Vec<(NodeId, Lod)> = Vec::new();
        for (id, target) in targets {
            let mut lod = target;
            loop {
                let cost = estimate(store.as_ref(), id, lod);
                if used + cost <= inter_budget {
                    used += cost;
                    fitted.push((id, lod));
                    break;
                }
                if lod.as_u8() >= lod_spec.max.as_u8() {
                    // Even the coarsest tier does not fit — drop the node.
                    break;
                }
                lod = Lod::try_from(lod.as_u8() + 1).unwrap_or(lod);
            }
        }

        // Reconstruct the render plan in original order: first, fitted
        // intermediates (kept in render order), last.
        let mut node_plan: Vec<(NodeId, Lod)> = Vec::with_capacity(ids.len());
        node_plan.push((first, Lod::LOD0));
        let fitted_by_id: HashMap<NodeId, Lod> = fitted.into_iter().collect();
        for id in &intermediates {
            if let Some(lod) = fitted_by_id.get(id) {
                node_plan.push((*id, *lod));
            }
        }
        node_plan.push((last, Lod::LOD0));

        // Render the body lines in plan order.
        let mut lines: Vec<String> = Vec::with_capacity(node_plan.len());
        for (id, lod) in &node_plan {
            let node = store.snapshot(*id);
            let name = node
                .as_ref()
                .map(|n| n.name.to_string())
                .unwrap_or_default();
            let role = node
                .as_ref()
                .and_then(|n| n.role.clone())
                .map(|r| r.as_str().to_string())
                .unwrap_or_default();
            let text = render_node(store.as_ref(), *id, *lod);
            lines.push(format!("[{role}] {name}: {text}"));
        }
        let body = lines.join("\n");
        // `budget_used` measures the content consumed: the sum of the rendered
        // node-text characters (not prefixes/separators), so it is comparable to
        // `max_chars` and the intermediate fitting budget.
        let budget_used = node_plan
            .iter()
            .map(|(id, lod)| estimate(store.as_ref(), *id, *lod))
            .sum();

        AssembledPrompt {
            system: worker.system_prompt(),
            body,
            budget_used,
            node_plan,
        }
    }
}

#[cfg(test)]
#[path = "../tests/assembler.rs"]
mod tests;