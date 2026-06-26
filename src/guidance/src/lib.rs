//! Guidance: AST-guided vector search & edge AI orchestration engine.
//!
//! This is the **consumer-layer** crate — it composes all domain capabilities
//! into application logic.  It has **13+ domain dependencies** (guidance-types,
//! search-vector, llm, project-knowledge, tree-sitter, etc.) and is intentionally
//! a coupling hub.  Modules that become independently useful should be extracted
//! to their own crate.
//!
//! ## Module Responsibilities & Coupling
//!
//! | Module | Responsibility | External deps |
//! |--------|---------------|---------------|
//! | `ast_parser` | Tree-sitter AST parsing (Zig, Python) | `guidance-types`, `tree-sitter`, `tree-sitter-zig`, `tree-sitter-python` |
//! | `config` | ProjectConfig loading/serialization | `serde`, `serde_json`, `dirs` |
//! | `enhancer` | LLM comment generation | `guidance-llm`, `guidance-types` |
//! | `plugin` | External subprocess plugin loader | `guidance-types` |
//! | `scanner` | Pattern detection (GoF, ringbuf, etc.) | `common-core` (string utils only) |
//! | `sync_engine` | Gen/status/clean orchestration | `guidance-types`, `guidance-search-vector`, `ast_parser`, `sync/*` |
//! | `query_engine` | Explain/query orchestration | `guidance-types`, `guidance-search-vector`, `guidance-project-knowledge`, `query/*` |
//! | `query` sub-modules | Query pipeline: identifier, strategy, filters, synthesizer, snapshot | `guidance-types`, `regex` |
//! | `sync` sub-modules | JSON store, file lock, staleness, comment management | `guidance-types`, `fs2`, `common-core` |
//!
//! ## Extraction Candidates (when other consumers exist)
//! - `scanner` — pure pattern detection, zero domain coupling
//! - `config` — pure config types, zero domain coupling
//! - `plugin` — clean async subprocess runner
//! - `query/snapshot` — pure file-reading snapshot type
//! - `enhancer` — generic LLM comment generation trait
pub mod ast_parser;
pub mod config;
pub mod enhancer;
pub mod grounding;
pub mod memory;
pub mod plugin;
pub mod query_engine;
pub mod runtime;
pub mod scanner;
pub mod sync_engine;
pub use common_core::walk;

pub mod query;

pub mod sync {
    pub mod comments;
    pub mod json_store;
    pub mod json_writer;
    pub mod staleness;
}
