use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use fluent_db::error::DbError;
use fluent_db::hnsw::HnswIndex;
use fluent_db::vector;
use fluent_db::store::SqliteStore;
use rusqlite::params;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum VectorDbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] common_core::error::SqliteError),
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("embedding dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },
    #[error("serialization error: {0}")]
    Serialization(String),
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: i64,
    pub name: String,
    pub source: String,
    pub signature: Option<String>,
    pub similarity: f32,
}

/// SQLite hybrid search engine backed by the canonical `fluent-db` components:
/// a `SqliteStore` for the connection/schema and an `HnswIndex` for the KNN
/// index.
///
/// Two table families coexist: the legacy `guidance_nodes` member index
/// (untouched — P5 regression gate) and the P1 fragment index
/// (FTS5 + vectors + lemmas) that the hybrid recall path queries.
pub struct GuidanceDb {
    store: SqliteStore,
    hnsw: HnswIndex,
}

impl GuidanceDb {
    pub fn open(path: &Path) -> Result<Self, VectorDbError> {
        let store = SqliteStore::open(path)?;
        let db = Self {
            store,
            hnsw: HnswIndex::new(),
        };
        db.init_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self, VectorDbError> {
        let store = SqliteStore::open_in_memory()?;
        let db = Self {
            store,
            hnsw: HnswIndex::new(),
        };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<(), VectorDbError> {
        self.store.init_schema(
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
        self.store.with_conn(|conn| {
            fluent_llm::embeddings_cache::init_embedding_cache(conn).map_err(DbError::from)?;
            common_core::sqlite::run_batch(
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
            ensure_file_status_columns(conn).map_err(DbError::from)?;
            ensure_fragment_q8_column(conn).map_err(DbError::from)?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn insert_node(
        &self,
        name: &str,
        source: &str,
        signature: Option<&str>,
        comment: Option<&str>,
        module: &str,
        language: &str,
        embedding: Option<&[f32]>,
    ) -> Result<i64, VectorDbError> {
        let embedding_blob = embedding.map(vector::vec_to_bytes);

        // INSERT + last_insert_rowid run under one connection lock so the
        // returned id is the row this call wrote.
        let node_id = self.store.with_conn(|conn| {
            fluent_db::query::execute(
                conn,
                "INSERT INTO guidance_nodes (name, source, signature, comment, module, language, embedding)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    name,
                    source,
                    signature,
                    comment,
                    module,
                    language,
                    embedding_blob,
                ],
            )?;
            Ok(fluent_db::query::last_insert_rowid(conn))
        })?;

        // Insert into HNSW index if embedding is provided. The connection lock
        // is released before the index lock so the `hnsw → id_map → conn`
        // ordering is never inverted (R9).
        if let Some(emb) = embedding {
            self.hnsw_insert(node_id, emb);
        }

        Ok(node_id)
    }

    /// Insert a vector into the HNSW index.
    fn hnsw_insert(&self, node_id: i64, embedding: &[f32]) {
        self.hnsw.insert(node_id, embedding);
    }

    /// Rebuild the HNSW index from all embedded nodes in the database.
    pub fn rebuild_hnsw(&self) -> Result<usize, VectorDbError> {
        let rows = self.store.query_rows(
            "SELECT id, embedding FROM guidance_nodes WHERE embedding IS NOT NULL",
            &[],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;

        let count = rows.len();
        self.hnsw.rebuild_from(rows.into_iter(), |blob| {
            let embedding = vector::bytes_to_vec(blob);
            if embedding.is_empty() {
                None
            } else {
                Some(embedding)
            }
        })?;

        Ok(count)
    }

    /// Vector similarity search. Uses HNSW index when available, falls back
    /// to brute-force O(n × d) scan otherwise.
    ///
    /// ## Performance
    /// - With HNSW: O(log n) approximate nearest neighbor search
    /// - Without HNSW, n < 10_000:  sub-millisecond on modern CPU
    /// - Without HNSW, n < 100_000: ~10 ms
    pub fn vector_search(
        &self,
        query_vec: &[f32],
        k: usize,
    ) -> Result<Vec<SearchResult>, VectorDbError> {
        // Try HNSW first, but fall back to brute-force if it returns fewer
        // results than requested (can happen with very small indices).
        if let Some(results) = self.hnsw_search(query_vec, k) {
            if results.len() >= k {
                return Ok(results);
            }
        }

        // Fall back to brute-force
        self.bruteforce_vector_search(query_vec, k)
    }

    /// HNSW approximate nearest neighbor search.
    fn hnsw_search(&self, query_vec: &[f32], k: usize) -> Option<Vec<SearchResult>> {
        let neighbours = self.hnsw.search(query_vec, k);
        if neighbours.is_empty() {
            return None;
        }
        // Resolve HNSW `d_id` indices through the external-id map, then touch
        // the connection. The `hnsw → id_map → conn` order is preserved.
        let id_map = self.hnsw.id_map_snapshot();

        let mut results = Vec::with_capacity(neighbours.len());
        for (d_id, distance) in neighbours {
            if d_id >= id_map.len() {
                continue;
            }
            let node_id = id_map[d_id];

            // Convert cosine distance to similarity via the canonical kernel.
            let similarity = vector::distance_to_similarity(distance);

            if let Ok(Some(row)) = self.store.query_row(
                "SELECT name, source, signature FROM guidance_nodes WHERE id = ?1",
                params![node_id],
                |row| {
                    Ok(SearchResult {
                        id: node_id,
                        name: row.get(0)?,
                        source: row.get(1)?,
                        signature: row.get(2)?,
                        similarity,
                    })
                },
            ) {
                results.push(row);
            }
        }

        Some(results)
    }

    /// Brute-force O(n × d) vector similarity search.
    fn bruteforce_vector_search(
        &self,
        query_vec: &[f32],
        k: usize,
    ) -> Result<Vec<SearchResult>, VectorDbError> {
        let rows = self.store.query_rows(
            "SELECT id, name, source, signature, embedding FROM guidance_nodes WHERE embedding IS NOT NULL",
            &[],
            |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let source: String = row.get(2)?;
                let signature: Option<String> = row.get(3)?;
                let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                Ok((id, name, source, signature, embedding_blob))
            },
        )?;

        let results: Vec<SearchResult> = rows
            .into_iter()
            .filter_map(|(id, name, source, signature, embedding_blob)| {
                let embedding = vector::bytes_to_vec(&embedding_blob?);
                if embedding.len() != query_vec.len() {
                    return None;
                }
                let similarity = vector::cosine_similarity_f32(query_vec, &embedding);
                Some(SearchResult {
                    id,
                    name,
                    source,
                    signature,
                    similarity,
                })
            })
            .collect();

        // Shared top-K tail (P2): descending similarity + truncate(k). The
        // row-metadata join above stays (domain logic, not eligible).
        let results = common_core::score::top_k_by_score(results, k, |r| r.similarity, true);

        Ok(results)
    }

    pub fn keyword_search(&self, query: &str) -> Result<Vec<SearchResult>, VectorDbError> {
        // 1. Try exact full-query substring match first (fast, precise).
        let pattern = format!("%{query}%");
        let exact: Vec<SearchResult> = self.store.query_rows(
            "SELECT id, name, source, signature FROM guidance_nodes
             WHERE name LIKE ?1 OR signature LIKE ?1 OR comment LIKE ?1
             LIMIT 50",
            params![pattern],
            |row| {
                Ok(SearchResult {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source: row.get(2)?,
                    signature: row.get(3)?,
                    similarity: 1.0,
                })
            },
        )?;
        if !exact.is_empty() {
            return Ok(exact);
        }

        // 2. Token-based fallback for natural-language queries. Split
        //    into tokens, require at least `min_matches` tokens to match
        //    name/signature/comment. Short queries (1-2 tokens) need 1
        //    match; longer queries need 2+ to filter noise.
        let tokens: Vec<&str> = query.split_whitespace().filter(|t| t.len() >= 3).collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        // Require a proportional number of token matches to filter noise.
        // Short queries (1-2 tokens) need 1 match; longer queries need
        // ~30% of significant tokens to match, preventing false positives
        // from incidental keyword overlap (e.g. "coral" + "module" matching
        // unrelated code when the query is about quantum entanglement).
        let min_matches: i32 = if tokens.len() <= 2 {
            1
        } else {
            ((tokens.len() as f32 * 0.3).ceil() as i32).max(2)
        };

        // Build positional LIKE conditions for each token.
        let mut wheres = Vec::new();
        let mut param_idx = 1;
        let mut token_patterns: Vec<String> = Vec::new();
        for &tok in &tokens {
            let pattern = format!("%{tok}%");
            token_patterns.push(pattern);
            let t = param_idx;
            wheres.push(format!(
                "(name LIKE ?{t} OR signature LIKE ?{t} OR comment LIKE ?{t})"
            ));
            param_idx += 1;
        }
        let hits_expr = wheres
            .iter()
            .map(|w| format!("CASE WHEN {w} THEN 1 ELSE 0 END"))
            .collect::<Vec<_>>()
            .join(" + ");
        let min_hits_param = param_idx;

        let sql = format!(
            "SELECT id, name, source, signature, ({hits_expr}) AS hits
             FROM guidance_nodes
             WHERE hits >= ?{min_hits_param}
             ORDER BY hits DESC
             LIMIT 50"
        );

        let mut params: Vec<rusqlite::types::Value> = token_patterns
            .into_iter()
            .map(rusqlite::types::Value::from)
            .collect();
        params.push(rusqlite::types::Value::Integer(i64::from(min_matches)));

        self.store
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(conn, &sql, params, |row| {
                    Ok(SearchResult {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        source: row.get(2)?,
                        signature: row.get(3)?,
                        similarity: row.get::<_, i32>(4)? as f32,
                    })
                })
            })
            .map_err(VectorDbError::from)
    }

    pub fn hybrid_search(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        k: usize,
    ) -> Result<Vec<SearchResult>, VectorDbError> {
        let keyword_results = self.keyword_search(query)?;

        let vector_results = if let Some(vec) = query_vec {
            self.vector_search(vec, k)?
        } else {
            Vec::new()
        };

        // Generic ranked fusion over `(id, item)` pairs. Reached via the
        // Canonical `fluent_db::vector::rrf_merge` (M4: shim deleted).
        let mut fused: Vec<SearchResult> = vector::rrf_merge(
            keyword_results.into_iter().map(|r| (r.id, r)).collect(),
            vector_results.into_iter().map(|r| (r.id, r)).collect(),
            60.0,
        )
        .into_iter()
        .map(|(score, mut r)| {
            r.similarity = score as f32;
            r
        })
        .collect();
        fused.truncate(k);

        Ok(fused)
    }

    pub fn get_node_count(&self) -> Result<i64, VectorDbError> {
        let count = self
            .store
            .query_row("SELECT COUNT(*) FROM guidance_nodes", &[], |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(count.unwrap_or(0))
    }

    pub fn get_embedding_count(&self) -> Result<i64, VectorDbError> {
        let count = self.store.query_row(
            "SELECT COUNT(*) FROM guidance_nodes WHERE embedding IS NOT NULL",
            &[],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.unwrap_or(0))
    }

    /// Sync all JSON files from a directory into the `guidance_nodes` table.
    /// Walks JSON files, parses GuidanceDoc, upserts into database.
    /// Rebuilds HNSW index after sync.
    pub fn sync_from_dir(&self, json_dir: &std::path::Path) -> Result<usize, VectorDbError> {
        if !json_dir.is_dir() {
            return Ok(0);
        }

        let mut json_files = Vec::new();
        common_core::walk::walk_files(json_dir, &["json"], |path| {
            json_files.push(path.to_path_buf());
        });
        let fingerprint = node_sync_fingerprint(&json_files);
        // Change gate: member docs untouched since the last successful
        // sync rebuild nothing. Count-zero always runs (an emptied doc
        // dir must still clear stale nodes — never a false-fresh skip
        // over a clear). A corrupt/missing watermark fails open toward
        // the rebuild: wasted time, never stale rows.
        let fresh = self
            .read_node_sync_watermark()
            .ok()
            .flatten()
            .is_some_and(|mark| mark == fingerprint);
        if fingerprint.file_count > 0 && fresh {
            return Ok(0);
        }

        let synced = {
            let mut synced = 0;

            // Clear existing nodes before re-sync to avoid stale duplicates.
            self.store.execute("DELETE FROM guidance_nodes", &[])?;

            for path in &json_files {
                let content = common_core::io::read_to_string_err(path).map_err(|e| {
                    DbError::Other(format!("failed to read {}: {e}", path.display()))
                })?;
                if content.trim().is_empty() {
                    continue;
                }

                let doc: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                    DbError::Other(format!("failed to parse {}: {e}", path.display()))
                })?;

                let source = doc["meta"]["source"].as_str().unwrap_or("");
                let module = doc["meta"]["module"].as_str().unwrap_or("");
                let language = doc["meta"]["language"].as_str().unwrap_or("zig");
                let comment = doc["comment"].as_str();

                // Upsert node — skip duplicates where (name, signature)
                // already exists.  This handles multiple JSON files for
                // the same source (e.g. `guidance/src/...` vs `src/guidance/src/...`).
                if let Some(members) = doc["members"].as_array() {
                    for member in members {
                        let name = member["name"].as_str().unwrap_or("");
                        let signature = member["signature"].as_str();
                        let member_comment = member["comment"].as_str();
                        let _is_anchor = member["is_anchor"].as_bool().unwrap_or(false);

                        // Check for existing row with same (name, signature).
                        let exists: bool = self
                            .store
                            .query_row(
                                "SELECT 1 FROM guidance_nodes WHERE name = ?1 AND signature IS ?2 LIMIT 1",
                                rusqlite::params![name, signature],
                                |_| Ok(true),
                            )?
                            .unwrap_or(false);
                        if exists {
                            continue;
                        }

                        let _ = self.store.execute(
                            "INSERT INTO guidance_nodes (name, source, signature, comment, module, language)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            rusqlite::params![
                                name,
                                source,
                                signature,
                                member_comment.or(comment),
                                module,
                                language
                            ],
                        );
                        synced += 1;
                    }
                }
            }

            synced
        };

        // Rebuild HNSW index after releasing the conn lock
        if synced > 0 {
            let _ = self.rebuild_hnsw();
        }
        // The watermark advances only on success: a crash anywhere
        // above leaves the old mark, so the next sync rebuilds and the
        // rows always converge. Written even when `synced == 0` — an
        // all-empty doc dir is a converged state, not a failure.
        if fingerprint.file_count > 0 {
            self.write_node_sync_watermark(&fingerprint)?;
        }

        Ok(synced)
    }

    /// Read the last successful node-sync watermark, if any. Corrupt
    /// values read as absent (the caller fails open toward a rebuild).
    fn read_node_sync_watermark(
        &self,
    ) -> Result<Option<NodeSyncFingerprint>, VectorDbError> {
        let value: Option<String> = self
            .store
            .query_row(
                "SELECT value FROM sync_state WHERE key = 'node_sync'",
                &[],
                |row| row.get(0),
            )
            .map_err(VectorDbError::from)?;
        Ok(value.and_then(|value| {
            let (mtime, count) = value.split_once(':')?;
            Some(NodeSyncFingerprint {
                max_mtime_ms: mtime.parse().ok()?,
                file_count: count.parse().ok()?,
            })
        }))
    }

    /// Persist the node-sync watermark after a successful rebuild.
    fn write_node_sync_watermark(
        &self,
        fingerprint: &NodeSyncFingerprint,
    ) -> Result<(), VectorDbError> {
        let value = format!(
            "{}:{}",
            fingerprint.max_mtime_ms, fingerprint.file_count
        );
        self.store
            .execute(
                "INSERT OR REPLACE INTO sync_state (key, value) VALUES ('node_sync', ?1)",
                rusqlite::params![value],
            )
            .map_err(VectorDbError::from)?;
        Ok(())
    }

    /// Check if the HNSW index is built.
    pub fn has_hnsw(&self) -> bool {
        self.hnsw.is_built()
    }

    /// Get the number of points in the HNSW index.
    pub fn hnsw_len(&self) -> usize {
        self.hnsw.len()
    }

    // -- P1 fragment index (fragment tables) ----------------------------------

    /// Replace a file's fragments: delete-by-file (fragments, lemmas, FTS
    /// rows via triggers) then insert the file row, fragments, and lemmas
    /// atomically. Returns the fragment count.
    pub fn upsert_fragments(
        &self,
        file: &FileRecord,
        fragments: &[FragmentRecord],
        lemmas: &[FragmentLemma],
    ) -> Result<usize, VectorDbError> {
        self.store
            .transaction(|tx| {
                use rusqlite::params;
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM fragment_lemmas WHERE fragment_id IN (SELECT id FROM fragments WHERE file_id = ?1)",
                    params![file.id],
                )?;
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM fragments WHERE file_id = ?1",
                    params![file.id],
                )?;
                fluent_db::query::execute(
                    tx,
                    "INSERT OR REPLACE INTO files
                     (id, absolute_path, relative_path, root_path, size_bytes, last_modified_time, kind, format,
                      content_hash, index_status, fail_count, last_error, indexed_time)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, NULL,
                             CASE WHEN ?10 = 'indexed' THEN strftime('%s','now') * 1000 ELSE NULL END)",
                    params![
                        file.id,
                        file.absolute_path,
                        file.relative_path,
                        file.root_path,
                        file.size_bytes as i64,
                        file.last_modified_time,
                        file.kind,
                        file.format,
                        file.content_hash,
                        file.index_status.as_deref().unwrap_or("indexed"),
                    ],
                )?;
                for fragment in fragments {
                    // P6 int8: new rows store quantized bytes only
                    // (`dims + 8` vs `4 * dims` FP32); legacy FP32 rows
                    // stay readable through the read fallback.
                    let embedding_q8 = fragment
                        .embedding
                        .as_deref()
                        .map(vector::QuantizedEmbedding::from_f32)
                        .map(|quantized| vector::quantized_to_bytes(&quantized));
                    fluent_db::query::execute(
                        tx,
                        "INSERT INTO fragments
                         (id, grp, file_id, range_json, content_kind, content_text, cjk_text,
                          symbol_type, symbol_name, scope, signature, doc, modifiers,
                          heading, heading_level, embedding, embedding_q8)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, NULL, ?16)",
                        params![
                            fragment.id,
                            fragment.group,
                            fragment.file_id,
                            fragment.range_json,
                            fragment.content_kind,
                            fragment.content_text,
                            fragment.cjk_text,
                            fragment.symbol_type,
                            fragment.symbol_name,
                            fragment.scope,
                            fragment.signature,
                            fragment.doc,
                            fragment.modifiers,
                            fragment.heading,
                            fragment.heading_level,
                            embedding_q8,
                        ],
                    )?;
                }
                for lemma in lemmas {
                    fluent_db::query::execute(
                        tx,
                        "INSERT OR REPLACE INTO fragment_lemmas (fragment_id, lemma, confidence)
                         VALUES (?1, ?2, ?3)",
                        params![lemma.fragment_id, lemma.lemma, lemma.confidence],
                    )?;
                }
                Ok(fragments.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Freshness fingerprint of a file's committed fragment row.
    ///
    /// Mirrors exactly what [`GuidanceDb::upsert_fragments`] stores
    /// (`size_bytes`, `last_modified_time`) plus whether any fragment
    /// already carries an embedding — so callers can skip re-ingesting
    /// unchanged files, but still re-ingest when a newly configured
    /// embedder has vectors to backfill. `None` when the file was never
    /// ingested. One indexed-PK row read; no allocation beyond the id.
    ///
    /// Caveat (shared with member-gen staleness): the fingerprint is
    /// size+mtime, not a content hash — a same-size rewrite inside one
    /// mtime tick still re-ingests (mtime moves), but a content swap
    /// that preserves both size and mtime (`cp -p` of different bytes)
    /// reads as fresh. Change detection, not content addressing.
    pub fn fragment_freshness(
        &self,
        file_id: &str,
    ) -> Result<Option<FragmentFreshness>, VectorDbError> {
        let row: Option<(i64, i64, i64)> = self
            .store
            .query_row(
                "SELECT size_bytes, last_modified_time,
                  EXISTS(SELECT 1 FROM fragments WHERE file_id = files.id
                         AND (embedding IS NOT NULL OR embedding_q8 IS NOT NULL))
                 FROM files WHERE id = ?1",
                rusqlite::params![file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(VectorDbError::from)?;
        Ok(row.map(|(size, mtime, embedded)| FragmentFreshness {
            // A corrupt negative size can never match a real file:
            // fail open toward re-ingest, never toward a stale skip.
            size_bytes: u64::try_from(size).unwrap_or(u64::MAX),
            last_modified_time: mtime,
            embedded: embedded != 0,
        }))
    }

    /// Insert or replace lemma rows without touching fragments.
    pub fn insert_fragment_lemmas(&self, lemmas: &[FragmentLemma]) -> Result<usize, VectorDbError> {
        self.store
            .transaction(|tx| {
                for lemma in lemmas {
                    fluent_db::query::execute(
                        tx,
                        "INSERT OR REPLACE INTO fragment_lemmas (fragment_id, lemma, confidence)
                         VALUES (?1, ?2, ?3)",
                        rusqlite::params![lemma.fragment_id, lemma.lemma, lemma.confidence],
                    )?;
                }
                Ok(lemmas.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Ranked lexical recall over the FTS5 index. Scores are `-bm25`
    /// (higher is better); recall assigns 1-based ranks. An unindexable
    /// query yields no rows (never match-all).
    pub fn search_fts(
        &self,
        query: &str,
        limit: usize,
        filter: &FragmentFilter,
    ) -> Result<Vec<ScoredFragment>, VectorDbError> {
        let Some(match_expr) = build_fts_match(query) else {
            return Ok(Vec::new());
        };
        let mut sql = String::from(
            "SELECT f.id, f.file_id, bm25(fragments_fts) AS r FROM fragments_fts \
             JOIN fragments f ON f.rowid = fragments_fts.rowid \
             LEFT JOIN files fl ON fl.id = f.file_id \
             WHERE fragments_fts MATCH ?1",
        );
        let mut values = vec![rusqlite::types::Value::Text(match_expr)];
        let mut param = 2;
        append_fragment_filter(&mut sql, &mut values, filter, &mut param);
        let _ = write!(sql, " ORDER BY r ASC LIMIT ?{param}");
        values.push(rusqlite::types::Value::Integer(limit as i64));
        self.store
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(conn, &sql, values, |row| {
                    let rank_value: f64 = row.get(2)?;
                    Ok(ScoredFragment {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        score: -rank_value,
                    })
                })
            })
            .map_err(VectorDbError::from)
    }

    /// Ranked vector recall (P6: quantized-first brute-force cosine KNN
    /// with an FP32 fallback for legacy rows; HNSW wiring rides with the P6
    /// calibration). Dimension mismatches and corrupt rows are skipped;
    /// recall assigns 1-based ranks.
    pub fn search_vector_fragments(
        &self,
        embedding: &[f32],
        limit: usize,
        filter: &FragmentFilter,
    ) -> Result<Vec<ScoredFragment>, VectorDbError> {
        let mut sql = String::from(
            "SELECT f.id, f.file_id, f.embedding, f.embedding_q8 FROM fragments f \
             LEFT JOIN files fl ON fl.id = f.file_id \
             WHERE (f.embedding IS NOT NULL OR f.embedding_q8 IS NOT NULL)",
        );
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        let mut param = 1;
        append_fragment_filter(&mut sql, &mut values, filter, &mut param);
        let rows: Vec<FragmentVectorRow> = self
            .store
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(conn, &sql, values, |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
            })
            .map_err(VectorDbError::from)?;
        // Shared vector tail (G-1.vecmath + G-1.topk): quantized-first
        // decode (corrupt rows skipped, legacy FP32 re-quantized on the
        // fly), q8 KNN, distance mapped back to similarity. Order ties fall
        // back to rowid (stable sort); the id tie-break lives in RRF.
        let query = vector::QuantizedEmbedding::from_f32(embedding);
        let decoded: Vec<(String, String, vector::QuantizedEmbedding)> = rows
            .into_iter()
            .filter_map(|(id, file_id, fp32_blob, q8_blob)| {
                let quantized = q8_blob
                    .as_deref()
                    .and_then(vector::quantized_from_bytes)
                    .or_else(|| {
                        fp32_blob
                            .as_deref()
                            .map(vector::bytes_to_vec)
                            .filter(|vec| vec.len() == embedding.len())
                            .map(|vec| vector::QuantizedEmbedding::from_f32(&vec))
                    })?;
                Some((id, file_id, quantized))
            })
            .collect();
        let files: HashMap<&str, &str> = decoded
            .iter()
            .map(|(id, file_id, _)| (id.as_str(), file_id.as_str()))
            .collect();
        Ok(vector::knn_brute_force_q8(
            &query,
            decoded.iter().map(|(id, _, vec)| (id.as_str(), vec)),
            limit,
        )
        .into_iter()
        .map(|(id, distance)| ScoredFragment {
            file_id: files.get(id).unwrap_or(&"").to_string(),
            id: id.to_string(),
            score: f64::from(vector::distance_to_similarity(distance)),
        })
        .collect())
    }

    /// All rows for an entity: the major plus its group minors.
    pub fn fragments_for_entity(&self, entity_id: &str) -> Result<Vec<FragmentRow>, VectorDbError> {
        self.store
            .query_rows(
                "SELECT id, grp, file_id, range_json, content_kind, content_text,
                        symbol_type, symbol_name, scope, signature, doc, modifiers,
                        heading, heading_level
                 FROM fragments WHERE id = ?1 OR grp = ?2 ORDER BY id",
                params![entity_id, entity_id],
                FragmentRow::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Fragment ids carrying any of the given lemmas (stable rowid order).
    /// Fragment ids carrying any listed lemma, most matches first.
    /// `fragment_lemmas` rows are unique per `(fragment_id, lemma)`, so
    /// `COUNT(*)` is the number of distinct query lemmas matched: a
    /// fragment matching the whole query outranks partial matches (exact
    /// discrimination survives rank-only RRF fusion), while uniform
    /// counts fall back to insertion order exactly as before.
    pub fn fragments_for_lemmas(
        &self,
        lemmas: &[String],
        limit: usize,
    ) -> Result<Vec<String>, VectorDbError> {
        if lemmas.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<String> = (0..lemmas.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT fragment_id FROM fragment_lemmas WHERE lemma IN ({}) \
             GROUP BY fragment_id ORDER BY COUNT(*) DESC, MIN(rowid) ASC LIMIT ?{}",
            placeholders.join(","),
            lemmas.len() + 1
        );
        let mut values: Vec<rusqlite::types::Value> =
            lemmas.iter().map(|l| rusqlite::types::Value::Text(l.clone())).collect();
        values.push(rusqlite::types::Value::Integer(limit as i64));
        self.store
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(conn, &sql, values, |row| row.get(0))
            })
            .map_err(VectorDbError::from)
    }

    /// One indexed file by id.
    pub fn get_file(&self, file_id: &str) -> Result<Option<FileRecord>, VectorDbError> {
        self.store
            .query_row(
                "SELECT id, absolute_path, relative_path, root_path, size_bytes,
                        last_modified_time, kind, format, content_hash,
                        index_status, fail_count, last_error
                 FROM files WHERE id = ?1",
                params![file_id],
                FileRecord::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// All indexed files (filter resolution input).
    pub fn list_files(&self) -> Result<Vec<FileRecord>, VectorDbError> {
        self.store
            .query_rows(
                "SELECT id, absolute_path, relative_path, root_path, size_bytes,
                        last_modified_time, kind, format, content_hash,
                        index_status, fail_count, last_error
                 FROM files ORDER BY id",
                &[],
                FileRecord::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Stored source-content hashes (`files.content_hash`) for the
    /// member-JSON clock gate: absolute path → sha256 of the source bytes
    /// at ingest time. Empty when no row carries a hash — the gate then
    /// falls back to mtime behavior, never to a stale skip.
    pub fn source_content_hashes(&self) -> Result<HashMap<String, String>, VectorDbError> {
        let rows: Vec<(String, Option<String>)> = self
            .store
            .query_rows(
                "SELECT absolute_path, content_hash FROM files WHERE content_hash IS NOT NULL",
                &[],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(VectorDbError::from)?;
        Ok(rows
            .into_iter()
            .filter_map(|(path, hash)| hash.map(|hash| (path, hash)))
            .collect())
    }

    /// Whether a table exists (schema-gate input).
    pub fn has_table(&self, table: &str) -> bool {
        self.store
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .flatten()
            .is_some()
    }

    /// Delete-by-file plus upsert of fragments/lemmas in one transaction
    /// (P2 commit primitive; failure is stored data via `mark_file_failed`).
    pub fn replace_file(
        &self,
        file: &FileRecord,
        fragments: &[FragmentRecord],
        lemmas: &[FragmentLemma],
    ) -> Result<usize, VectorDbError> {
        self.upsert_fragments(file, fragments, lemmas)
    }

    /// Record a per-file failure (stored data, never an aborted run).
    /// Upserts the file row so files that fail before their first commit
    /// (embed/prepare) are still tracked for the failed-retry pass.
    pub fn mark_file_failed(&self, file: &FileRecord, reason: &str) -> Result<(), VectorDbError> {
        self.store
            .execute(
                "INSERT INTO files
                 (id, absolute_path, relative_path, root_path, size_bytes, last_modified_time,
                  kind, format, content_hash, index_status, fail_count, last_error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'failed', 1, ?10)
                 ON CONFLICT(id) DO UPDATE SET
                   index_status = 'failed', last_error = ?10, fail_count = fail_count + 1",
                &[
                    &file.id as &dyn rusqlite::ToSql,
                    &file.absolute_path as &dyn rusqlite::ToSql,
                    &file.relative_path as &dyn rusqlite::ToSql,
                    &file.root_path as &dyn rusqlite::ToSql,
                    &(file.size_bytes as i64) as &dyn rusqlite::ToSql,
                    &file.last_modified_time as &dyn rusqlite::ToSql,
                    &file.kind as &dyn rusqlite::ToSql,
                    &file.format as &dyn rusqlite::ToSql,
                    &file.content_hash as &dyn rusqlite::ToSql,
                    &reason as &dyn rusqlite::ToSql,
                ],
            )
            .map(|_| ())
            .map_err(VectorDbError::from)
    }

    /// Drop the whole fragment index (workspace-index drop; the legacy
    /// `guidance_nodes` table is untouched).
    pub fn drop_index(&self) -> Result<(), VectorDbError> {
        self.store
            .transaction(|tx| {
                for table in [
                    "fragment_lemmas",
                    "fragments",
                    "files",
                    "graph_edges",
                    "graph_symbols",
                ] {
                    fluent_db::query::execute(
                        tx,
                        &format!("DELETE FROM {table}"),
                        rusqlite::params![],
                    )?;
                }
                Ok(())
            })
            .map_err(VectorDbError::from)
    }

    /// Delete a file record and all its fragments/lemmas (P2 diff `deleted`),
    /// plus its persisted graph inputs (edges it imports, edges resolved
    /// to it, symbols it defines).
    pub fn delete_file(&self, file_id: &str) -> Result<(), VectorDbError> {
        let path: Option<String> = self
            .store
            .query_row(
                "SELECT absolute_path FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .map_err(VectorDbError::from)?;
        self.store
            .transaction(|tx| {
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM fragment_lemmas WHERE fragment_id IN (SELECT id FROM fragments WHERE file_id = ?1)",
                    rusqlite::params![file_id],
                )?;
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM fragments WHERE file_id = ?1",
                    rusqlite::params![file_id],
                )?;
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM files WHERE id = ?1",
                    rusqlite::params![file_id],
                )?;
                if let Some(path) = &path {
                    fluent_db::query::execute(
                        tx,
                        "DELETE FROM graph_edges WHERE importer = ?1 OR resolved = ?1",
                        rusqlite::params![path],
                    )?;
                    fluent_db::query::execute(
                        tx,
                        "DELETE FROM graph_symbols WHERE file = ?1",
                        rusqlite::params![path],
                    )?;
                }
                Ok(())
            })
            .map_err(VectorDbError::from)
    }

    /// Replace one file's persisted graph inputs (delete + insert in one
    /// transaction). `specifiers` are raw import texts; `resolved` stays
    /// NULL here — specifiers resolve at hydration time against the
    /// workspace file set, never per file. Same-name definitions
    /// (overloads) share one row with unioned calls.
    pub fn replace_file_graph(
        &self,
        importer: &str,
        specifiers: &[String],
        symbols: &[(String, Vec<String>)],
    ) -> Result<(), VectorDbError> {
        let mut merged_names: Vec<String> = Vec::new();
        let mut merged_calls: HashMap<String, Vec<String>> = HashMap::new();
        for (name, calls) in symbols {
            let entry = merged_calls.entry(name.clone()).or_insert_with(|| {
                merged_names.push(name.clone());
                Vec::new()
            });
            for call in calls {
                if !entry.contains(call) {
                    entry.push(call.clone());
                }
            }
        }
        let mut calls_json: HashMap<String, String> = HashMap::new();
        for name in &merged_names {
            let json = serde_json::to_string(&merged_calls[name])
                .map_err(|e| VectorDbError::Serialization(e.to_string()))?;
            calls_json.insert(name.clone(), json);
        }
        self.store
            .transaction(|tx| {
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM graph_edges WHERE importer = ?1",
                    rusqlite::params![importer],
                )?;
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM graph_symbols WHERE file = ?1",
                    rusqlite::params![importer],
                )?;
                for specifier in specifiers {
                    fluent_db::query::execute(
                        tx,
                        "INSERT INTO graph_edges (importer, specifier, resolved)
                         VALUES (?1, ?2, NULL)",
                        rusqlite::params![importer, specifier],
                    )?;
                }
                for name in &merged_names {
                    fluent_db::query::execute(
                        tx,
                        "INSERT INTO graph_symbols (name, file, calls_json)
                         VALUES (?1, ?2, ?3)",
                        rusqlite::params![name, importer, calls_json[name]],
                    )?;
                }
                Ok(())
            })
            .map_err(VectorDbError::from)
    }

    /// All persisted import edges, ordered for deterministic hydration.
    pub fn graph_edge_rows(&self) -> Result<Vec<GraphEdgeRow>, VectorDbError> {
        self.store
            .query_rows(
                "SELECT importer, specifier, resolved FROM graph_edges
                 ORDER BY importer, specifier",
                &[],
                GraphEdgeRow::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// All persisted symbol rows, ordered for deterministic hydration.
    pub fn graph_symbol_rows(&self) -> Result<Vec<GraphSymbolRow>, VectorDbError> {
        self.store
            .query_rows(
                "SELECT name, file, calls_json FROM graph_symbols ORDER BY file, name",
                &[],
                GraphSymbolRow::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Ids of files in `failed` status (failed-retry-once input).
    pub fn failed_file_ids(&self) -> Result<Vec<String>, VectorDbError> {
        self.store
            .query_rows(
                "SELECT id FROM files WHERE index_status = 'failed' ORDER BY id",
                &[],
                |row| row.get(0),
            )
            .map_err(VectorDbError::from)
    }

    /// Major entity ids for a file (major = `grp = id`, or ungrouped).
    /// Diagnostic + adapter input for entity listing.
    pub fn entity_ids_for_file(&self, file_id: &str) -> Result<Vec<String>, VectorDbError> {
        self.store
            .query_rows(
                "SELECT id FROM fragments WHERE file_id = ?1 \
                 AND (grp = id OR grp IS NULL OR grp = '') ORDER BY rowid",
                params![file_id],
                |row| row.get(0),
            )
            .map_err(VectorDbError::from)
    }

    /// Distinct non-empty group ids for files (diagnostic: group collapse).
    pub fn group_ids_for_files(&self, file_ids: &[String]) -> Result<Vec<String>, VectorDbError> {
        if file_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT DISTINCT grp FROM fragments WHERE file_id IN ({}) \
             AND grp IS NOT NULL AND grp != '' ORDER BY grp",
            common_core::sqlite::in_clause(file_ids.len())
        );
        let values: Vec<rusqlite::types::Value> =
            file_ids.iter().map(|id| rusqlite::types::Value::Text(id.clone())).collect();
        self.store
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(conn, &sql, values, |row| row.get(0))
            })
            .map_err(VectorDbError::from)
    }
}

/// One persisted import edge: the importing file, the raw specifier text,
/// and the resolved target (NULL until hydration resolves it against the
/// workspace file set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdgeRow {
    /// Importing file (absolute path; the graph node key).
    pub importer: String,
    /// Raw specifier text.
    pub specifier: String,
    /// Resolved target file, if any.
    pub resolved: Option<String>,
}

impl GraphEdgeRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            importer: row.get(0)?,
            specifier: row.get(1)?,
            resolved: row.get(2)?,
        })
    }
}

/// One persisted symbol site: the defined name, its file, and the raw
/// names it calls. The empty name marks an unnamed scope's calls (kept
/// for call edges, never a definition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSymbolRow {
    /// Defined symbol name ("" = unnamed scope).
    pub name: String,
    /// Defining file (absolute path).
    pub file: String,
    /// Raw callee names.
    pub calls: Vec<String>,
}

impl GraphSymbolRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let raw: String = row.get(2)?;
        Ok(Self {
            name: row.get(0)?,
            file: row.get(1)?,
            calls: serde_json::from_str(&raw).unwrap_or_default(),
        })
    }
}

