//! Embedding-cache DDL — the single owner.
//!
//! Generic SQLite schema with no LLM logic (one table keyed by
//! `UNIQUE(query_hash)`), so it lives with the other DDL in `fluent-db`
//! rather than in a client crate. Previously homed in
//! The former `fluent_llm::embeddings_cache` re-export shim is gone —
//! consumers use this path directly (the fluent-llm no-domain-imports
//! guard forbids `use fluent_db` in client crates).
//!
//! `UNIQUE(query_hash)` is identity, not quality — cached-embedding reuse
//! is key-equality, never similarity.

use rusqlite::{Connection, Result};

/// Canonical `embedding_cache` table DDL.
pub const EMBEDDING_CACHE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS embedding_cache (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    query_hash TEXT NOT NULL UNIQUE,
    query_text TEXT NOT NULL,
    embedding BLOB NOT NULL,
    created_at TEXT DEFAULT (datetime('now'))
)";

/// Create the `embedding_cache` table if it doesn't already exist.
pub fn init_embedding_cache(conn: &Connection) -> Result<()> {
    conn.execute_batch(EMBEDDING_CACHE_SCHEMA)
}

#[cfg(test)]
#[path = "../tests/embedding_cache.rs"]
mod tests;
