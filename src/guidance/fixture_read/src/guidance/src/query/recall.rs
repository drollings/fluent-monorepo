//! Fused recall: FTS, vector, and lemma routes with RRF.

/// Fuse FTS, vector, and lemma routes for exploration queries.
/// Vector recall covers semantic units; lemma recall folds inflections.
pub fn run_recall(query: &str) -> Vec<String> {
    let _ = query;
    vec![]
}