// -- P1 fragment records, filter, and FTS match builder ---------------------

/// Add P2 file-status columns to pre-P2 databases (fresh tables already
/// carry them via `ALTER ... ADD COLUMN` no-ops guarded here).
fn ensure_file_status_columns(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    use rusqlite::params;
    let mut existing = std::collections::HashSet::new();
    let mut stmt = conn.prepare("PRAGMA table_info(files)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows.flatten() {
        existing.insert(name);
    }
    for (column, ddl) in [
        ("content_hash", "TEXT"),
        ("index_status", "TEXT DEFAULT 'indexed'"),
        ("fail_count", "INTEGER NOT NULL DEFAULT 0"),
        ("last_error", "TEXT"),
        ("indexed_time", "INTEGER"),
    ] {
        if !existing.contains(column) {
            conn.execute(
                &format!("ALTER TABLE files ADD COLUMN {column} {ddl}"),
                params![],
            )?;
        }
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
        conn.execute("ALTER TABLE fragments ADD COLUMN embedding_q8 BLOB", params![])?;
    }
    Ok(())
}

/// Indexed file row (mirrors the P0 `FileInfo` contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    /// Stable file id.
    pub id: String,
    /// Absolute workspace path.
    pub absolute_path: String,
    /// Workspace-relative path.
    pub relative_path: String,
    /// Owning root path.
    pub root_path: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Last-modified time (ms epoch).
    pub last_modified_time: i64,
    /// File kind tag.
    pub kind: Option<String>,
    /// Format tag.
    pub format: String,
    /// Content hash (sha256 hex), when computed.
    pub content_hash: Option<String>,
    /// Index status (`indexed` | `failed` | `None` = pending).
    pub index_status: Option<String>,
    /// Consecutive failure count.
    pub fail_count: i64,
    /// Last failure reason.
    pub last_error: Option<String>,
}

