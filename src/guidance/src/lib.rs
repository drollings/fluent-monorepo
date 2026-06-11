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
#![deny(warnings, clippy::all, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::large_stack_arrays,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::zero_sized_map_values,
    clippy::unnecessary_literal_bound,
    clippy::cast_possible_wrap,
    clippy::unreadable_literal,
    clippy::similar_names,
    clippy::single_char_pattern,
    clippy::byte_char_slices,
    clippy::too_many_arguments
)]

pub mod ast_parser;
pub mod config;
pub mod enhancer;
pub mod plugin;
pub mod query_engine;
pub mod runtime;
pub mod scanner;
pub mod sync_engine;

pub mod query {
    pub mod identifier;
    pub mod llm_filter;
    pub mod llm_filter_batch;
    pub mod snapshot;
    pub mod strategy;
    pub mod synthesize;
}

pub mod sync {
    pub mod comments;
    pub mod file_lock;
    pub mod json_store;
    pub mod json_writer;
    pub mod staleness;
}
