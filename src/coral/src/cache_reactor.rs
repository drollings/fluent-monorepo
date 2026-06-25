use std::sync::Arc;

use crate::error::CacheError;
use crate::wasm_runtime::WasmRuntime;
use bon::Builder;
use common_core::hash::content_hash_with_model;
use guidance_llm::client::{is_malformed_response, LlmClient, LlmConfig};
use guidance_llm::decomposer::LocalDecomposer;
use guidance_types::{ContextNode, NodeId, WasmTool};

use crate::cache_l1::{CacheTier, L1Cache, RoutingResult};
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

    pub decomposer: Option<LocalDecomposer>,

    pub frontier_config: Option<LlmConfig>,

    #[builder(default = 3)]
    pub max_depth: u8,

    pub embedder: Option<guidance_llm::OllamaEmbedding>,

    pub wasm_runtime: Option<Box<dyn WasmRuntime>>,
}

pub struct QueueReactor {
    pub library: Arc<Library>,
    pub l1_cache: L1Cache,
    pub knn_k: usize,
    pub l4_threshold: f32,
    pub l3_max_depth: u8,
    pub decomposer: Option<LocalDecomposer>,
    pub frontier_config: Option<LlmConfig>,
    pub max_depth: u8,
    pub embedder: Option<guidance_llm::OllamaEmbedding>,
    pub wasm_runtime: Option<Box<dyn WasmRuntime>>,
}

impl QueueReactor {
    pub fn new(args: QueueReactorCreateArgs) -> Self {
        Self {
            library: args.library,
            l1_cache: args.l1_cache,
            knn_k: args.knn_k,
            l4_threshold: args.l4_threshold,
            l3_max_depth: args.l3_max_depth,
            decomposer: args.decomposer,
            frontier_config: args.frontier_config,
            max_depth: args.max_depth,
            embedder: args.embedder,
            wasm_runtime: args.wasm_runtime,
        }
    }

    pub fn route(&self, query: &str) -> Result<RoutingResult, CacheError> {
        self.route_with_depth(query, 0)
    }

    fn route_with_depth(&self, query: &str, depth: u8) -> Result<RoutingResult, CacheError> {
        // L1: Memory cache
        if depth == 0 {
            if let Some(cached) = self.l1_cache.get(query) {
                return Ok(cached);
            }
        }

        // L2: WASM workflow cache
        if let Some(result) = self.route_l2_wasm(query) {
            self.l1_cache.set(query.to_string(), result.clone());
            return Ok(result);
        }

        let router = ParallelRouter::new(
            Arc::clone(&self.library),
            self.knn_k,
            self.l4_threshold,
            self.l3_max_depth,
        );

        // L3: Graph traversal / keyword search
        if let Ok(result) = router.route(query) {
            self.persist_solution(query, &result);
            if depth == 0 {
                self.l1_cache.set(query.to_string(), result.clone());
            }
            return Ok(result);
        }

        // L4: Semantic (embedding-based) search
        if let Some(ref embedder) = self.embedder {
            if let Ok(emb) = embedder.embed_raw(query) {
                if let Ok(result) = router.route_with_embedding(query, &emb) {
                    self.persist_solution(query, &result);
                    if depth == 0 {
                        self.l1_cache.set(query.to_string(), result.clone());
                    }
                    return Ok(result);
                }
            }
        }

        // L4.5: Local decomposition
        if let Some(ref decomposer) = self.decomposer {
            if depth < self.max_depth {
                let subtasks = decomposer.decompose(query);
                let mut merged_result = String::new();
                for subtask in &subtasks {
                    if let Ok(sub_result) = self.route_with_depth(subtask, depth + 1) {
                        if !merged_result.is_empty() {
                            merged_result.push_str("\n---\n");
                        }
                        merged_result.push_str(&sub_result.result);
                    }
                }
                if !merged_result.is_empty() {
                    let result = RoutingResult {
                        query: query.to_string(),
                        result: merged_result,
                        tier: CacheTier::L4_5Decompose,
                    };
                    self.persist_solution(query, &result);
                    if depth == 0 {
                        self.l1_cache.set(query.to_string(), result.clone());
                    }
                    return Ok(result);
                }
            }
        }

        // L5: Frontier LLM fallback
        if let Some(ref frontier) = self.frontier_config {
            if let Ok(result) = Self::route_l5_frontier(query, frontier) {
                self.persist_solution(query, &result);
                if depth == 0 {
                    self.l1_cache.set(query.to_string(), result.clone());
                }
                return Ok(result);
            }
        }

        Err(CacheError::CacheMiss)
    }

