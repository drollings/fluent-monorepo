use std::collections::HashSet;
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
        if !query.is_empty() {
            if let Ok(results) = self.library.keyword_search(query) {
                if !results.is_empty() {
                    return Ok(RoutingResult {
                        query: query.to_string(),
                        result: serde_json::to_string(&results).unwrap_or_default(),
                        tier: "L3".into(),
                    });
                }
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
            if let Ok(hits) = self.library.knn_search(query_emb, self.knn_k, None) {
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

    pub async fn route_async(&self, query: &str, query_emb: Vec<f32>) -> Result<RoutingResult, CacheError> {
        let knn_k = self.knn_k;
        let l4_threshold = self.l4_threshold;
        let l3_max_depth = self.l3_max_depth;
        let lib = Arc::clone(&self.library);
        let query_owned = query.to_string();
        let has_embedding = !query_emb.is_empty();

        let knn_fut = tokio::task::spawn_blocking(move || {
            if !has_embedding {
                return Ok(Vec::new());
            }
            lib.knn_search(&query_emb, knn_k, None)
                .map_err(|_| CacheError::CacheMiss)
        });

        let traverse_fut = if has_embedding {
            None
        } else {
            let lib2 = Arc::clone(&self.library);
            Some(tokio::task::spawn_blocking(move || -> Result<Vec<GraphNode>, CacheError> {
                let node_count = lib2.node_count().map_err(|_| CacheError::CacheMiss)?;
                if node_count == 0 {
                    return Ok(Vec::new());
                }
                let all_nodes = lib2.get_all_node_ids().map_err(|_| CacheError::CacheMiss)?;
                let mut seen = HashSet::new();
                for node_id in all_nodes {
                    if let Ok(nodes) = lib2.traverse_from(node_id, l3_max_depth) {
                        for node in &nodes {
                            seen.insert(node.node_id);
                        }
                    }
                }
                let result: Vec<_> = seen
                    .into_iter()
                    .filter_map(|id| lib2.get_node(id).ok().flatten())
                    .map(|n| GraphNode { node_id: n.id.unwrap(), name: n.name, depth: 0 })
                    .collect();
                Ok(result)
            }))
        };

        let knn_result = knn_fut.await.map_err(|_| CacheError::CacheMiss)?.unwrap_or_default();

        if !knn_result.is_empty() && knn_result[0].distance < l4_threshold {
            return Ok(RoutingResult {
                query: query_owned,
                result: format!("KNN hit: {}", knn_result[0].name.as_str()),
                tier: "L4".into(),
            });
        }

        if let Some(trav) = traverse_fut {
            let traverse_result = trav.await.map_err(|_| CacheError::CacheMiss)?.unwrap_or_default();
            if !traverse_result.is_empty() {
                return Ok(RoutingResult {
                    query: query_owned,
                    result: format!(
                        "Graph traversal: {} nodes at depth {}",
                        traverse_result.len(),
                        traverse_result.iter().map(|n| n.depth).max().unwrap_or(0)
                    ),
                    tier: "L3".into(),
                });
            }
        }

        Err(CacheError::CacheMiss)
    }

    fn traverse_all(&self, max_depth: u8) -> Result<Vec<GraphNode>, CacheError> {
        let node_count = self.library.node_count().map_err(|_| CacheError::CacheMiss)?;
        if node_count == 0 {
            return Ok(vec![]);
        }
        let all_nodes = self
            .library
            .get_all_node_ids()
            .map_err(|_| CacheError::CacheMiss)?;
        let mut seen = HashSet::new();
        for node_id in all_nodes {
            if let Ok(nodes) = self.library.traverse_from(node_id, max_depth) {
                for node in &nodes {
                    seen.insert(node.node_id);
                }
            }
        }
        let result: Vec<_> = seen
            .into_iter()
            .filter_map(|id| self.library.get_node(id).ok().flatten())
            .map(|n| GraphNode {
                node_id: n.id.unwrap(),
                name: n.name,
                depth: 0,
            })
            .collect();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use guidance_common::types::ContextNode;

    use super::*;

    fn make_router() -> ParallelRouter {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        ParallelRouter::new(lib, 10, 0.7, 4)
    }

    fn insert_test_node(lib: &Arc<Library>, name: &str, source: &str) {
        let node = ContextNode {
            id: None,
            name: name.into(),
            source: source.into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        lib.insert_node(&node).expect("insert node");
    }

    fn insert_edge(lib: &Arc<Library>, from: &str, to: &str) {
        let from_id = lib
            .find_node_by_name(from)
            .expect("find")
            .expect("from node");
        let to_id = lib.find_node_by_name(to).expect("find").expect("to node");
        lib.insert_edge(from_id, to_id, "depends", 1.0)
            .expect("insert edge");
    }

    #[test]
    fn test_router_empty_db_returns_miss() {
        let router = make_router();
        let result = router.route("test");
        assert!(result.is_err());
    }

    #[test]
    fn test_router_keyword_search_hit() {
        let router = make_router();
        insert_test_node(&router.library, "zig_compiler", "Zig compiler documentation");
        let result = router.route("zig");
        assert!(result.is_ok());
        let r = result.unwrap();
        assert!(r.result.contains("zig_compiler"));
    }

    #[test]
    fn test_traverse_all_works() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        insert_test_node(&lib, "root", "root source");
        insert_test_node(&lib, "child", "child source");
        insert_test_node(&lib, "grandchild", "grandchild source");
        insert_edge(&lib, "root", "child");
        insert_edge(&lib, "child", "grandchild");

        let router = ParallelRouter::new(lib, 10, 0.7, 4);
        let nodes = router.traverse_all(3).expect("traverse_all");
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn test_route_with_embedding_hit() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let emb = vec![0.1, 0.2, 0.3, 0.4];
        let node = ContextNode {
            id: None,
            name: "target_node".into(),
            source: "source".into(),
            lod: vec![],
            embedding: Some(emb.clone()),
            capabilities: None,
        };
        lib.insert_node(&node).expect("insert");

        let router = ParallelRouter::new(Arc::clone(&lib), 10, 0.7, 4);
        let query_emb = vec![0.1, 0.2, 0.3, 0.4];
        let result = router.route_with_embedding("target", &query_emb);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().tier, "L4");
    }

    #[tokio::test]
    async fn test_route_async_with_embedding() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let emb = vec![0.1, 0.2, 0.3, 0.4];
        let node = ContextNode {
            id: None,
            name: "async_target".into(),
            source: "source".into(),
            lod: vec![],
            embedding: Some(emb.clone()),
            capabilities: None,
        };
        lib.insert_node(&node).expect("insert");

        let router = ParallelRouter::new(Arc::clone(&lib), 10, 0.5, 4);
        let query_emb = vec![0.1, 0.2, 0.3, 0.4];
        let result = router.route_async("async_target", query_emb).await;
        assert!(result.is_ok());
    }
}

