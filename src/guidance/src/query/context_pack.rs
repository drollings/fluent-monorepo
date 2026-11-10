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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextItem {
    /// Excerpt text (ranked content, signature, or section).
    pub text: String,
    /// Source path for provenance.
    pub source: String,
}

/// A budget-fitted pack in caller-priority order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// Strict pack: fresh packs normally, anything else refuses with a
/// reason. Opt-in; `pack_context` keeps its labelled shape.
pub fn pack_context_strict(
    items: &[ContextItem],
    max_tokens: usize,
    freshness: &crate::freshness::Freshness,
) -> Result<ContextPack, crate::freshness::StrictRefusal> {
    if freshness.is_fresh() {
        Ok(pack_context(items, max_tokens))
    } else if freshness.is_pending() {
        Err(crate::freshness::StrictRefusal {
            reason: "pending".to_string(),
        })
    } else if freshness.is_dependent_stale() {
        Err(crate::freshness::StrictRefusal {
            reason: "dependent-stale".to_string(),
        })
    } else {
        Err(crate::freshness::StrictRefusal {
            reason: "stale".to_string(),
        })
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
    /// Distinct edge tiers behind the dependents, sorted.
    pub tiers: Vec<String>,
    /// Test files touching the changed items, sorted.
    pub tests: Vec<String>,
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
    let mut tiers: Vec<String> = graph
        .import_edges()
        .iter()
        .filter(|edge| dependents.contains(&edge.importer))
        .map(|edge| edge.tier.as_str().to_string())
        .chain(graph.call_edges().iter().filter(|edge| {
            dependents.contains(&edge.caller_file)
                || edge.callee_files.iter().any(|file| dependents.contains(file))
        }).map(|edge| edge.tier.as_str().to_string()))
        .collect();
    tiers.sort();
    tiers.dedup();
    ImpactReport {
        changed: changed.to_vec(),
        dependents,
        owners,
        gates,
        tiers,
        tests: Vec::new(),
    }
}

/// Impact with tests-touching: the dependents closure plus the test
/// files exercising the changed items. Test files pair `(path, text)`;
/// item names pair with the changed paths by position.
#[must_use]
pub fn impact_report_with_tests(
    graph: &GraphIndex,
    changed: &[String],
    test_files: &[(String, String)],
    item_names: &[String],
) -> ImpactReport {
    let mut report = impact_report(graph, changed);
    report.tests = tests_touching(test_files, item_names);
    report
}

/// Test files exercising an item: files whose text names it as a token
/// (identifier boundaries, not substrings, so `food` never matches
/// `foo`). Read-only evidence for packs and impact, never an edit set.
#[must_use]
pub fn tests_touching(test_files: &[(String, String)], item_names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, text) in test_files {
        if item_names.iter().any(|name| mentions_token(text, name)) {
            out.push(path.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether `name` occurs as an identifier token in `text`.
fn mentions_token(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|token| token == name)
}

/// Deterministic pack inputs in priority order: target, diagnostic,
/// callee signatures, callers, siblings, test excerpts. Ties within
/// one level resolve by the caller's order, so the same inputs always
/// render the same pack.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PackInput {
    /// Target item source text.
    pub target_source: String,
    /// Target item path for provenance.
    pub target_path: String,
    /// Verbatim diagnostic, when fixing.
    pub diagnostic: Option<String>,
    /// Callee signatures (never bodies).
    pub callee_sigs: Vec<String>,
    /// Capped caller usages.
    pub callers: Vec<String>,
    /// Capped sibling usages.
    pub siblings: Vec<String>,
    /// Read-only test excerpts.
    pub test_excerpts: Vec<String>,
    /// Token budget for the pack.
    pub budget_tokens: usize,
}

/// Citation manifest for a pack: one citation plus one hash per kept
/// item, in pack order, for audit without re-reading sources.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackManifest {
    /// Kept item sources in pack order.
    pub citations: Vec<String>,
    /// Content hashes of the kept items, in pack order.
    pub hashes: Vec<String>,
    /// Retrieval tier per kept item, aligned with citations.
    pub tiers: Vec<String>,
    /// Whether any kept item tripped the untrusted-text screen.
    /// Recorded in measuring mode; enforced in enforcing mode.
    pub suspicious: bool,
}

/// Tier label for a pack item by provenance label. Verbatim target
/// bytes and symbol signatures are exact; collected callers, siblings,
/// and test excerpts are lexical. Pure naming over the source label.
#[must_use]
pub fn tier_for_pack_source(source: &str, target_path: &str) -> &'static str {
    if source == target_path || source == "signatures" {
        crate::search_types::RetrievalTier::Exact.as_str()
    } else {
        crate::search_types::RetrievalTier::Lexical.as_str()
    }
}