impl FileRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            absolute_path: row.get(1)?,
            relative_path: row.get(2)?,
            root_path: row.get(3)?,
            size_bytes: row.get::<_, i64>(4)? as u64,
            last_modified_time: row.get(5)?,
            kind: row.get(6)?,
            format: row.get(7)?,
            content_hash: row.get(8)?,
            index_status: row.get(9)?,
            fail_count: row.get(10)?,
            last_error: row.get(11)?,
        })
    }
}

/// Freshness fingerprint of a file's committed fragment row (see
/// [`GuidanceDb::fragment_freshness`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentFreshness {
    /// File size in bytes at ingest time.
    pub size_bytes: u64,
    /// Last-modified time (ms epoch) at ingest time.
    pub last_modified_time: i64,
    /// Whether any committed fragment carries an embedding.
    pub embedded: bool,
}

/// Change fingerprint for the node-sync gate (see
/// [`GuidanceDb::sync_from_dir`]): the doc set is unchanged iff both
/// the file count and the newest mtime match the last successful sync.
/// Same risk class as all mtime discipline here — a delete+add pair
/// that keeps the count with an older-or-equal max mtime reads as
/// fresh (change detection, not content addressing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeSyncFingerprint {
    /// Newest member-doc mtime (ms epoch), 0 when unreadable.
    max_mtime_ms: i64,
    /// Member-doc file count.
    file_count: usize,
}

