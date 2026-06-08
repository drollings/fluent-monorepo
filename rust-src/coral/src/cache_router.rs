use std::sync::Arc;

use guidance_common::error::CacheError;
use guidance_common::types::GraphNode;

use crate::cache_l1::RoutingResult;
use crate::db::Library;

pub struct ParallelRouter {
    library: Arc<Library>,
    knn_k: usize,
    l4_threshold: f32,
    l3_max_depth: u8,
}

impl ParallelRouter {
    pub fn new(
        library: Arc<Library>,
        knn_k: usize,
        l4_threshold: f32,
        l3_max_depth: u8,
    ) -> Self {
        Self {
            library,
            knn_k,
            l4_threshold,
            l3_max_depth,
        }
    }

    pub fn route(&self, query: &str) -> Result<RoutingResult, CacheError> {
        if let Ok(hits) = self.library.knn_search(&[], self.knn_k) {
            if !hits.is_empty() && hits[0].distance < self.l4_threshold {
                return Ok(RoutingResult {
                    query: query.to_string(),
                    result: format!("KNN hit: {}", hits[0].name.as_str()),
                    tier: "L4".into(),
                });
            }
        }

        if let Ok(nodes) = self.traverse_all(self.l3_max_depth) {
            if !nodes.is_empty() {
                return Ok(RoutingResult {
                    query: query.to_string(),
                    result: format!(
                        "Graph traversal: {} nodes at depth {}",
                        nodes.len(),
                        nodes.iter().map(|n| n.depth).max().unwrap_or(0)
                    ),
                    tier: "L3".into(),
                });
            }
        }

        Err(CacheError::CacheMiss)
    }

    pub fn route_with_embedding(&self, query: &str, query_emb: &[f32]) -> Result<RoutingResult, CacheError> {
        if !query_emb.is_empty() {
            if let Ok(hits) = self.library.knn_search(query_emb, self.knn_k) {
                if !hits.is_empty() && hits[0].distance < self.l4_threshold {
                    return Ok(RoutingResult {
                        query: query.to_string(),
                        result: format!("KNN hit: {}", hits[0].name.as_str()),
                        tier: "L4".into(),
                    });
                }
            }
        }
        Err(CacheError::CacheMiss)
    }

    fn traverse_all(&self, _max_depth: u8) -> Result<Vec<GraphNode>, CacheError> {
        let conn = self.library.node_count().map_err(|_| CacheError::CacheMiss)?;
        if conn == 0 {
            return Ok(vec![]);
        }
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_router() -> ParallelRouter {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        ParallelRouter::new(lib, 10, 0.7, 4)
    }

    #[test]
    fn test_router_empty_db_returns_miss() {
        let router = make_router();
        let result = router.route("test");
        assert!(result.is_err());
    }
}
