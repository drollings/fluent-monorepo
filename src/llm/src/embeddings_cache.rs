//! Embedding-cache DDL — the single owner.
//!
//! The `EMBEDDING_CACHE_SCHEMA` + `init_embedding_cache` schema was deduped
//! from `coral` + `search-vector` into one source of truth here.
//! Generic helpers (`open_wal`, `open_in_memory`, `open_shared_in_memory`,
//! `run_batch`, `make_hnsw`, `is_unique_violation`, `in_clause`) live in
//! `fluent_db::sqlite`.
//!
//! The owner goldens in `tests/embeddings_cache.rs` are the lasting contract.
//!
//! Calibration (roadmap §1, M10): `UNIQUE(query_hash)` is identity, not
//! quality — cached-embedding reuse is key-equality, never similarity.
//! The DDL moves unchanged; existing databases are unaffected.

use rusqlite::{Connection, Result};

/// Canonical `embedding_cache` table DDL.
///
/// This schema was previously duplicated verbatim in `search-vector` and
/// `coral`. It lives here as the single source of truth.
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
