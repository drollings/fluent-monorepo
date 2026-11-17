//! Generic node-document schema: DDL and the rename ensure.
//!
//! The shared node document store, a generic embedded-JSON primitive with
//! change-gated sync plus vector/keyword search; any consumer may use it.
//! The guidance P1 fragment schema moved to
//! `guidance_core::query::search_db::schema` (same DDL text). A legacy
//! `guidance_nodes` table renames to `nodes` on open.

use fluent_db::error::DbError;
use fluent_db::store::SqliteStore;

use super::VectorDbError;

/// Generic node-document tables only: the rename ensure, the `nodes`
/// table, the embedding cache, and their indexes. Statement order matches
/// the historical single-batch init so fresh files stay byte-identical.
pub(crate) fn init_generic_schema(store: &SqliteStore) -> Result<(), VectorDbError> {
    // Rename before create: a pre-rename database carries its rows in
    // `guidance_nodes`, and creating `nodes` first would shadow them.
    store.with_conn(|conn| ensure_nodes_table(conn).map_err(DbError::from))?;
    store.init_schema(
        "CREATE TABLE IF NOT EXISTS nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                source TEXT NOT NULL,
                signature TEXT,
                comment TEXT,
                module TEXT NOT NULL,
                language TEXT NOT NULL DEFAULT 'zig',
                embedding BLOB,
                created_at TEXT DEFAULT (datetime('now'))
            );",
    )?;
    store.with_conn(|conn| {
        fluent_db::embedding_cache::init_embedding_cache(conn).map_err(DbError::from)?;
        fluent_db::sqlite::run_batch(
            conn,
            "CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
                 CREATE INDEX IF NOT EXISTS idx_nodes_source ON nodes(source);
                 CREATE INDEX IF NOT EXISTS idx_nodes_name_source ON nodes(name, source);
                 CREATE INDEX IF NOT EXISTS idx_cache_query_hash ON embedding_cache(query_hash);",
        )
        .map_err(DbError::from)?;
        Ok(())
    })?;
    Ok(())
}

/// Generic node document store: a legacy `guidance_nodes` table renames
/// to `nodes` on open when a pre-rename database still carries it; fresh
/// databases create `nodes` directly. The store is a shared primitive
/// (embedded JSON documents with change-gated sync plus vector/keyword
/// search), not guidance-specific.
fn ensure_nodes_table(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('guidance_nodes', 'nodes')",
    )?;
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if tables.contains(&"guidance_nodes".to_string()) && !tables.contains(&"nodes".to_string()) {
        conn.execute_batch("ALTER TABLE guidance_nodes RENAME TO nodes")?;
    }
    Ok(())
}
