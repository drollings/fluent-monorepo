//! Coral: Context-graph library for guidance.
//!
//! ## Modules
//! - `db` — SQLite-backed node store with KNN search, traversal, and capability filtering
//! - `cache_l1` — L1 (hot) cache for frequently accessed nodes
//! - `cache_reactor` — Event-driven cache reactivity
//! - `cache_router` — Multi-tier routing with parallel KNN + traversal
//! - `ingest` — Batch ingestion with deferred transactional flush
//! - `mcp` — JSON-RPC 2.0 server (Model Context Protocol) over STDIO
//! - `wasm_runtime` — WASM plugin bridge implementing `WorkUnit` + `Component`
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
    clippy::manual_is_multiple_of
)]

pub mod cache_l1;
pub mod cache_reactor;
pub mod cache_router;
pub mod db;
pub mod error;
pub mod ingest;
pub mod mcp;
pub mod packer;
pub mod wasm_runtime;
pub mod wvr;
