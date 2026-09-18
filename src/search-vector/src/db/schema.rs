//! Fragment/graph schema: DDL, init, and migrate-on-open ensures.
//!
//! Moved verbatim from `db.rs` (M6 split): the full `init_schema` body (same
//! SQL text, now taking the store explicitly — including the legacy
//! `guidance_nodes` table, untouched P5 surface) plus the `ensure_*` column
//! guards.

use fluent_db::error::DbError;
use fluent_db::store::SqliteStore;

use super::VectorDbError;

pub(crate) fn init_schema(store: &SqliteStore) -> Result<(), VectorDbError> {
    store.init_schema(
        "CREATE TABLE IF NOT EXISTS guidance_nodes (
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
                "CREATE INDEX IF NOT EXISTS idx_nodes_name ON guidance_nodes(name);
                 CREATE INDEX IF NOT EXISTS idx_nodes_source ON guidance_nodes(source);
                 CREATE INDEX IF NOT EXISTS idx_nodes_name_source ON guidance_nodes(name, source);
                 CREATE INDEX IF NOT EXISTS idx_cache_query_hash ON embedding_cache(query_hash);
                 CREATE TABLE IF NOT EXISTS sync_state (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS files (
                     id TEXT PRIMARY KEY,
                     absolute_path TEXT NOT NULL,
                     relative_path TEXT NOT NULL,
                     root_path TEXT NOT NULL,
                     size_bytes INTEGER NOT NULL DEFAULT 0,
                     last_modified_time INTEGER NOT NULL DEFAULT 0,
                     kind TEXT,
                     format TEXT NOT NULL DEFAULT ''
                 );
                 CREATE TABLE IF NOT EXISTS fragments (
                     id TEXT PRIMARY KEY,
                     grp TEXT,
                     file_id TEXT NOT NULL,
                     range_json TEXT NOT NULL DEFAULT '{}',
                     content_kind TEXT NOT NULL DEFAULT 'text',
                     content_text TEXT,
                     cjk_text TEXT NOT NULL DEFAULT '',
                     symbol_type TEXT,
                     symbol_name TEXT,
                     scope TEXT,
                     signature TEXT,
                     doc TEXT,
                     modifiers TEXT NOT NULL DEFAULT '',
                      heading TEXT,
                      heading_level INTEGER,
                      embedding BLOB,
                      embedding_q8 BLOB
                  );
                 CREATE INDEX IF NOT EXISTS idx_frag_file ON fragments(file_id);
                 CREATE INDEX IF NOT EXISTS idx_frag_group ON fragments(grp);
                 CREATE INDEX IF NOT EXISTS idx_frag_symbol ON fragments(symbol_name);
                 CREATE VIRTUAL TABLE IF NOT EXISTS fragments_fts USING fts5(
                     content_text, symbol_name, cjk_text,
                     tokenize='unicode61 remove_diacritics 2',
                     content='fragments', content_rowid='rowid');
                 CREATE TRIGGER IF NOT EXISTS fragments_ai AFTER INSERT ON fragments BEGIN
                     INSERT INTO fragments_fts(rowid, content_text, symbol_name, cjk_text)
                     VALUES (new.rowid, new.content_text, new.symbol_name, new.cjk_text);
                 END;
                 CREATE TRIGGER IF NOT EXISTS fragments_ad AFTER DELETE ON fragments BEGIN
                     INSERT INTO fragments_fts(fragments_fts, rowid, content_text, symbol_name, cjk_text)
                     VALUES ('delete', old.rowid, old.content_text, old.symbol_name, old.cjk_text);
                 END;
                 CREATE TRIGGER IF NOT EXISTS fragments_au AFTER UPDATE ON fragments BEGIN
                     INSERT INTO fragments_fts(fragments_fts, rowid, content_text, symbol_name, cjk_text)
                     VALUES ('delete', old.rowid, old.content_text, old.symbol_name, old.cjk_text);
                     INSERT INTO fragments_fts(rowid, content_text, symbol_name, cjk_text)
                     VALUES (new.rowid, new.content_text, new.symbol_name, new.cjk_text);
                 END;
                CREATE TABLE IF NOT EXISTS fragment_lemmas (
                    fragment_id TEXT NOT NULL,
                    lemma TEXT NOT NULL,
                    confidence REAL NOT NULL DEFAULT 1.0,
                    PRIMARY KEY (fragment_id, lemma));
                CREATE INDEX IF NOT EXISTS idx_lemmas_lemma ON fragment_lemmas(lemma);
                CREATE TABLE IF NOT EXISTS graph_edges (
                    importer TEXT NOT NULL,
                    specifier TEXT NOT NULL,
                    resolved TEXT,
                    PRIMARY KEY (importer, specifier));
                CREATE INDEX IF NOT EXISTS idx_graph_edges_importer ON graph_edges(importer);
                CREATE INDEX IF NOT EXISTS idx_graph_edges_resolved ON graph_edges(resolved);
                CREATE TABLE IF NOT EXISTS graph_symbols (
                    name TEXT NOT NULL,
                    file TEXT NOT NULL,
                    calls_json TEXT NOT NULL DEFAULT '[]',
                    PRIMARY KEY (name, file));
                CREATE INDEX IF NOT EXISTS idx_graph_symbols_name ON graph_symbols(name);
                CREATE INDEX IF NOT EXISTS idx_graph_symbols_file ON graph_symbols(file);",
            )
            .map_err(DbError::from)?;
            ensure_file_status_columns(conn)?;
            ensure_fragment_q8_column(conn).map_err(DbError::from)?;
            Ok(())
        })?;
    Ok(())
}

/// Add P2 file-status columns to pre-P2 databases (fresh tables already
/// carry them via the idempotent ensure-column no-op guarded here).
fn ensure_file_status_columns(conn: &rusqlite::Connection) -> Result<(), DbError> {
    use fluent_db::migrate::ensure_column;
    for (column, col_def) in [
        ("content_hash", "TEXT"),
        ("index_status", "TEXT DEFAULT 'indexed'"),
        ("fail_count", "INTEGER NOT NULL DEFAULT 0"),
        ("last_error", "TEXT"),
        ("indexed_time", "INTEGER"),
    ] {
        ensure_column(conn, "files", column, col_def)?;
    }
    Ok(())
}

/// P6 int8 rollout: nullable `embedding_q8` on `fragments` (scale plus
/// dims plus i8 values: `dims + 8` bytes vs `4 * dims` FP32). Guarded like
/// the file status columns — old databases gain the column on open, old
/// FP32 rows keep working through the read fallback.
fn ensure_fragment_q8_column(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    use rusqlite::params;
    let mut existing = std::collections::HashSet::new();
    let mut stmt = conn.prepare("PRAGMA table_info(fragments)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows.flatten() {
        existing.insert(name);
    }
    if !existing.contains("embedding_q8") {
        conn.execute(
            "ALTER TABLE fragments ADD COLUMN embedding_q8 BLOB",
            params![],
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/schema.rs"]
mod tests;
