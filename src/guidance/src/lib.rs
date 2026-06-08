//! Guidance: AST-guided vector search & edge AI orchestration engine.
//!
//! ## Modules
//! - `ast_parser` — Tree-sitter based AST parsing for Zig, Python, and other languages
//! - `plugin` — Plugin loader for external guidance source providers
//! - `query_engine` — Query engine with intent classification, WordIndex fallback, and multi-strategy search
//! - `sync_engine` — File watcher and guidance doc generation (gen, status, staleness)
//! - `query` — Query pipeline: `identifier`, `strategy` (FSM), `llm_filter`, `llm_filter_batch`, `synthesize`
//! - `sync` — Sync infrastructure: `json_store`, `json_writer`, `staleness`, `comments`
//! - `vector` — Vector search: `vector_db` (SQLite + cosine), `math`, `quantized_embedding`, `semantic_aliases`
#![allow(clippy::too_many_arguments)]
pub mod ast_parser;
pub mod config;
pub mod enhancer;
pub mod plugin;
pub mod query_engine;
pub mod sync_engine;

pub mod query {
    pub mod identifier;
    pub mod llm_filter;
    pub mod llm_filter_batch;
    pub mod strategy;
    pub mod synthesize;
}

pub mod sync {
    pub mod comments;
    pub mod json_store;
    pub mod json_writer;
    pub mod staleness;
}

pub mod vector {
    pub mod math;
    pub mod quantized_embedding;
    pub mod semantic_aliases;
    pub mod vector_db;
}