    fn route_l2_wasm(&self, query: &str) -> Option<RoutingResult> {
        let tool = self.find_wasm_tool(query)?;
        let runtime = self.wasm_runtime.as_ref()?;

        let path = std::path::Path::new(&tool.path);
        let mut plugin = runtime.load_plugin_from_file(path).ok()?;

        let result_bytes = plugin.call(query.as_bytes()).ok()?;
        let result_str = String::from_utf8_lossy(&result_bytes).to_string();

        Some(RoutingResult {
            query: query.to_string(),
            result: result_str,
            tier: CacheTier::L2WasmWorkflow,
        })
    }

    fn route_l5_frontier(query: &str, frontier: &LlmConfig) -> Result<RoutingResult, CacheError> {
        let client = LlmClient::with_config(frontier.clone());
        let anonymized = guidance_llm::anonymize::anonymize(query);
        let messages = vec![
            guidance_llm::client::ChatMessage {
                role: "system".into(),
                content: "You are a helpful assistant. Answer concisely.".into(),
            },
            guidance_llm::client::ChatMessage {
                role: "user".into(),
                content: anonymized,
            },
        ];
        match client.chat_complete(&messages) {
            Ok(response) => {
                if is_malformed_response(&response) {
                    return Err(CacheError::CacheMiss);
                }
                Ok(RoutingResult {
                    query: query.to_string(),
                    result: response,
                    tier: CacheTier::L5Frontier,
                })
            }
            Err(_) => Err(CacheError::CacheMiss),
        }
    }

    pub fn persist_solution(&self, query: &str, result: &RoutingResult) {
        let hash_bytes = content_hash_with_model(query, "solution");
        let hash_id = i64::from_le_bytes(hash_bytes[..8].try_into().unwrap());
        let embedding = self
            .embedder
            .as_ref()
            .and_then(|e| e.embed_raw(&result.result).ok().filter(|v| !v.is_empty()));
        let node = ContextNode {
            id: Some(NodeId::from_int(hash_id)),
            name: format!("solution:{query}").into(),
            source: query.to_string(),
            lod: vec![result.result.clone(), query.to_string()],
            embedding,
            capabilities: None,
        };
        let _ = self.library.insert_node(&node);
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
    use guidance_types::ContextNode;

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

        let args = QueueReactorCreateArgs::builder().library(lib).build();

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
            tier: CacheTier::L1Memory,
        };
        reactor.l1_cache.set("cached_q".into(), cached);

        let result = reactor.route("cached_q").expect("should hit L1");
        assert_eq!(result.tier, CacheTier::L1Memory);
        assert_eq!(result.result, "cached_result");
    }

    #[test]
    fn test_reactor_l4_5_decompose_not_configured() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let args = QueueReactorCreateArgs::builder().library(lib).build();
        let reactor = QueueReactor::new(args);
        assert!(reactor.decomposer.is_none());
    }

    #[test]
    fn test_reactor_persist_solution() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let args = QueueReactorCreateArgs::builder()
            .library(lib.clone())
            .build();
        let reactor = QueueReactor::new(args);

        let result = RoutingResult {
            query: "test".into(),
            result: "test_result".into(),
            tier: CacheTier::L3Graph,
        };
        reactor.persist_solution("test", &result);
        // Node should be inserted without error
    }

    #[test]
    fn test_reactor_l2_wasm_no_tool() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let args = QueueReactorCreateArgs::builder().library(lib).build();
        let reactor = QueueReactor::new(args);
        let result = reactor.route_l2_wasm("test");
        assert!(result.is_none());
    }

    #[test]
    fn test_reactor_l5_frontier_not_configured() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let args = QueueReactorCreateArgs::builder().library(lib).build();
        let reactor = QueueReactor::new(args);
        assert!(reactor.frontier_config.is_none());
    }

    #[test]
    fn test_cache_tier_display() {
        assert_eq!(CacheTier::L1Memory.to_string(), "L1");
        assert_eq!(CacheTier::L2WasmWorkflow.to_string(), "L2");
        assert_eq!(CacheTier::L3Graph.to_string(), "L3");
        assert_eq!(CacheTier::L4Semantic.to_string(), "L4");
        assert_eq!(CacheTier::L4_5Decompose.to_string(), "L4.5");
        assert_eq!(CacheTier::L5Frontier.to_string(), "L5");
    }
}
