use std::path::Path;

use crate::hnsw::{AdaptiveHnsw, HnswIndex};
use crate::math as vector;
use fluent_db::error::DbError;
use fluent_db::store::SqliteStore;
use rusqlite::params;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum VectorDbError {
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

/// Generic node-document store: embedded JSON documents with change-gated
/// sync plus vector/keyword/hybrid search. A generic primitive any consumer
/// may use — it creates only the `nodes`/`embedding_cache` tables, never
/// the guidance P1 fragment schema (see `GuidanceDb` for the full store).
pub struct VectorDocStore {
    store: SqliteStore,
    hnsw: HnswIndex,
}

impl VectorDocStore {
    pub fn open(path: &Path) -> Result<Self, VectorDbError> {
        let store = SqliteStore::open(path)?;
        // Derived index with a rebuild path: WAL + NORMAL stays process-crash
        // consistent without one fsync per per-file commit. Sidecars are truth.
        store.with_conn(|conn| {
            conn.execute_batch("PRAGMA synchronous=NORMAL;")
                .map_err(DbError::from)?;
            Ok(())
        })?;
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
        crate::db::schema::init_generic_schema(&self.store)
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
                "INSERT INTO nodes (name, source, signature, comment, module, language, embedding)
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
    pub(crate) fn hnsw_insert(&self, node_id: i64, embedding: &[f32]) {
        self.hnsw.insert(node_id, embedding);
    }

    /// Rebuild the HNSW index from all embedded nodes in the database.
    pub fn rebuild_hnsw(&self) -> Result<usize, VectorDbError> {
        let rows = self.store.query_rows(
            "SELECT id, embedding FROM nodes WHERE embedding IS NOT NULL",
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
    pub(crate) fn hnsw_search(&self, query_vec: &[f32], k: usize) -> Option<Vec<SearchResult>> {
        // Single adaptive-dispatch policy ([B] cost/recall only — never a
        // confidence/verification gate): probe HNSW iff built and above
        // threshold. `None` takes the call-site brute-force fallback in
        // `vector_search`, whose fill-to-`k` top-up and tail are unchanged.
        // The connection is touched only after the index guard releases,
        // preserving the `index → map → conn` ordering.
        let embedded = self.get_embedding_count().ok()? as usize;
        if !AdaptiveHnsw::default().dispatch(self.hnsw.is_built(), embedded) {
            return None;
        }
        // The shared lookup short-circuits malformed probes (empty query,
        // `k == 0`) that would otherwise reach the distance kernel's dim
        // assertion, and resolves hits against the traversed generation (a
        // concurrent rebuild degrades to `None`, i.e. the brute-force path).
        let resolved = crate::hnsw::hnsw_lookup(&self.hnsw, query_vec, k)?;

        let mut results = Vec::with_capacity(resolved.len());
        for (node_id, distance) in resolved {
            // Convert cosine distance to similarity via the canonical kernel.
            let similarity = vector::distance_to_similarity(distance);

            if let Ok(Some(row)) = self.store.query_row(
                "SELECT name, source, signature FROM nodes WHERE id = ?1",
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
    pub(crate) fn bruteforce_vector_search(
        &self,
        query_vec: &[f32],
        k: usize,
    ) -> Result<Vec<SearchResult>, VectorDbError> {
        let rows = self.store.query_rows(
            "SELECT id, name, source, signature, embedding FROM nodes WHERE embedding IS NOT NULL",
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
            "SELECT id, name, source, signature FROM nodes
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
             FROM nodes
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
        // canonical `crate::math::rrf_merge`.
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
            .query_row("SELECT COUNT(*) FROM nodes", &[], |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(count.unwrap_or(0))
    }

    pub fn get_embedding_count(&self) -> Result<i64, VectorDbError> {
        let count = self.store.query_row(
            "SELECT COUNT(*) FROM nodes WHERE embedding IS NOT NULL",
            &[],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.unwrap_or(0))
    }

    /// The shared connection handle (also hosts the guidance extension
    /// tables on a full `GuidanceDb`; generic callers use the typed
    /// methods above instead of SQL).
    #[must_use]
    pub fn sqlite_store(&self) -> &SqliteStore {
        &self.store
    }

    /// Check if the HNSW index is built.
    pub fn has_hnsw(&self) -> bool {
        self.hnsw.is_built()
    }

    /// Get the number of points in the HNSW index.
    pub fn hnsw_len(&self) -> usize {
        self.hnsw.len()
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
}

#[cfg(test)]
#[path = "../tests/vector_store.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/hybrid_calibration.rs"]
mod hybrid_calibration_tests;
