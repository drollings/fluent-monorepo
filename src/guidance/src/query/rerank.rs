//! Single rerank seam over code findings, doc cards, and briefs.
//!
//! Deterministic spam handling runs first and always: manifest-row
//! collapse, fixture exclusion, file-level dedupe (via
//! `EvidenceSet`, never a second filter). Model verdicts demote
//! ranks only; hard drops wait for a calibrated registry entry.
//! Transport down serves ranked-unfiltered with an explicit label.

pub use fluent_llm::parse::parse_rerank_output;

use crate::read::decide::{CandidateLabel, CandidateVerdict, DocLabel, FindingLabel};
use fluent_llm::decision::{bulk_rerank, Decision};

/// Demotion tier for one verdict label: task-value rank, never a
/// producer-confidence gate. Relevant/Strong first, insufficient
/// next, irrelevant last.
pub trait RerankTier {
    /// Rank tier: lower demotes less.
    fn tier(&self) -> u8;
}

impl RerankTier for FindingLabel {
    fn tier(&self) -> u8 {
        match self {
            FindingLabel::Relevant => 0,
            FindingLabel::Insufficient => 1,
            FindingLabel::Irrelevant => 2,
        }
    }
}

impl RerankTier for DocLabel {
    fn tier(&self) -> u8 {
        match self {
            DocLabel::Strong => 0,
            DocLabel::Considered => 1,
        }
    }
}

/// Collapse manifest rows: keep the first `Cargo.toml` finding,
/// count the rest. Manifests describe the tree, never answer it.
#[must_use]
/// Collapse manifest rows: keep the first `Cargo.toml` finding,
/// count the rest. Manifests describe the tree, never answer it.
pub fn collapse_manifest_rows(
    findings: Vec<crate::read::engine::Finding>,
) -> (Vec<crate::read::engine::Finding>, usize) {
    let mut kept = Vec::with_capacity(findings.len());
    let mut collapsed = 0usize;
    let mut seen_manifest = false;
    for finding in findings {
        let is_manifest = std::path::Path::new(finding.source.display())
            .file_name()
            .is_some_and(|name| name == "Cargo.toml");
        if is_manifest {
            if seen_manifest {
                collapsed += 1;
                continue;
            }
            seen_manifest = true;
        }
        kept.push(finding);
    }
    (kept, collapsed)
}

/// Whether one source path may render: eval rows never pass, fixture
/// rows need the explicit opt-in. The single rule behind every
/// rendered path — citations, cards, doc rows, and findings alike.
#[must_use]
pub fn scope_row_admitted(path: &str, include_fixtures: bool) -> bool {
    let scope = crate::source_scope::classify(path, None);
    scope != crate::source_scope::SourceScope::Eval
        && (include_fixtures || scope != crate::source_scope::SourceScope::Fixture)
}

/// Drop fixture-scope findings unless the escape hatch is set.
/// Eval rows never pass, even opted in.
#[must_use]
/// Drop fixture-scope findings unless the escape hatch is set.
/// Eval rows never pass, even opted in.
pub fn exclude_fixtures(
    findings: Vec<crate::read::engine::Finding>,
    include_fixtures: bool,
) -> Vec<crate::read::engine::Finding> {
    findings
        .into_iter()
        .filter(|finding| scope_row_admitted(finding.source.display(), include_fixtures))
        .collect()
}

/// Serving status of one rerank pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Serving status of one rerank pass.
pub enum RerankStatus {
    /// Verdicts applied as rank demotion.
    Ranked,
    /// Backend down: prior order stands, labelled downstream.
    UnavailableRankedUnfiltered,
}

/// Classify one verdict list: all-zero confidence with candidates
/// present means the backend was down, never a relevance signal.
#[must_use]
/// Classify one verdict list: all-zero confidence with candidates
/// present means the backend was down, never a relevance signal.
pub fn rerank_status<L>(verdicts: &[CandidateVerdict<L>]) -> RerankStatus {
    if !verdicts.is_empty() && verdicts.iter().all(|v| v.confidence == 0.0) {
        RerankStatus::UnavailableRankedUnfiltered
    } else {
        RerankStatus::Ranked
    }
}