fn node_sync_fingerprint(json_files: &[std::path::PathBuf]) -> NodeSyncFingerprint {
    let mut max_mtime_ms = 0i64;
    for path in json_files {
        let ms = common_core::io::mtime(path)
            .and_then(|mtime| {
                mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis() as i64)
                    .ok()
            })
            .unwrap_or(0);
        max_mtime_ms = max_mtime_ms.max(ms);
    }
    NodeSyncFingerprint {
        max_mtime_ms,
        file_count: json_files.len(),
    }
}

/// Fragment row for ingestion (mirrors the P0 `EntityFragment` contract;
/// `cjk_text` is the space-joined bigram expansion computed by the
/// guidance-core ingestion helper).
#[derive(Debug, Clone, PartialEq)]
pub struct FragmentRecord {
    /// Fragment id (unique per file).
    pub id: String,
    /// Group id; the major carries its own id.
    pub group: Option<String>,
    /// Owning file id.
    pub file_id: String,
    /// Serialized range.
    pub range_json: String,
    /// Content kind (`text` | `image`).
    pub content_kind: String,
    /// Text content, if any.
    pub content_text: Option<String>,
    /// Space-joined CJK bigram expansion.
    pub cjk_text: String,
    /// Symbol type / name / scope / signature / doc / modifiers.
    pub symbol_type: Option<String>,
    /// Symbol type / name / scope / signature / doc / modifiers.
    pub symbol_name: Option<String>,
    /// Symbol type / name / scope / signature / doc / modifiers.
    pub scope: Option<String>,
    /// Symbol type / name / scope / signature / doc / modifiers.
    pub signature: Option<String>,
    /// Symbol type / name / scope / signature / doc / modifiers.
    pub doc: Option<String>,
    /// Symbol type / name / scope / signature / doc / modifiers.
    pub modifiers: String,
    /// Markdown heading, if any.
    pub heading: Option<String>,
    /// Heading level, if any.
    pub heading_level: Option<i64>,
    /// Fragment embedding, if any.
    pub embedding: Option<Vec<f32>>,
}

