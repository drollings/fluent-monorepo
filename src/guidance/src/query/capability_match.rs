//! Capability-route matching over funneled query terms: SVO signals
//! extracted from the funnel vocabulary plus a pure SVO-overlap
//! rank. Wired: `recall_capability_briefs` ranks every branch-1 pool through it.
//!
//! The query side never sees raw text: extraction runs on the
//! alias-expanded terms, so predicate/role lemmas share the funnel
//! vocabulary instead of reintroducing scaffolding the funnel
//! removed. The rank is rank-only, never a filter — zero overlap
//! still renders in deterministic order.

use std::collections::HashSet;

use common_core::sort::sorted_by_vec;

use crate::graph_index::{signals_from_routing, GraphQuerySignals};

use super::funnel::FunneledTerms;

/// Query-side SVO signals for the capability route, extracted from
/// the funnel's expanded terms. Total and fail-open: no pipeline or
/// an empty funnel yields empty signals (the route falls back to
/// text matching), never invented lemmas.
#[must_use]
pub fn funneled_query_signals(
    funneled: &FunneledTerms,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> GraphQuerySignals {
    if funneled.is_empty() {
        return GraphQuerySignals::default();
    }
    let Some(nlp) = nlp else {
        return GraphQuerySignals::default();
    };
    let text = funneled.expanded.join(" ");
    if text.trim().is_empty() {
        return GraphQuerySignals::default();
    }
    let Ok(doc) = nlp.process_sync(&text, None) else {
        return GraphQuerySignals::default();
    };
    signals_from_routing(&spacy_rs::extract_routing_signals(&doc))
}

/// SVO overlap between query signals and one brief's stored SVO
/// tuple: head-to-head predicate hits plus role coverage over the
/// query's predicate and role lemmas. Both sides must already be
/// lowercase-normalized through the same lemmatizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct CapabilityOverlap {
    /// Query predicate lemmas found in the brief's predicate.
    pub predicate: usize,
    /// Query predicate + role lemmas found in the brief's roles.
    pub roles: usize,
}

impl CapabilityOverlap {
    /// True when nothing overlaps (still renders — rank, not filter).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.predicate == 0 && self.roles == 0
    }

    /// Deterministic rank key: predicate hits sort before any
    /// amount of role-only coverage.
    #[must_use]
    pub fn rank_key(&self) -> (usize, usize) {
        (self.predicate, self.roles)
    }
}

/// Score one brief's SVO tuple against the query signals. Pure.
#[must_use]
pub fn capability_overlap(
    query: &GraphQuerySignals,
    brief_predicate: &[String],
    brief_roles: &[String],
) -> CapabilityOverlap {
    let predicate_set: HashSet<&str> = brief_predicate.iter().map(String::as_str).collect();
    let role_set: HashSet<&str> = brief_roles.iter().map(String::as_str).collect();
    let predicate = query
        .predicate
        .iter()
        .filter(|lemma| predicate_set.contains(lemma.as_str()))
        .count();
    let roles = query
        .predicate
        .iter()
        .chain(query.roles.iter())
        .filter(|lemma| role_set.contains(lemma.as_str()))
        .count();
    CapabilityOverlap { predicate, roles }
}

/// One branch-1 candidate before ranking: a stored brief plus its
/// optional FTS score. Plain data; ranking lives here so the brief
/// route and traversal share one order.
#[derive(Debug, Clone)]
pub struct BriefCandidate {
    /// Capability id (`source_path#anchor`).
    pub id: String,
    /// Model-written purpose brief.
    pub purpose: String,
    /// Stored SVO predicate lemma (empty when the brief yields none).
    pub predicate: String,
    /// Stored SVO role lemmas.
    pub roles: Vec<String>,
    /// Owning crate from the path decomposition.
    pub crate_name: String,
    /// Doc domain from the path decomposition.
    pub domain: String,
    /// Workspace-relative doc path.
    pub source_path: String,
    /// Section anchor within the doc.
    pub anchor: String,
    /// FTS score over brief text (`None` for lemma-only candidates).
    pub fts_score: Option<f64>,
}

/// One SVO-ranked brief: the branch-1 candidate with its deterministic
/// rank (rank-only, never filtered).
#[derive(Debug, Clone)]
pub struct RankedBrief {
    /// Capability id (`source_path#anchor`).
    pub id: String,
    /// Model-written purpose brief.
    pub purpose: String,
    /// Owning crate from the path decomposition.
    pub crate_name: String,
    /// Doc domain from the path decomposition.
    pub domain: String,
    /// Workspace-relative doc path.
    pub source_path: String,
    /// Section anchor within the doc.
    pub anchor: String,
    /// SVO overlap against the funneled query signals.
    pub overlap: CapabilityOverlap,
    /// FTS score over brief text (`0.0` for lemma-only candidates).
    pub fts_score: f64,
    /// SVO predicate lemma (empty when the brief yields none).
    pub predicate: String,
    /// SVO role lemmas.
    pub roles: Vec<String>,
    /// 1-based rank (SVO overlap, then FTS, then id).
    pub rank: usize,
}

/// Rank cap for the rendered section.
pub const CAPABILITY_TOP_BRIEFS: usize = 3;

/// Rank brief candidates by SVO overlap over the funneled query
/// signals, then FTS score, then id. Deterministic; rank-only, never
/// filtered. Capped at [`CAPABILITY_TOP_BRIEFS`].
#[must_use]
pub fn rank_briefs(signals: &GraphQuerySignals, candidates: &[BriefCandidate]) -> Vec<RankedBrief> {
    let scored: Vec<(RankedBrief, (usize, usize), f64)> = candidates
        .iter()
        .map(|candidate| {
            let brief_predicate = if candidate.predicate.is_empty() {
                Vec::new()
            } else {
                vec![candidate.predicate.clone()]
            };
            let overlap = capability_overlap(signals, &brief_predicate, &candidate.roles);
            let brief = RankedBrief {
                id: candidate.id.clone(),
                purpose: candidate.purpose.clone(),
                crate_name: candidate.crate_name.clone(),
                domain: candidate.domain.clone(),
                source_path: candidate.source_path.clone(),
                anchor: candidate.anchor.clone(),
                predicate: candidate.predicate.clone(),
                roles: candidate.roles.clone(),
                overlap,
                fts_score: candidate.fts_score.unwrap_or(0.0),
                rank: 0,
            };
            (
                brief,
                overlap.rank_key(),
                candidate.fts_score.unwrap_or(0.0),
            )
        })
        .collect();
    // M2.5: shared stable sorter; comparator moves verbatim
    // (SVO overlap desc, FTS desc with NaN collapsing to `Equal`,
    // id asc), so NaN and tie behavior from the M2.1 tables hold.
    let scored = sorted_by_vec(scored, |a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
    scored
        .into_iter()
        .take(CAPABILITY_TOP_BRIEFS)
        .enumerate()
        .map(|(index, (mut brief, _, _))| {
            brief.rank = index + 1;
            brief
        })
        .collect()
}

#[cfg(test)]
#[path = "../../tests/query_capability_match.rs"]
mod tests;
