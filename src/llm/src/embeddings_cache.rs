//! Embedding-cache DDL — re-export shim.
//!
//! The single source of truth moved to `fluent_db::embedding_cache` (generic
//! SQLite schema belongs with the other DDL, not in a client crate). This
//! module re-exports it unchanged so existing `fluent_llm::embeddings_cache`
//! paths keep working; the owner goldens in `tests/embeddings_cache.rs`
//! exercise the re-export and still pin the DDL.
//!
//! `UNIQUE(query_hash)` is identity, not quality — cached-embedding reuse
//! is key-equality, never similarity. The DDL moves unchanged; existing
//! databases are unaffected.

pub use fluent_db::embedding_cache::{init_embedding_cache, EMBEDDING_CACHE_SCHEMA};
