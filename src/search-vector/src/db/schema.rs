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
                     content_text, symbol_name, doc, cjk_text,
                     tokenize='unicode61 remove_diacritics 2',
                     content='fragments', content_rowid='rowid');
                CREATE TABLE IF NOT EXISTS fragment_lemmas (
                    fragment_rowid INTEGER NOT NULL,
                    lemma_id INTEGER NOT NULL,
                    confidence REAL NOT NULL DEFAULT 1.0,
                    PRIMARY KEY (fragment_rowid, lemma_id)
                ) WITHOUT ROWID;
                CREATE INDEX IF NOT EXISTS idx_lemmas_lemma ON fragment_lemmas(lemma_id);
                CREATE TABLE IF NOT EXISTS lemma_terms (
                    id INTEGER PRIMARY KEY,
                    lemma TEXT NOT NULL UNIQUE
                );
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
                CREATE INDEX IF NOT EXISTS idx_graph_symbols_file ON graph_symbols(file);
                CREATE TABLE IF NOT EXISTS symbols (
                    symbol_id TEXT PRIMARY KEY,
                    file_id TEXT NOT NULL,
                    name TEXT,
                    scope TEXT,
                    signature TEXT,
                    interface_hash TEXT NOT NULL DEFAULT '',
                    content_hash TEXT NOT NULL DEFAULT '',
                    range_json TEXT NOT NULL DEFAULT '{}');
                CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id);
                CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
                CREATE TABLE IF NOT EXISTS fragment_summaries (
                    fragment_id TEXT PRIMARY KEY,
                    comment_hash TEXT NOT NULL,
                    model TEXT NOT NULL,
                    prompt_version INTEGER NOT NULL,
                    temperature REAL NOT NULL,
                    seed INTEGER NOT NULL,
                    summary TEXT NOT NULL,
                    status TEXT NOT NULL,
                    generated_at INTEGER NOT NULL DEFAULT 0,
                    authorized INTEGER NOT NULL DEFAULT 0);
                CREATE INDEX IF NOT EXISTS idx_summaries_authorized ON fragment_summaries(authorized);
                CREATE VIRTUAL TABLE IF NOT EXISTS fragment_summaries_fts USING fts5(
                    summary,
                    tokenize='unicode61 remove_diacritics 2',
                    content='fragment_summaries', content_rowid='rowid');
                CREATE TABLE IF NOT EXISTS fragment_embedding_status (
                    fragment_id TEXT PRIMARY KEY,
                    input_hash TEXT NOT NULL,
                    embedder TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL,
                    updated_at INTEGER NOT NULL DEFAULT 0);",
            )
            .map_err(DbError::from)?;
            ensure_file_status_columns(conn)?;
            ensure_fragment_q8_column(conn).map_err(DbError::from)?;
            ensure_fts_doc_column(conn).map_err(DbError::from)?;
            ensure_fts_triggers(conn).map_err(DbError::from)?;
            ensure_lemma_dictionary(conn).map_err(DbError::from)?;
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

/// Comment recall: `doc` joins the FTS index so comment-only terms stay
/// lexically reachable after code-body lemmas moved to comments. FTS5
/// tables cannot be altered, so old databases rebuild the index from the
/// content table on open.
fn ensure_fts_doc_column(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'fragments_fts'",
        [],
        |row| row.get(0),
    )?;
    if sql.contains("doc,") || sql.contains("(doc") {
        return Ok(());
    }
    conn.execute_batch(
        "DROP TABLE IF EXISTS fragments_fts;
         CREATE VIRTUAL TABLE fragments_fts USING fts5(
             content_text, symbol_name, doc, cjk_text,
             tokenize='unicode61 remove_diacritics 2',
             content='fragments', content_rowid='rowid');
         INSERT INTO fragments_fts(fragments_fts) VALUES('rebuild');",
    )
}

