//! Hybrid pipeline: how the query pipeline ranks code hits.

/// Rank code hits vs capability briefs in separate sections: code
/// recall never merges with card recall.
pub fn rank_code_hits(query: &str) -> Vec<String> {
    let _ = query;
    vec![]
}
