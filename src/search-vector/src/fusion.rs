//! Score fusion: RRF kernel surface, boost algebra, and the fused-score
//! ordinal type.
//!
//! Canonical home (ROADMAP_20260911_PRIMITIVES M6) for the fusion math
//! previously spread across `guidance-core` (`search_types::{RrfScore,
//! FusedHit, rrf_fuse}`, the `fuse_candidates` postings step, and the
//! `graph_index::rerank` boosts). Pure, no guidance types: ids are
//! `String`, scores `f64`.
//!
//! The kernel itself stays in `fluent_db::vector::rrf_merge_n` — the single
//! generalized kernel, never a second implementation here. This module is
//! the string-id contract surface plus the boost algebra over it: it calls
//! the kernel in exactly one place ([`score_postings`]); [`rrf_fuse`] and
//! the guidance `fuse_candidates` both compose that seam.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Fused RRF composite score: an ordinal over within-route positions,
/// not a confidence and not a magnitude. The fusion kernel sums
/// `1/(K + rank)` terms across routes whose private scales do not
/// commute (unbounded BM25 magnitudes, constant lemma weights,
/// embedding distances), so composites compare positions only.
/// Consequences, enforced by the shape of this type rather than by
/// prose: no arithmetic (`Add`/`Sub`/`Mul`/`Div`/`Sum` are deliberately
/// absent — a composite never combines with anything, and a raw route
/// score cannot flow here without an explicit wrap); ordering through
/// the derived `PartialOrd` (same `partial_cmp` semantics as the bare
/// `f64` it replaces); magnitude reads only through [`RrfScore::value`]
/// for lossy display. Serialization is transparent: on the wire this
/// is still a JSON number, byte-identical to before.
/// Source: `guidance-core::search_types::RrfScore` (moved, not copied).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RrfScore(pub f64);

impl RrfScore {
    /// Wrap a kernel-produced composite. The single sanctioned call
    /// site is the fusion kernel boundary; every other construction
    /// (tests, fixtures) is explicit and auditable.
    #[must_use]
    pub fn new(score: f64) -> Self {
        Self(score)
    }

    /// Magnitude read for lossy display only (`{:.2}` rendering, trace
    /// text). Rounds away information by design — never feed back into
    /// ranking.
    #[must_use]
    pub fn value(self) -> f64 {
        self.0
    }
}

/// One fused candidate: id + RRF score + 1-based rank.
/// Source: `guidance-core::search_types::FusedHit` (moved, not copied).
#[derive(Debug, Clone, PartialEq)]
pub struct FusedHit {
    /// Candidate id.
    pub id: String,
    /// Summed RRF score.
    pub score: f64,
    /// 1-based fused rank.
    pub rank: usize,
}

/// Boost for candidates that call (or are called by) an anchor symbol.
/// Source: roadmap L4 via `guidance-core::graph_index::L4_CALL_BOOST`.
pub const CALL_BOOST: f64 = 0.5;
/// Boost for files in the dependents closure of an anchor file.
/// Source: roadmap L4 via `guidance-core::graph_index::L4_DEPENDENT_BOOST`.
pub const DEPENDENT_BOOST: f64 = 0.3;
/// Boost for candidates sharing an anchor file.
/// Source: roadmap L4 via `guidance-core::graph_index::L4_SAME_FILE_BOOST`.
pub const SAME_FILE_BOOST: f64 = 0.2;
/// Boost for candidates sharing an anchor scope.
/// Source: roadmap L4 via `guidance-core::graph_index::L4_SCOPE_BOOST`.
pub const SCOPE_BOOST: f64 = 0.1;
/// Role-coverage overlap is tiebreak-scale by construction: an ordinal
/// second sort key, never added into the score (composites never
/// combine — see [`RrfScore`]). Pinned for contract stability; the
/// ranking code paths do not add it.
/// Source: `guidance-core::graph_index::L4_ROLE_EPSILON`.
pub const ROLE_EPSILON: f64 = 1e-4;

/// Score `(id, rank)` postings through the single kernel:
/// `score = Σ 1/(K + rank)` per id; duplicate postings for one id sum
/// (one recall per route, as in zvec `fuseCandidates`). The one
/// kernel-call site in the workspace — [`rrf_fuse`] and the guidance
/// `fuse_candidates` both compose this seam instead of calling
/// `fluent_db::vector::rrf_merge_n` themselves.
#[must_use]
pub fn score_postings(postings: Vec<(String, usize)>, k: f64) -> HashMap<String, f64> {
    fluent_db::vector::rrf_merge_n(
        postings
            .into_iter()
            .map(|(id, rank)| (id, rank, ()))
            .collect(),
        k,
    )
    .into_iter()
    .map(|(score, id, ())| (id, score))
    .collect()
}

/// N-route RRF fusion: `score = Σ 1/(K + rank)` over every found recall,
/// lexicographic id tie-break, 1-based ranks.
/// Source: `pipeline/search/index.ts` (`fuseCandidates`; storage recall
/// ranks are 1-based positions, so list position `pos` fuses at rank
/// `pos + 1`). Moved from `guidance-core::search_types::rrf_fuse` and
/// re-composed over [`score_postings`]; the per-list dedupe and the
/// `(score desc, id asc)` order are byte-identical to the moved code.
#[must_use]
pub fn rrf_fuse(lists: &[Vec<String>], k: f64) -> Vec<FusedHit> {
    let mut postings: Vec<(String, usize)> = Vec::new();
    for list in lists {
        let mut seen = HashSet::new();
        for (pos, id) in list.iter().enumerate() {
            if seen.insert(id.as_str()) {
                postings.push((id.clone(), pos + 1));
            }
        }
    }
    let scores = score_postings(postings, k);
    let mut fused: Vec<FusedHit> = scores
        .into_iter()
        .map(|(id, score)| FusedHit {
            id,
            score,
            rank: 0,
        })
        .collect();
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    for (index, hit) in fused.iter_mut().enumerate() {
        hit.rank = index + 1;
    }
    fused
}

/// Additive boost application: `base + Σ boosts`. Boosts stack —
/// a candidate sharing its anchor's file *and* calling its anchor's
/// symbol earns both terms. Pure addition, so push order never matters
/// to the result; callers push in structural order
/// (file → dependent → call → scope) for auditability.
#[must_use]
pub fn apply_boosts(base: f64, boosts: impl IntoIterator<Item = f64>) -> f64 {
    boosts.into_iter().fold(base, |acc, boost| acc + boost)
}

#[cfg(test)]
#[path = "../tests/fusion.rs"]
mod tests;