/// Lemma row for the `fragment_lemmas` L2 table.
#[derive(Debug, Clone, PartialEq)]
pub struct FragmentLemma {
    /// Owning fragment id.
    pub fragment_id: String,
    /// Lemma string.
    pub lemma: String,
    /// Parse confidence.
    pub confidence: f64,
}

/// Full fragment row for hit materialization.
#[derive(Debug, Clone, PartialEq)]
pub struct FragmentRow {
    /// Fragment id.
    pub id: String,
    /// Group id, if any.
    pub group: Option<String>,
    /// Owning file id.
    pub file_id: String,
    /// Serialized range.
    pub range_json: String,
    /// Content kind.
    pub content_kind: String,
    /// Text content, if any.
    pub content_text: Option<String>,
    /// Symbol fields.
    pub symbol_type: Option<String>,
    /// Symbol fields.
    pub symbol_name: Option<String>,
    /// Symbol fields.
    pub scope: Option<String>,
    /// Symbol fields.
    pub signature: Option<String>,
    /// Symbol fields.
    pub doc: Option<String>,
    /// Symbol fields.
    pub modifiers: String,
    /// Markdown fields.
    pub heading: Option<String>,
    /// Markdown fields.
    pub heading_level: Option<i64>,
}

impl FragmentRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            group: row.get(1)?,
            file_id: row.get(2)?,
            range_json: row.get(3)?,
            content_kind: row.get(4)?,
            content_text: row.get(5)?,
            symbol_type: row.get(6)?,
            symbol_name: row.get(7)?,
            scope: row.get(8)?,
            signature: row.get(9)?,
            doc: row.get(10)?,
            modifiers: row.get(11)?,
            heading: row.get(12)?,
            heading_level: row.get(13)?,
        })
    }
}

