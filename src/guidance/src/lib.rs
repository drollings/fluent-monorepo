#![forbid(unsafe_code)]

//! Guidance: AST-guided vector search & edge AI orchestration engine.
//!
//! This is the **consumer-layer** crate — it composes all domain capabilities
//! into application logic.  It has **13+ domain dependencies** (fluent-types,
//! search-vector, llm, knowledge, tree-sitter, etc.) and is intentionally
//! a coupling hub.  Modules that become independently useful should be extracted
//! to their own crate.
//!
//! ## Module Responsibilities & Coupling
//!
//! | Module | Responsibility | External deps |
//! |--------|---------------|---------------|
//! | `ast_parser` | Tree-sitter AST parsing (Zig, Python) | `fluent-types`, `tree-sitter`, `tree-sitter-zig`, `tree-sitter-python` |
//! | `config` | ProjectConfig loading/serialization | `serde`, `serde_json`, `dirs` |
//! | `sync_engine` | Gen/status/clean orchestration | `fluent-types`, `search-vector`, `ast_parser`, `sync/*` |
//! | `query_engine` | Explain/query orchestration | `fluent-types`, `search-vector`, `fluent-knowledge`, `query/*` |
//! | `query` sub-modules | Query pipeline: identifier, strategy, filters, synthesizer, snapshot | `fluent-types`, `regex` |
//! | `sync` sub-modules | JSON store, staleness, comment management | `fluent-types`, `common-core` |
//! | `search_types` | Index/search value types + pure search helpers (P0 contracts) | `serde`, `thiserror` |
//! | `search_constants` | Tested search/index constants + pure budget helpers | none |
//!
//! ## Extraction Candidates (when other consumers exist)
//! - `config` — pure config types, zero domain coupling
//! - `query/snapshot` — pure file-reading snapshot type
pub mod ast_parser;
pub mod change_set;
pub mod clippy;
pub mod component_map;
pub mod components;
pub mod config;
pub mod coordinator;
pub mod diff;
pub mod extractor;
pub mod fidelity;
pub mod fix;
pub mod freshness;
pub mod gates;
pub mod graph_index;
pub mod grounding;
pub mod index_generation;
pub mod index_pipeline;
pub mod invalidation;
pub mod item_hashes;
pub mod journal;
pub mod maintenance;
pub mod model_lanes;
pub mod offload;
pub mod orchestrator;
pub mod pipeline;
pub mod plans;
pub mod policy;
pub mod query_engine;
pub mod runtime;
pub mod scheduler;
pub mod scorecard;
pub mod seal;
pub mod selection;
pub mod shadow;
pub mod sync_engine;
pub mod triage;
pub mod watcher;
pub mod waves;
pub use common_core::walk;
pub mod search_constants;
pub mod search_types;

pub mod query;
pub mod summary;

pub mod sync {
    pub mod comments;
    pub mod json_store;
    pub mod json_writer;
    pub mod staleness;
}

#[cfg(test)]
mod tests;
