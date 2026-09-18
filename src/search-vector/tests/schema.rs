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
            "fragment_lemmas",
            "fragments",
            "fragments_fts",
            "fragments_fts_config",
            "fragments_fts_data",
            "fragments_fts_docsize",
            "fragments_fts_idx",
            "graph_edges",
            "graph_symbols",
            "guidance_nodes",
            "sync_state",
        ],
        "application table set changed: deliberate migration or split drift"
    );
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
        ],
        "application index set changed: deliberate migration or split drift"
    );
    assert_eq!(
        object_names(&db, "trigger"),
        ["fragments_ad", "fragments_ai", "fragments_au"],
        "FTS trigger set changed: deliberate migration or split drift"
    );
}