/// Recall filter pushed into fragment queries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FragmentFilter {
    /// File allow-list (`None` = no path selection; `Some([])` = no files).
    pub file_ids: Option<Vec<String>>,
    /// Group allow-list (force-track targeting).
    pub group_ids: Vec<String>,
    /// Symbol-name allow-list.
    pub symbol_names: Vec<String>,
    /// Symbol-type allow-list.
    pub symbol_types: Vec<String>,
    /// Modification-time window (ms epoch).
    pub modified_after: Option<i64>,
    /// Modification-time window (ms epoch).
    pub modified_before: Option<i64>,
}

/// One ranked recall hit (rank assignment stays with the recall loop).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredFragment {
    /// Fragment id.
    pub id: String,
    /// Owning file id.
    pub file_id: String,
    /// Raw score (higher is better; scale varies by axis).
    pub score: f64,
}

/// One fragment vector row: id, file id, legacy FP32 blob, int8 blob.
type FragmentVectorRow = (String, String, Option<Vec<u8>>, Option<Vec<u8>>);

/// Shared pushdown: file allow-list, symbol allow-lists, mtime window.
/// `frag_alias`/`file_alias` qualify the joined `fragments`/`files`.
fn append_fragment_filter(
    sql: &mut String,
    values: &mut Vec<rusqlite::types::Value>,
    filter: &FragmentFilter,
    param: &mut usize,
) {
    use rusqlite::types::Value;
    let mut push_ids = |column: &str, ids: &[String]| {
        if ids.is_empty() {
            sql.push_str(" AND 1 = 0");
            return;
        }
        let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", *param + i)).collect();
        let _ = write!(sql, " AND {column} IN ({})", placeholders.join(","));
        values.extend(ids.iter().map(|id| Value::Text(id.clone())));
        *param += ids.len();
    };
    if let Some(ids) = &filter.file_ids {
        push_ids("f.file_id", ids);
    }
    if !filter.group_ids.is_empty() {
        push_ids("f.grp", &filter.group_ids);
    }
    if !filter.symbol_names.is_empty() {
        push_ids("f.symbol_name", &filter.symbol_names);
    }
    if !filter.symbol_types.is_empty() {
        push_ids("f.symbol_type", &filter.symbol_types);
    }
    if let Some(after) = filter.modified_after {
        let _ = write!(sql, " AND fl.last_modified_time >= ?{param}");
        values.push(Value::Integer(after));
        *param += 1;
    }
    if let Some(before) = filter.modified_before {
        let _ = write!(sql, " AND fl.last_modified_time <= ?{param}");
        values.push(Value::Integer(before));
        *param += 1;
    }
}

