//! Candidate fusion: score sums, ordering, evidence sort, limit/track cut,
//! and hit materialization. Ports zvec `fuseCandidates`, `sortEvidence`,
//! `candidateToHit`, `candidateToTrace`, and the visible/track slice.
//!
//! Scoring runs through the single generalized kernel
//! (`fluent_db::vector::rrf_merge_n`) with zvec 1-based storage ranks.
//!
//! Score stance — read before touching this module: the fused score is an
//! ordinal composite over incommensurable within-route orders, not a
//! confidence. Each route ranks by its own private scale (unbounded BM25
//! magnitudes on FTS, constant weights on the lemma route, embedding
//! distances on the vector route); the kernel sums `1/(K + rank)` terms and
//! therefore compares positions, never magnitudes. Consequences:
//!
//! - A single top-rank hit does not outvote two mid-rank hits. With three
//!   routes, `[Fts#1]` scores `1/61` while `[Lemma#5, Vector#5]` scores
//!   `2/65` — the diffuse candidate fuses first. Precise-hit-wins is not
//!   guaranteed; the boundary pin below (`…_two_mid_ranks_outvote_one_top_rank`)
//!   asserts the exact arithmetic.
//! - No consumer may read fused scores as magnitudes: display rounding is
//!   lossy, cross-route score arithmetic is unsound (constant lemma weights
//!   alone prove the scales do not commute), and rank ties break
//!   lexicographically by id.
//! - Adding a scoring route re-opens this stance. A fourth route changes
//!   every composite sum, so it ships only with a blast-radius re-meter
//!   over the full quality matrix — never as a silent kernel tweak.

use std::collections::HashMap;
use std::time::Instant;

use crate::zg_constants::RRF_K;
use crate::zg_types::{
    derive_matched_by, public_entity_id, Entity, FileInfo, RecallPath, RrfScore, SearchFinalTrace,
    SearchHit, SearchHitEvidence, SearchHitTrace, SearchRecallTrace, SearchStageTrace,
};

/// One recall evidence: the fragment plus where it surfaced.
#[derive(Debug, Clone)]
pub struct RecallEvidence {
    /// Hit fragment.
    pub fragment: crate::zg_types::EntityFragment,
    /// Recall path.
    pub path: RecallPath,
    /// Route id.
    pub route_id: String,
    /// Route query.
    pub query: String,
    /// 1-based recall rank.
    pub rank: Option<usize>,
    /// Raw recall score.
    pub score: Option<f64>,
    /// Force-append flag (set on force-tracked evidence).
    pub forced: Option<bool>,
}

/// A fused candidate under construction.
#[derive(Debug, Clone)]
pub struct RecallCandidate {
    /// Public entity id.
    pub id: String,
    /// Resolved entity.
    pub entity: Entity,
    /// Owning file.
    pub file: FileInfo,
    /// Contributing paths (deduped).
    pub sources: Vec<RecallPath>,
    /// Per-route recall traces.
    pub recall: Vec<SearchRecallTrace>,
    /// Rank-ordered evidence (sorted at materialization).
    pub evidence: Vec<RecallEvidence>,
    /// Fused RRF score (ordinal composite — see [`crate::zg_types::RrfScore`]).
    pub score: crate::zg_types::RrfScore,
    /// 1-based fused rank (0 until fused).
    pub rank: usize,
    /// Force-append flag.
    pub forced: bool,
}

/// Score, order, and rank candidates: `score = Σ 1/(K + rank)` over found
/// recalls, lexicographic id tie-break. Source: `fuseCandidates`.
#[must_use]
pub fn fuse_candidates(mut candidates: Vec<RecallCandidate>) -> Vec<RecallCandidate> {
    let postings: Vec<(String, usize, ())> = candidates
        .iter()
        .flat_map(|candidate| {
            candidate
                .recall
                .iter()
                .filter(|trace| trace.found && trace.rank.is_some())
                .map(|trace| (candidate.id.clone(), trace.rank.unwrap_or(usize::MAX), ()))
        })
        .collect();
    let scores: HashMap<String, f64> = fluent_db::vector::rrf_merge_n(postings, RRF_K)
        .into_iter()
        .map(|(score, id, ())| (id, score))
        .collect();
    for candidate in &mut candidates {
        // The single sanctioned composite wrap: kernel output in,
        // ordinal out. Nothing else in the tree constructs this type
        // from a magnitude except explicit test fixtures.
        candidate.score =
            RrfScore::new(scores.get(&candidate.id).copied().unwrap_or(0.0));
        candidate.forced = candidate
            .recall
            .iter()
            .any(|trace| trace.forced == Some(true));
    }
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    for (index, candidate) in candidates.iter_mut().enumerate() {
        candidate.rank = index + 1;
    }
    candidates
}