/// FTS trigger bodies live here alone: drop-then-create keeps old
/// databases in sync without a second definition site.
fn ensure_fts_triggers(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS fragments_ai;
         DROP TRIGGER IF EXISTS fragments_ad;
         DROP TRIGGER IF EXISTS fragments_au;
         CREATE TRIGGER fragments_ai AFTER INSERT ON fragments BEGIN
             INSERT INTO fragments_fts(rowid, content_text, symbol_name, doc, cjk_text)
             VALUES (new.rowid, new.content_text, new.symbol_name, new.doc, new.cjk_text);
         END;
         CREATE TRIGGER fragments_ad AFTER DELETE ON fragments BEGIN
             INSERT INTO fragments_fts(fragments_fts, rowid, content_text, symbol_name, doc, cjk_text)
             VALUES ('delete', old.rowid, old.content_text, old.symbol_name, old.doc, old.cjk_text);
         END;
         CREATE TRIGGER fragments_au AFTER UPDATE ON fragments BEGIN
             INSERT INTO fragments_fts(fragments_fts, rowid, content_text, symbol_name, doc, cjk_text)
             VALUES ('delete', old.rowid, old.content_text, old.symbol_name, old.doc, old.cjk_text);
             INSERT INTO fragments_fts(rowid, content_text, symbol_name, doc, cjk_text)
             VALUES (new.rowid, new.content_text, new.symbol_name, new.doc, new.cjk_text);
         END;
         DROP TRIGGER IF EXISTS fragment_summaries_ai;
         DROP TRIGGER IF EXISTS fragment_summaries_ad;
         DROP TRIGGER IF EXISTS fragment_summaries_au;
         CREATE TRIGGER fragment_summaries_ai AFTER INSERT ON fragment_summaries BEGIN
             INSERT INTO fragment_summaries_fts(rowid, summary)
             VALUES (new.rowid, new.summary);
         END;
         CREATE TRIGGER fragment_summaries_ad AFTER DELETE ON fragment_summaries BEGIN
             INSERT INTO fragment_summaries_fts(fragment_summaries_fts, rowid, summary)
             VALUES ('delete', old.rowid, old.summary);
         END;
         CREATE TRIGGER fragment_summaries_au AFTER UPDATE ON fragment_summaries BEGIN
             INSERT INTO fragment_summaries_fts(fragment_summaries_fts, rowid, summary)
             VALUES ('delete', old.rowid, old.summary);
             INSERT INTO fragment_summaries_fts(rowid, summary)
             VALUES (new.rowid, new.summary);
         END;",
    )
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

/// Lemma storage normalization: string lemma rows move to integer
/// dictionary keys (`lemma_terms` + integer `fragment_lemmas` rows),
/// preserving lookup order, delete, and rebuild semantics. Old databases
/// migrate on open; the dictionary is derived, so unreferenced terms
/// never block.
fn ensure_lemma_dictionary(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('fragment_lemmas')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !columns.iter().any(|name| name == "lemma") {
        return Ok(());
    }
    conn.execute_batch(
        "ALTER TABLE fragment_lemmas RENAME TO fragment_lemmas_legacy;
         CREATE TABLE fragment_lemmas (
             fragment_rowid INTEGER NOT NULL,
             lemma_id INTEGER NOT NULL,
             confidence REAL NOT NULL DEFAULT 1.0,
             PRIMARY KEY (fragment_rowid, lemma_id)
         ) WITHOUT ROWID;
         INSERT OR IGNORE INTO lemma_terms(lemma)
             SELECT DISTINCT lemma FROM fragment_lemmas_legacy;
         INSERT OR IGNORE INTO fragment_lemmas(fragment_rowid, lemma_id, confidence)
             SELECT f.rowid, d.id, l.confidence FROM fragment_lemmas_legacy l
             JOIN fragments f ON f.id = l.fragment_id
             JOIN lemma_terms d ON d.lemma = l.lemma;
         DROP TABLE fragment_lemmas_legacy;
         DROP INDEX IF EXISTS idx_lemmas_lemma;
         CREATE INDEX idx_lemmas_lemma ON fragment_lemmas(lemma_id);",
    )
}

#[cfg(test)]
#[path = "../../tests/schema.rs"]
mod tests;