/// Build the FTS5 `MATCH` expression for a query: ASCII tokens hit the
/// text/symbol columns, CJK runs hit the bigram column (P0 `cjk-bigram`
/// contract). Returns `None` when nothing is indexable — callers return
/// empty (never match-all).
#[must_use]
pub fn build_fts_match(query: &str) -> Option<String> {
    fn quote(token: &str) -> String {
        format!("\"{}\"", token.replace('"', "\"\""))
    }
    let mut ascii: Vec<String> = Vec::new();
    for token in query.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let lower = token.to_lowercase();
        if !ascii.contains(&lower) {
            ascii.push(lower);
        }
    }
    let bigrams = common_core::string::cjk_bigrams(query);
    let mut parts = Vec::new();
    if !ascii.is_empty() {
        let or: Vec<String> = ascii.iter().map(|t| quote(t)).collect();
        parts.push(format!("{{content_text symbol_name}} : ({})", or.join(" OR ")));
    }
    if !bigrams.is_empty() {
        let or: Vec<String> = bigrams.iter().map(|b| quote(b)).collect();
        parts.push(format!("cjk_text : ({})", or.join(" OR ")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" OR "))
    }
}

#[cfg(test)]
#[path = "../tests/error_hops.rs"]
mod error_hops;

#[cfg(test)]
#[path = "../tests/file_status.rs"]
mod file_status;

