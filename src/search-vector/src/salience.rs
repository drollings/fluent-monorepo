//! Candidate-centrality scoring: a minimal PageRank power loop.
//!
//! Moved verbatim from `fluent-router`'s `ranking.rs` (M7.2): pure graph math
//! with zero domain coupling (`i64` node keys in, normalized ranks out — no
//! ledger, no model, no prompt). The router keeps its salience policy
//! (`SalienceSignal`/`SalienceScorer`/`WEIGHTS`, the multi-key sorts with
//! id-asc tiebreaks, the provider snapshot, the model ranking call); this
//! module owns only the centrality kernel both the snapshot load and any
//! future consumer compose.

use std::collections::BTreeMap;

/// An adjacency map for the candidate-centrality graph (undirected).
pub type NodeGraph = BTreeMap<i64, Vec<i64>>;

/// Fixed iteration bound for the minimal PageRank power loop.
pub const PAGERANK_ITERATIONS: usize = 20;

/// The PageRank teleport probability.
pub const PAGERANK_ALPHA: f64 = 0.85;

/// Minimal PageRank via a fixed-iteration power loop over an undirected graph.
///
/// Deterministic in `f64` (no parallel reduction — the same graph always yields
/// the same ranks) and normalized so the top node scores `1.0`. Dangling nodes
/// (empty adjacency) redistribute their mass evenly through the teleport.
///
/// This is the minimal form of the "graph analytics — PageRank" backlog item,
/// scoped to candidate centrality. It is a small numeric routine, not a
/// general graph-analytics subsystem.
#[must_use]
/// Minimal PageRank via a fixed-iteration power loop over an undirected graph.
/// 
/// Deterministic in `f64` (no parallel reduction — the same graph always yields
/// the same ranks) and normalized so the top node scores `1.0`. Dangling nodes
/// (empty adjacency) redistribute their mass evenly through the teleport.
/// 
/// This is the minimal form of the "graph analytics — PageRank" backlog item,
/// scoped to candidate centrality. It is a small numeric routine, not a
/// general graph-analytics subsystem.
pub fn pagerank(graph: &NodeGraph, iterations: usize, alpha: f64) -> BTreeMap<i64, f64> {
    let n = graph.len().max(1);
    let mut ranks: BTreeMap<i64, f64> = graph.keys().map(|&k| (k, 1.0 / n as f64)).collect();
    for _ in 0..iterations {
        let teleport = (1.0 - alpha) / n as f64;
        let mut dangling = 0.0;
        let mut out_deg: BTreeMap<i64, f64> = BTreeMap::new();
        for (&u, adj) in graph {
            if adj.is_empty() {
                dangling += ranks[&u];
            }
            out_deg.insert(u, adj.len().max(1) as f64);
        }
        let mut next: BTreeMap<i64, f64> = BTreeMap::new();
        for (&u, adj) in graph {
            let mut acc = teleport + alpha * dangling / n as f64;
            for &v in adj {
                acc += alpha * ranks[&v] / out_deg[&v];
            }
            next.insert(u, acc);
        }
        ranks = next;
    }
    let max = ranks.values().copied().fold(0.0_f64, f64::max);
    if max > 0.0 {
        for rank in ranks.values_mut() {
            *rank /= max;
        }
    }
    ranks
}

#[cfg(test)]
#[path = "../tests/salience.rs"]
mod tests;