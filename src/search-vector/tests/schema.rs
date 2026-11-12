//! Schema snapshot golden: opening a `GuidanceDb` creates exactly this set
//! of application tables, indexes, and triggers. The M6 module split moves
//! SQL text verbatim — this pins it byte-identical across the move.

use crate::db::GuidanceDb;

fn object_names(db: &GuidanceDb, kind: &str) -> Vec<String> {
    db.store
        .with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type = ?1 ORDER BY name")
                .expect("prepare");
            Ok(stmt
                .query_map([kind], |row| row.get::<_, String>(0))
                .expect("query")
                .collect::<Result<Vec<_>, rusqlite::Error>>()
                .expect("rows"))
        })
        .expect("with_conn")
}

#[test]
fn schema_creates_exact_application_tables() {
    let db = GuidanceDb::open_in_memory().expect("in-memory db");
    let mut tables = object_names(&db, "table");
    tables.retain(|name| !name.starts_with("sqlite_"));
    // Note: `fragments_fts_*` shadow tables are FTS5 internals created with
    // the virtual table — pinned here so trigger/DDL edits can't silently
    // change the FTS footprint.
    assert_eq!(
        tables,
        [
            "embedding_cache",
            "files",
            "fragment_embedding_status",
            "fragment_lemmas",
            "fragment_summaries",
            "fragment_summaries_fts",
            "fragment_summaries_fts_config",
            "fragment_summaries_fts_data",
            "fragment_summaries_fts_docsize",
            "fragment_summaries_fts_idx",
            "fragments",
            "fragments_fts",
            "fragments_fts_config",
            "fragments_fts_data",
            "fragments_fts_docsize",
            "fragments_fts_idx",
            "graph_edges",
            "graph_symbols",
            "guidance_nodes",
            "lemma_terms",
            "symbols",
            "sync_state",
        ],
        "application table set changed: deliberate migration or split drift"
    );
}

#[test]
fn fragment_lemmas_is_without_rowid() {
    // The composite PK is the access path; a rowid table adds a second
    // B-tree that duplicates every key, so WITHOUT ROWID removes it.
    let db = GuidanceDb::open_in_memory().expect("in-memory db");
    let sql: String = db
        .store
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'fragment_lemmas'",
                [],
                |row| row.get(0),
            )?)
        })
        .expect("ddl");
    assert!(sql.contains("WITHOUT ROWID"), "{sql}");
    let autoindexes: Vec<String> = db
        .store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' \
                 AND name LIKE 'sqlite_autoindex_fragment_lemmas%'",
            )?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, rusqlite::Error>>()?;
            Ok(rows)
        })
        .expect("indexes");
    assert!(autoindexes.is_empty(), "{autoindexes:?}");
}

#[test]
fn schema_creates_exact_application_indexes_and_triggers() {
    let db = GuidanceDb::open_in_memory().expect("in-memory db");
    let mut indexes = object_names(&db, "index");
    indexes.retain(|name| !name.starts_with("sqlite_"));
    assert_eq!(
        indexes,
        [
            "idx_cache_query_hash",
            "idx_frag_file",
            "idx_frag_group",
            "idx_frag_symbol",
            "idx_graph_edges_importer",
            "idx_graph_edges_resolved",
            "idx_graph_symbols_file",
            "idx_graph_symbols_name",
            "idx_lemmas_lemma",
            "idx_nodes_name",
            "idx_nodes_name_source",
            "idx_nodes_source",
            "idx_summaries_authorized",
            "idx_symbols_file",
            "idx_symbols_name",
        ],
        "application index set changed: deliberate migration or split drift"
    );
    assert_eq!(
        object_names(&db, "trigger"),
        [
            "fragment_summaries_ad",
            "fragment_summaries_ai",
            "fragment_summaries_au",
            "fragments_ad",
            "fragments_ai",
            "fragments_au",
        ],
        "FTS trigger set changed: deliberate migration or split drift"
    );
}

#[test]
fn fts_triggers_cover_doc_column() {
    // The insert/update/delete triggers must carry `doc` or comment-only
    // terms silently leave the FTS index.
    let db = GuidanceDb::open_in_memory().expect("in-memory db");
    let sql: String = db
        .store
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'fragments_ai'",
                [],
                |row| row.get(0),
            )?)
        })
        .expect("trigger sql");
    assert!(sql.contains("new.doc"), "{sql}");
}

#[test]
fn old_fts_table_gains_doc_column_on_open() {
    // Migration: a pre-doc FTS index gains the column and repopulates it
    // from the content table the next time the database opens.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mig.db");
    let db = GuidanceDb::open(&path).expect("open");
    db.store
        .with_conn(|conn| {
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS fragments_ai;
                 DROP TRIGGER IF EXISTS fragments_ad;
                 DROP TRIGGER IF EXISTS fragments_au;
                 DROP TABLE IF EXISTS fragments_fts;
                 CREATE VIRTUAL TABLE fragments_fts USING fts5(
                     content_text, symbol_name, cjk_text,
                     tokenize='unicode61 remove_diacritics 2',
                     content='fragments', content_rowid='rowid');
                 CREATE TRIGGER fragments_ai AFTER INSERT ON fragments BEGIN
                     INSERT INTO fragments_fts(rowid, content_text, symbol_name, cjk_text)
                     VALUES (new.rowid, new.content_text, new.symbol_name, new.cjk_text);
                 END;
                 CREATE TRIGGER fragments_ad AFTER DELETE ON fragments BEGIN
                     INSERT INTO fragments_fts(fragments_fts, rowid, content_text, symbol_name, cjk_text)
                     VALUES ('delete', old.rowid, old.content_text, old.symbol_name, old.cjk_text);
                 END;
                 CREATE TRIGGER fragments_au AFTER UPDATE ON fragments BEGIN
                     INSERT INTO fragments_fts(fragments_fts, rowid, content_text, symbol_name, cjk_text)
                     VALUES ('delete', old.rowid, old.content_text, old.symbol_name, old.cjk_text);
                     INSERT INTO fragments_fts(rowid, content_text, symbol_name, cjk_text)
                     VALUES (new.rowid, new.content_text, new.symbol_name, new.cjk_text);
                 END;",
            )
            .expect("downgrade");
            Ok(())
        })
        .expect("with_conn");
    db.upsert_fragments(
        &crate::db::fragments::FileRecord {
            id: "file-a".to_string(),
            absolute_path: "/repo/a.rs".to_string(),
            relative_path: "a.rs".to_string(),
            root_path: "/repo".to_string(),
            size_bytes: 10,
            last_modified_time: 10,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        },
        &[crate::db::fragments::FragmentRecord {
            id: "entity-a".to_string(),
            group: None,
            file_id: "file-a".to_string(),
            range_json: "{}".to_string(),
            content_kind: "text".to_string(),
            content_text: Some("pub struct Widget {}".to_string()),
            cjk_text: String::new(),
            symbol_type: Some("struct".to_string()),
            symbol_name: Some("Widget".to_string()),
            scope: None,
            signature: None,
            doc: Some("Adds one to the total".to_string()),
            modifiers: String::new(),
            heading: None,
            heading_level: None,
            embedding: None,
        }],
        &[],
    )
    .expect("upsert");
    drop(db);
    let db = GuidanceDb::open(&path).expect("reopen");
    let hits = db
        .search_fts(
            "total",
            10,
            &crate::db::fragments::FragmentFilter::default(),
        )
        .expect("search");
    assert!(
        hits.iter().any(|hit| hit.id == "entity-a"),
        "migrated index must find comment-only terms"
    );
}
