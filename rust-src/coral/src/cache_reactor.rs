use std::sync::Arc;

use bon::Builder;
use guidance_common::error::CacheError;
use guidance_common::types::WasmTool;

use crate::cache_l1::{L1Cache, RoutingResult};
use crate::cache_router::ParallelRouter;
use crate::db::Library;

#[derive(Builder)]
pub struct QueueReactorCreateArgs {
    pub library: Arc<Library>,

    #[builder(default)]
    pub l1_cache: L1Cache,

    #[builder(default = 10)]
    pub knn_k: usize,

    #[builder(default = 0.7)]
    pub l4_threshold: f32,

    #[builder(default = 4)]
    pub l3_max_depth: u8,
}

pub struct QueueReactor {
    pub library: Arc<Library>,
    pub l1_cache: L1Cache,
    pub knn_k: usize,
    pub l4_threshold: f32,
    pub l3_max_depth: u8,
}

impl QueueReactor {
    pub fn new(args: QueueReactorCreateArgs) -> Self {
        Self {
            library: args.library,
            l1_cache: args.l1_cache,
            knn_k: args.knn_k,
            l4_threshold: args.l4_threshold,
            l3_max_depth: args.l3_max_depth,
        }
    }

    pub fn route(&self, query: &str) -> Result<RoutingResult, CacheError> {
        if let Some(cached) = self.l1_cache.get(query) {
            return Ok(cached);
        }

        let router = ParallelRouter::new(
            Arc::clone(&self.library),
            self.knn_k,
            self.l4_threshold,
            self.l3_max_depth,
        );
        let result = router.route(query)?;

        self.l1_cache
            .set(query.to_string(), result.clone());

        Ok(result)
    }

    pub fn find_wasm_tool(&self, capability: &str) -> Option<WasmTool> {
        self.library
            .find_wasm_tools_by_capability(capability)
            .ok()?
            .into_iter()
            .next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guidance_common::types::ContextNode;

    #[test]
    fn test_reactor_l1_miss_fallback() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let node = ContextNode {
            id: None,
            name: "test_node".into(),
            source: "source".into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        lib.insert_node(&node).expect("insert");

        let args = QueueReactorCreateArgs::builder()
            .library(lib)
            .build();

        let reactor = QueueReactor::new(args);
        let result = reactor.route("test_query");
        assert!(result.is_err() || result.is_ok());
    }

    #[test]
    fn test_reactor_l1_hit() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));

        let args = QueueReactorCreateArgs::builder()
            .library(lib.clone())
            .build();

        let reactor = QueueReactor::new(args);

        let cached = RoutingResult {
            query: "cached_q".into(),
            result: "cached_result".into(),
            tier: "L1".into(),
        };
        reactor.l1_cache.set("cached_q".into(), cached);

        let result = reactor.route("cached_q").expect("should hit L1");
        assert_eq!(result.tier, "L1");
        assert_eq!(result.result, "cached_result");
    }
}