#[cfg(test)]
#[path = "../tests/fragments.rs"]
mod fragments;

#[cfg(test)]
#[path = "../tests/graph_edges.rs"]
mod graph_edges;

#[cfg(test)]
#[path = "../tests/node_sync.rs"]
mod node_sync;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> GuidanceDb {
        GuidanceDb::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn test_insert_and_count() {
        let db = make_db();
        let id = db
            .insert_node(
                "hello",
                "src/test.zig",
                Some("fn hello() void"),
                Some("Says hello"),
                "test",
                "zig",
                None,
            )
            .expect("insert");
        assert!(id > 0);
        assert_eq!(db.get_node_count().expect("count"), 1);
    }

    #[test]
    fn test_keyword_search() {
        let db = make_db();
        db.insert_node(
            "greet",
            "src/test.zig",
            Some("fn greet() void"),
            Some("Greets the user"),
            "test",
            "zig",
            None,
        )
        .expect("insert");
        db.insert_node(
            "add",
            "src/math.zig",
            Some("fn add() i32"),
            Some("Adds numbers"),
            "math",
            "zig",
            None,
        )
        .expect("insert");

        let results = db.keyword_search("greet").expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "greet");
    }

    #[test]
    fn test_vector_search() {
        let db = make_db();
        let emb1: Vec<f32> = (0..4).map(|i| i as f32).collect();
        let emb2: Vec<f32> = (0..4).map(|i| (i + 10) as f32).collect();

        db.insert_node("a", "src/a.zig", None, None, "test", "zig", Some(&emb1))
            .expect("insert");
        db.insert_node("b", "src/b.zig", None, None, "test", "zig", Some(&emb2))
            .expect("insert");

        let query = vec![0.5, 1.5, 2.5, 3.5];
        let results = db.vector_search(&query, 2).expect("search");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "a");
    }

    #[test]
    fn test_empty_search() {
        let db = make_db();
        let results = db.keyword_search("nonexistent").expect("search");
        assert!(results.is_empty());
    }

    #[test]
    fn brute_force_order_truncation_and_tie_winner() {
        // Characterization (M3b): locks the brute-force tail's order,
        // k-truncation, and tie-break so the P2 migration must preserve them.
        // "twin-a"/"twin-b" carry identical embeddings (a contested tie);
        // the sort is stable over rowid-ordered rows, so insertion order wins.
        let db = make_db();
        let near = vec![1.0f32, 0.0, 0.0, 0.0];
        let far = vec![0.0f32, 1.0, 0.0, 0.0];
        db.insert_node("twin-a", "s", None, None, "m", "l", Some(&near))
            .expect("insert");
        db.insert_node("twin-b", "s", None, None, "m", "l", Some(&near))
            .expect("insert");
        db.insert_node("far", "s", None, None, "m", "l", Some(&far))
            .expect("insert");

        let all = db.bruteforce_vector_search(&near, 10).expect("search");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].name, "twin-a", "tie: insertion order wins");
        assert_eq!(all[1].name, "twin-b");
        assert_eq!(all[2].name, "far");
        assert!(all[0].similarity >= all[1].similarity);
        assert!(all[1].similarity >= all[2].similarity);

        // k truncation keeps the head in order.
        let top2 = db.bruteforce_vector_search(&near, 2).expect("search");
        assert_eq!(
            top2.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["twin-a", "twin-b"]
        );
        // k == 0 → empty.
        assert!(db.bruteforce_vector_search(&near, 0).expect("search").is_empty());
    }

    #[test]
    fn test_hybrid_search() {
        let db = make_db();
        let emb = vec![0.1, 0.2, 0.3, 0.4];

        db.insert_node(
            "hello_fn",
            "src/test.zig",
            Some("fn hello() void"),
            Some("Says hello"),
            "test",
            "zig",
            Some(&emb),
        )
        .expect("insert");

        let results = db
            .hybrid_search("hello", Some(&emb), 5)
            .expect("hybrid search");
        assert!(!results.is_empty());
    }
}
