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
#![allow(clippy::manual_is_multiple_of)]
pub mod db;
pub mod cache_l1;
pub mod cache_reactor;
pub mod cache_router;
pub mod ingest;
pub mod mcp;
pub mod wasm_runtime;