/// Sort evidence by rank (missing last), path (`fts` < `vector`), fragment
/// id. Source: `sortEvidence`.
pub fn sort_evidence(evidence: &mut [RecallEvidence]) {
    evidence.sort_by(|left, right| {
        left.rank
            .unwrap_or(usize::MAX)
            .cmp(&right.rank.unwrap_or(usize::MAX))
            .then_with(|| path_order(left.path).cmp(&path_order(right.path)))
            .then_with(|| left.fragment.id.cmp(&right.fragment.id))
    });
}

fn path_order(path: RecallPath) -> u8 {
    match path {
        RecallPath::Fts => 0,
        RecallPath::Vector => 1,
        RecallPath::Lemma => 2,
    }
}

/// Final cut: first `limit` fused candidates plus the force-tracked one when
/// it fell outside. Source: the visible/track slice.
#[must_use]
pub fn apply_limit_and_track(
    mut ordered: Vec<RecallCandidate>,
    limit: usize,
    track_entity_id: Option<&str>,
) -> Vec<RecallCandidate> {
    let mut visible: Vec<RecallCandidate> = ordered.drain(..limit.min(ordered.len())).collect();
    if let Some(tracked) = track_entity_id {
        if !visible.iter().any(|candidate| candidate.id == tracked) {
            if let Some(position) = ordered.iter().position(|candidate| candidate.id == tracked) {
                visible.push(ordered.remove(position));
            }
        }
    }
    visible
}

/// Per-hit trace: recall + fusion position + final cut.
/// Source: `candidateToTrace` (`returnedByLimit = rank <= limit`).
#[must_use]
pub fn candidate_trace(candidate: &RecallCandidate, limit: usize) -> SearchHitTrace {
    SearchHitTrace {
        recall: candidate.recall.clone(),
        fusion: Some(SearchStageTrace {
            rank: candidate.rank,
            score: candidate.score,
            forced: candidate.forced.then_some(true),
        }),
        ranking: None,
        final_trace: SearchFinalTrace {
            returned_by_limit: candidate.rank <= limit,
            cutoff_rank: limit,
        },
    }
}

/// Materialize a candidate into a search hit (evidence sorted, provenance
/// derived). Source: `candidateToHit`.
#[must_use]
pub fn build_hit(candidate: &RecallCandidate, limit: usize, trace: bool) -> SearchHit {
    let mut evidence = candidate.evidence.clone();
    sort_evidence(&mut evidence);
    SearchHit {
        entity: candidate.entity.clone(),
        file: candidate.file.clone(),
        evidence: evidence
            .into_iter()
            .map(|item| SearchHitEvidence {
                range: item.fragment.range.clone(),
                content: item.fragment.content.clone(),
                metadata: item.fragment.metadata.clone(),
                is_entity: item.fragment.id == public_entity_id(&item.fragment),
                path: item.path,
                route_id: Some(item.route_id),
                query: Some(item.query),
                rank: item.rank,
                score: item.score,
                forced: item.forced,
            })
            .collect(),
        rank: candidate.rank,
        score: candidate.score,
        matched_by: derive_matched_by(&candidate.sources),
        trace: trace.then(|| candidate_trace(candidate, limit)),
    }
}

/// Stage-timing collector: `record` stamps elapsed-since-previous.
/// Names mirror the zvec `TimingCollector` stages (`search_total`, …).
#[derive(Debug)]
pub struct TimingCollector {
    start: Instant,
    last: Instant,
    entries: Vec<crate::zg_types::TimingEntry>,
}

impl TimingCollector {
    /// Start collection.
    #[must_use]
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last: now,
            entries: Vec::new(),
        }
    }

    /// Stamp `name` with the milliseconds since the previous stamp.
    pub fn record(&mut self, name: &str) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64() * 1000.0;
        self.last = now;
        self.entries.push(crate::zg_types::TimingEntry {
            name: name.to_string(),
            duration_ms: elapsed,
            count: None,
        });
    }

    /// Finish with a `search_total` stamp and take the entries.
    #[must_use]
    pub fn finish(mut self) -> Vec<crate::zg_types::TimingEntry> {
        let total = self.start.elapsed().as_secs_f64() * 1000.0;
        self.entries.push(crate::zg_types::TimingEntry {
            name: "search_total".to_string(),
            duration_ms: total,
            count: None,
        });
        self.entries
    }
}

impl Default for TimingCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../../tests/query_fusion.rs"]
mod tests;