/// Build a token-budgeted task pack deterministically: assemble in
/// priority order, fit through the shared packer core, and manifest
/// the kept items. Same inputs yield the same pack, byte-identically.
#[must_use]
pub fn build_task_pack(input: &PackInput) -> (ContextPack, PackManifest) {
    let mut items = vec![ContextItem {
        text: input.target_source.clone(),
        source: input.target_path.clone(),
    }];
    if let Some(diagnostic) = &input.diagnostic {
        items.push(ContextItem {
            text: diagnostic.clone(),
            source: input.target_path.clone(),
        });
    }
    for sig in &input.callee_sigs {
        items.push(ContextItem {
            text: sig.clone(),
            source: "signatures".to_string(),
        });
    }
    for caller in &input.callers {
        items.push(ContextItem {
            text: caller.clone(),
            source: "callers".to_string(),
        });
    }
    for sibling in &input.siblings {
        items.push(ContextItem {
            text: sibling.clone(),
            source: "siblings".to_string(),
        });
    }
    for excerpt in &input.test_excerpts {
        items.push(ContextItem {
            text: excerpt.clone(),
            source: "tests".to_string(),
        });
    }
    let pack = pack_context(&items, input.budget_tokens);
    let manifest = PackManifest {
        citations: pack.items.iter().map(|item| item.source.clone()).collect(),
        hashes: pack
            .items
            .iter()
            .map(|item| common_core::hash::blake3_hex(item.text.as_bytes()))
            .collect(),
        tiers: pack
            .items
            .iter()
            .map(|item| tier_for_pack_source(&item.source, &input.target_path).to_string())
            .collect(),
        suspicious: screen_pack_items(&pack.items).flagged(),
    };
    (pack, manifest)
}

/// Screening verdict over pack excerpts: which items look
/// instruction-shaped. Producer self-doubt only (the text looks like
/// instruction); whether an excerpt is the right evidence stays with
/// citation verification. A confident-looking excerpt can still be
/// wrong for the task; a flagged one can still be the right evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackScreening {
    /// Indices into the screened items that tripped the predicate.
    pub flagged_indices: Vec<usize>,
}

impl PackScreening {
    /// Whether any excerpt tripped the screen.
    #[must_use]
    pub fn flagged(&self) -> bool {
        !self.flagged_indices.is_empty()
    }
}

/// Screen excerpts through the single triage predicate (no copy, no
/// second list). Pure labeling; handling is the caller's mode choice.
#[must_use]
pub fn screen_pack_items(items: &[ContextItem]) -> PackScreening {
    PackScreening {
        flagged_indices: items
            .iter()
            .enumerate()
            .filter(|(_, item)| crate::triage::is_suspicious(&item.text))
            .map(|(index, _)| index)
            .collect(),
    }
}

/// How a flagged pack is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreeningMode {
    /// Record labels in the manifest; emit the pack unchanged.
    Measuring,
    /// Hold flagged packs for review instead of emitting them.
    Enforcing,
}

/// Flagged pack held for review instead of emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldForReview {
    /// Why the pack was held (`untrusted-text`).
    pub reason: String,
}

/// Build a pack with an explicit screening mode. Measuring delegates
/// to `build_task_pack` byte-identically (labels only). Enforcing
/// holds flagged packs for review and emits clean ones; it never
/// silently drops an excerpt.
pub fn build_task_pack_screened(
    input: &PackInput,
    mode: ScreeningMode,
) -> Result<(ContextPack, PackManifest), HeldForReview> {
    let (pack, manifest) = build_task_pack(input);
    if mode == ScreeningMode::Enforcing && manifest.suspicious {
        return Err(HeldForReview {
            reason: "untrusted-text".to_string(),
        });
    }
    Ok((pack, manifest))
}

#[cfg(test)]
#[path = "../../tests/pack_screening.rs"]
mod pack_screening_tests;

#[cfg(test)]
#[path = "../../tests/context_pack.rs"]
mod context_pack_tests;

#[cfg(test)]
#[path = "../../tests/m11_packs.rs"]
mod m11_packs_tests;

#[cfg(test)]
#[path = "../../tests/impact.rs"]
mod impact_tests;