/// Rebuild decisions from verdicts for the shared bulk ranker and
/// shadow records. Total: unbuildable verdicts are skipped.
#[must_use]
/// Rebuild decisions from verdicts for the shared bulk ranker and
/// shadow records. Total: unbuildable verdicts are skipped.
pub fn verdict_decisions<L: CandidateLabel>(
    verdicts: &[CandidateVerdict<L>],
    model: &str,
    version: u64,
) -> Vec<(String, Decision)> {
    verdicts
        .iter()
        .filter_map(|verdict| {
            let label = verdict.label.as_str().to_string();
            Decision::from_distribution(
                &label,
                vec![(label.clone(), verdict.confidence.clamp(0.0, 1.0))],
                model.to_string(),
                version,
                0,
                false,
            )
            .ok()
            .map(|decision| (verdict.id.clone(), decision))
        })
        .collect()
}

/// Demote prior id order by verdict tier; within a tier follow the
/// shared bulk confidence order (stable). Rank-only: every prior id
/// appears exactly once, so recall is preserved by construction.
#[must_use]
/// Demote prior id order by verdict tier; within a tier follow the
/// shared bulk confidence order (stable). Rank-only: every prior id
/// appears exactly once, so recall is preserved by construction.
pub fn demote_by_verdict<L: CandidateLabel + RerankTier>(
    prior_ids: &[String],
    verdicts: &[CandidateVerdict<L>],
) -> Vec<String> {
    let decisions: Vec<Decision> = verdicts
        .iter()
        .filter_map(|verdict| {
            let label = verdict.id.clone();
            Decision::from_distribution(
                &label.clone(),
                vec![(label, verdict.confidence.clamp(0.0, 1.0))],
                "rerank".to_string(),
                1,
                0,
                false,
            )
            .ok()
        })
        .collect();
    let mut bulk_position = vec![usize::MAX; verdicts.len()];
    for (position, index) in bulk_rerank(&decisions).iter().enumerate() {
        bulk_position[*index] = position;
    }
    let rank_of = |id: &str| {
        verdicts
            .iter()
            .position(|v| v.id == id)
            .map_or((u8::MAX, usize::MAX), |index| {
                (verdicts[index].label.tier(), bulk_position[index])
            })
    };
    let mut order: Vec<String> = prior_ids.to_vec();
    order.sort_by_key(|a| rank_of(a));
    order
}

/// Admit the top-k prior ids by verdict tier, stable: tier 0 in
/// prior order first, then tier 1, and so on, truncated to `k`.
/// Unjudged ids sort last, like the demotion seam. Never admits
/// zero from a non-empty prior. A cost cap, never a relevance
/// gate: callers report every drop with its label.
#[must_use]
/// Admit the top-k prior ids by verdict tier, stable: tier 0 in
/// prior order first, then tier 1, and so on, truncated to `k`.
/// Unjudged ids sort last, like the demotion seam. Never admits
/// zero from a non-empty prior. A cost cap, never a relevance
/// gate: callers report every drop with its label.
pub fn admit_top<L: CandidateLabel + RerankTier>(
    prior_ids: &[String],
    verdicts: &[CandidateVerdict<L>],
    k: usize,
) -> Vec<String> {
    if prior_ids.is_empty() {
        return Vec::new();
    }
    let cap = k.max(1);
    let tier_of = |id: &str| {
        verdicts
            .iter()
            .find(|verdict| verdict.id == id)
            .map_or(u8::MAX, |verdict| verdict.label.tier())
    };
    let mut tiers: Vec<u8> = prior_ids.iter().map(|id| tier_of(id)).collect();
    tiers.sort_unstable();
    tiers.dedup();
    let mut admitted: Vec<String> = Vec::new();
    for tier in tiers {
        for id in prior_ids {
            if tier_of(id) == tier {
                admitted.push(id.clone());
                if admitted.len() >= cap {
                    return admitted;
                }
            }
        }
    }
    admitted
}

#[cfg(test)]
#[path = "../../tests/rerank_seam.rs"]
mod rerank_seam_tests;
