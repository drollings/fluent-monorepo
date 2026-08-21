//! `ToolRetriever` — the candidate shortlister for over-cap tool catalogues.
//!
//! Roadmap design decision 4: at `candidates_per_rung` or fewer every
//! candidate is grammar-rendered and reachable (O(1), no index needed); on
//! overflow the set is reduced to ≤ `candidates_per_rung` via the HNSW tool
//! index. **BM25 is excluded by design**, and Needle's own internal top-5
//! contrastive head is never used for routing.
//!
//! This module owns the trait, the identity/`Noop` implementation
//! ([`IdentityToolRetriever`]), and the production shortlister
//! ([`HnswToolRetriever`]).
//!
//! The trait works over the enriched [`NeedleRouteSchema`] candidates (the
//! typed single-source-of-truth form, not rendered JSON), so shortlisting
//! happens before the tools are grammar-rendered.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};

use common_core::sync::{lock_read, lock_write};
use fluent_db::error::DbError;
use fluent_db::hnsw::HnswIndex;
use fluent_db::store::SqliteStore;
use fluent_db::vector::{cosine_similarity, try_bytes_to_vec, vec_to_bytes};
use fluent_llm::EmbeddingProvider;

use crate::config::NeedleRouteSchema;

/// Shortlist a candidate tool set to at most `k` schemas for a query.
///
/// The returned schemas are a subset of `candidates` in
/// most-relevant-first order. Implementations must be deterministic and must
/// never fabricate a schema — only ever select from the given candidates.
pub trait ToolRetriever: Send + Sync {
    /// Reduce `candidates` to at most `k` schemas most relevant to `query`.
    fn shortlist(&self, query: &str, candidates: &[NeedleRouteSchema], k: usize)
        -> Vec<NeedleRouteSchema>;
}

/// Identity shortlister — returns the candidates unchanged.
///
/// Used when the catalogue already fits the rung cap (no index needed) or when
/// the shortlister engine is unavailable and the caller prefers pass-all over
/// an arbitrary truncation.
#[derive(Debug, Default)]
pub struct IdentityToolRetriever;

impl ToolRetriever for IdentityToolRetriever {
    fn shortlist(
        &self,
        _query: &str,
        candidates: &[NeedleRouteSchema],
        k: usize,
    ) -> Vec<NeedleRouteSchema> {
        candidates.iter().take(k).cloned().collect()
    }
}

/// HNSW-backed candidate shortlister (Milestone 5, design decision 4).
///
/// One vector per schema is embedded from the schema's document text
/// (description + name + examples + intents) through the configured
/// [`EmbeddingProvider`], persisted at `shortlist.index_path` when configured
/// (idempotent upsert — repeated boots reuse vectors), and searched via a
/// cosine [`HnswIndex`]. `shortlist` embeds the query, returns the top `k`
/// hits above `min_score` cosine similarity, and never fabricates a schema —
/// only ever selects from the given candidates.
///
/// **Degradation is pass-all, never a silent truncation**: when the index
/// cannot be built or the query cannot be embedded (missing embedder, empty
/// vector, unreachable endpoint) the full candidate list is returned so the
/// caller detects the `candidates_per_rung` overflow and falls through to the
/// classifier (design decision 4). A half-built index must not serve, so the
/// build is all-or-nothing.
pub struct HnswToolRetriever {
    /// The embed seam for schema documents and queries.
    embedder: Arc<dyn EmbeddingProvider>,
    /// Persisted index path. `None` keeps the index in-memory only.
    index_path: Option<String>,
    /// Cosine-similarity floor in [0,1]: hits below it are dropped.
    min_score: f64,
    /// Lazily-built index, keyed to the candidate set it was built from.
    built: RwLock<Option<Arc<ToolIndex>>>,
}

/// A built tool index: the cosine graph plus the schema-name alignment.
struct ToolIndex {
    /// The embed seam the index was built with (also used for the query).
    embedder: Arc<dyn EmbeddingProvider>,
    /// Schema name by HNSW external id (`d_id` == `ids.len()` at insert).
    ids: Vec<String>,
    /// Schema name + embedding, for the exact brute-force fallback when the
    /// approximate HNSW search under-fills a small catalogue (charts pattern).
    flat: Vec<(String, Vec<f32>)>,
    /// Cosine HNSW graph over one vector per schema.
    hnsw: HnswIndex,
}

impl HnswToolRetriever {
    /// Build a retriever over `embedder`, persisting vectors at `index_path`
    /// when `Some` (in-memory otherwise). `min_score` is the cosine-similarity
    /// floor in [0,1] below which a candidate is dropped.
    pub fn new(
        embedder: Arc<dyn EmbeddingProvider>,
        index_path: Option<String>,
        min_score: f64,
    ) -> Self {
        Self {
            embedder,
            index_path,
            min_score,
            built: RwLock::new(None),
        }
    }

    /// Build the tool index for `candidates`, reusing a prior build when the
    /// candidate set is unchanged. `None` on any failure — the caller degrades
    /// to pass-all.
    fn ensure_built(&self, candidates: &[NeedleRouteSchema]) -> Option<Arc<ToolIndex>> {
        let names: Vec<String> = candidates.iter().map(|s| s.name.clone()).collect();
        if let Some(existing) = lock_read(&self.built).as_ref() {
            if existing.ids == names {
                return Some(Arc::clone(existing));
            }
        }
        let index = self.build(candidates, &names)?;
        *lock_write(&self.built) = Some(Arc::new(index));
        Some(Arc::clone(lock_read(&self.built).as_ref()?))
    }

    /// All-or-nothing index build: embed every schema first; only when all
    /// succeed is the graph built and vectors persisted. `None` aborts cleanly
    /// (caller falls through — never a truncated tool set).
    fn build(&self, candidates: &[NeedleRouteSchema], names: &[String]) -> Option<ToolIndex> {
        let store = self.open_store();

        // Pass 1 — embed (or reuse a persisted, still-matching vector) for
        // every schema. Any failure aborts the whole build.
        let mut entries: Vec<(String, String, Vec<f32>)> = Vec::with_capacity(candidates.len());
        for schema in candidates {
            let doc = schema_doc_text(schema);
            let embedding = self.embed_one(store.as_ref(), schema, &doc)?;
            entries.push((schema.name.clone(), doc, embedding));
        }

        // Pass 2 — build the cosine graph aligned to `names` positions.
        let hnsw = HnswIndex::new();
        let mut flat: Vec<(String, Vec<f32>)> = Vec::with_capacity(entries.len());
        for (idx, (name, _, emb)) in entries.iter().enumerate() {
            hnsw.insert(idx as i64, emb);
            flat.push((name.clone(), emb.clone()));
        }

        // Pass 3 — persist (upsert) so a later boot reuses these vectors.
        if let Some(store) = store.as_ref() {
            for (name, doc, emb) in &entries {
                let _ = store.execute(
                    "INSERT INTO tool_index (name, doc_text, embedding) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(name) DO UPDATE \
                     SET doc_text = excluded.doc_text, embedding = excluded.embedding",
                    rusqlite::params![name, doc, vec_to_bytes(emb)],
                );
            }
        }

        Some(ToolIndex {
            embedder: Arc::clone(&self.embedder),
            ids: names.to_vec(),
            flat,
            hnsw,
        })
    }

    /// Open the persisted `SqliteStore` when `index_path` is configured,
    /// creating parent directories and the `tool_index` table. `None` on any
    /// failure (or no path) — the build continues in-memory.
    fn open_store(&self) -> Option<SqliteStore> {
        let path = Path::new(self.index_path.as_deref()?);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok()?;
            }
        }
        let store = SqliteStore::open(path).ok()?;
        store
            .init_schema(
                "CREATE TABLE IF NOT EXISTS tool_index (\
                 name TEXT PRIMARY KEY, doc_text TEXT NOT NULL, embedding BLOB NOT NULL);",
            )
            .ok()?;
        Some(store)
    }

    /// Embed one schema's document text, reusing a persisted vector when the
    /// cached doc still matches. `None` aborts the whole build (the caller
    /// degrades to pass-all).
    fn embed_one(
        &self,
        store: Option<&SqliteStore>,
        schema: &NeedleRouteSchema,
        doc: &str,
    ) -> Option<Vec<f32>> {
        if let Some(store) = store {
            match store.with_conn(|conn| {
                load_cached_embedding(conn, &schema.name, doc)
                    .map_err(|e| DbError::Other(e.to_string()))
            }) {
                Ok(Some(v)) => return Some(v),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(target: "router.needle.retriever", schema = %schema.name, error = %e, "tool index cache read failed — HNSW shortlist disabled (pass-all)");
                    return None;
                }
            }
        }
        let v = self.embedder.embed(doc).ok()?;
        if v.is_empty() {
            tracing::warn!(target: "router.needle.retriever", schema = %schema.name, "embedder returned empty vector — HNSW shortlist disabled (pass-all)");
            return None;
        }
        Some(v)
    }
}

impl ToolRetriever for HnswToolRetriever {
    fn shortlist(
        &self,
        query: &str,
        candidates: &[NeedleRouteSchema],
        k: usize,
    ) -> Vec<NeedleRouteSchema> {
        if k == 0 || candidates.is_empty() {
            return Vec::new();
        }
        let Some(index) = self.ensure_built(candidates) else {
            tracing::warn!(target: "router.needle.retriever", "tool index unavailable — passing full candidate set through (caller falls through on overflow)");
            return candidates.to_vec();
        };
        let query_embedding = match index.embedder.embed(query) {
            Ok(v) if !v.is_empty() && !query.trim().is_empty() => v,
            Ok(_) => {
                tracing::warn!(target: "router.needle.retriever", "query embedding empty — HNSW shortlist disabled (pass-all)");
                return candidates.to_vec();
            }
            Err(e) => {
                tracing::warn!(target: "router.needle.retriever", error = %e, "query embedding failed — HNSW shortlist disabled (pass-all)");
                return candidates.to_vec();
            }
        };
        let by_name: HashMap<&str, &NeedleRouteSchema> = candidates
            .iter()
            .map(|s| (s.name.as_str(), s))
            .collect();
        // Search with a wide beam (`ef` == the search width in the shared
        // `HnswIndex::search`), then dedup: hnsw_rs can return the same node
        // from different entry points, and with ef == k a small catalogue would
        // come back under-filled.
        let wide = k.saturating_mul(8).max(k + 1);
        let mut seen: HashSet<&str> = HashSet::new();
        let mut selected: Vec<&NeedleRouteSchema> = Vec::new();
        for (d_id, distance) in index.hnsw.search(&query_embedding, wide) {
            let Some(name) = index.ids.get(d_id).map(String::as_str) else {
                continue;
            };
            push_if_qualified(
                &mut seen,
                &by_name,
                self.min_score,
                name,
                (1.0 - distance).max(0.0),
                &mut selected,
            );
            if selected.len() >= k {
                break;
            }
        }
        // Exact top-up: the approximate HNSW search can under-fill a small
        // catalogue (duplicate hits, a barely-connected graph). Fill the
        // remaining slots from the exact brute-force ranking (charts pattern)
        // so the rung always sees as many qualified tools as the cap allows.
        if selected.len() < k {
            let mut topup: Vec<(&str, f32)> = index
                .flat
                .iter()
                .map(|(name, emb)| {
                    let similarity =
                        (1.0 - cosine_similarity(&query_embedding, emb)).max(0.0);
                    (name.as_str(), similarity)
                })
                .filter(|(name, _)| !seen.contains(name))
                .collect();
            topup.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (name, similarity) in topup {
                push_if_qualified(
                    &mut seen,
                    &by_name,
                    self.min_score,
                    name,
                    similarity,
                    &mut selected,
                );
                if selected.len() >= k {
                    break;
                }
            }
        }
        selected.into_iter().cloned().collect()
    }
}

/// Dedup-and-threshold a candidate: insert `name` into `seen`, and when it is
/// new and at/above `min_score`, push its schema onto `out`.
fn push_if_qualified<'a>(
    seen: &mut HashSet<&'a str>,
    by_name: &HashMap<&'a str, &'a NeedleRouteSchema>,
    min_score: f64,
    name: &'a str,
    similarity: f32,
    out: &mut Vec<&'a NeedleRouteSchema>,
) {
    if seen.insert(name) && f64::from(similarity) >= min_score {
        if let Some(schema) = by_name.get(name) {
            out.push(*schema);
        }
    }
}

/// The indexed document text for a schema: description + name + examples +
/// intents. One vector per schema.
fn schema_doc_text(schema: &NeedleRouteSchema) -> String {
    let mut parts: Vec<&str> = Vec::new();
    parts.push(&schema.name);
    parts.push(&schema.description);
    parts.extend(schema.examples.iter().map(String::as_str));
    parts.extend(schema.intents.iter().map(String::as_str));
    parts.join(" ")
}

/// Load a cached embedding for `name` whose document text still matches.
fn load_cached_embedding(
    conn: &rusqlite::Connection,
    name: &str,
    doc: &str,
) -> Result<Option<Vec<f32>>, DbError> {
    let row = fluent_db::query::query_row(
        conn,
        "SELECT doc_text, embedding FROM tool_index WHERE name = ?1",
        rusqlite::params![name],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
    )?;
    let Some((cached_doc, blob)) = row else {
        return Ok(None);
    };
    if cached_doc != doc {
        return Ok(None);
    }
    Ok(try_bytes_to_vec(&blob).filter(|v| !v.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_stubs::HashEmbedder;
    use fluent_llm::EmbeddingError;

    fn schema(name: &str, description: &str, examples: &[&str]) -> NeedleRouteSchema {
        NeedleRouteSchema {
            name: name.into(),
            description: description.into(),
            examples: examples.iter().map(|s| s.to_string()).collect(),
            parameters: serde_json::json!({}),
            intents: vec![],
            output_template: None,
            general: false,
        }
    }

    fn base_schema(name: &str) -> NeedleRouteSchema {
        schema(name, &format!("{name} route"), &[])
    }

    /// An embedder that always fails — used to exercise the pass-all
    /// degradation path.
    struct FailingEmbedder;

    impl EmbeddingProvider for FailingEmbedder {
        fn name(&self) -> &'static str {
            "test-failing"
        }

        fn dimensions(&self) -> u32 {
            0
        }

        fn embed(&self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::RequestFailed("boom".into()))
        }

        fn embed_batch(&self, _texts: &[&str]) -> Result<fluent_llm::BatchEmbedding, EmbeddingError> {
            Err(EmbeddingError::RequestFailed("boom".into()))
        }
    }

    fn retriever(dims: usize, min_score: f64) -> HnswToolRetriever {
        HnswToolRetriever::new(Arc::new(HashEmbedder::new(dims)), None, min_score)
    }

    #[test]
    fn empty_or_blank_query_degrades_to_pass_all() {
        let r = retriever(64, 0.0);
        let candidates = vec![base_schema("a"), base_schema("b")];
        // A blank query cannot be embedded meaningfully — pass the full set
        // through so the caller's overflow guard falls through (never empty
        // the rung on a defensible query).
        assert_eq!(r.shortlist("", &candidates, 5).len(), 2);
        assert_eq!(r.shortlist("   ", &candidates, 5).len(), 2);
        assert!(r.shortlist("q", &candidates, 0).is_empty());
        assert!(r.shortlist("q", &[], 5).is_empty());
    }

    #[test]
    fn shortlist_returns_most_relevant_first_within_k() {
        let r = retriever(128, 0.0);
        let weather = schema("weather", "weather forecast", &["what is the weather today"]);
        let math = schema("math", "arithmetic", &["compute 2 plus 2"]);
        let chat = schema("chat", "general conversation", &["tell me a joke"]);
        let candidates = vec![weather, math, chat];
        let short = r.shortlist("what is the weather today in berlin", &candidates, 2);
        assert_eq!(short.len(), 2, "shortlist must respect k");
        assert_eq!(short[0].name, "weather", "most relevant candidate must lead");
        assert!(
            short.iter().all(|s| candidates.iter().any(|c| c.name == s.name)),
            "shortlist must never fabricate a schema"
        );
    }

    #[test]
    fn shortlist_respects_min_score_floor() {
        let r = retriever(128, 0.999);
        let candidates = vec![schema("weather", "weather forecast", &["what is the weather today"])];
        // Unrelated query → the top hit still scores below the 0.999 floor.
        let short = r.shortlist("unrelated query about nothing at all", &candidates, 5);
        assert!(short.is_empty(), "below-min_score candidates must be dropped");
    }

    #[test]
    fn shortlist_degrades_to_pass_all_when_embedding_fails() {
        let r = HnswToolRetriever::new(Arc::new(FailingEmbedder), None, 0.0);
        let candidates = vec![base_schema("a"), base_schema("b")];
        let short = r.shortlist("anything", &candidates, 5);
        assert_eq!(short.len(), 2, "failure must pass the full set through, never truncate");
        assert_eq!(short[0].name, "a");
        assert_eq!(short[1].name, "b");
    }

    #[test]
    fn shortlist_is_deterministic() {
        let r = retriever(128, 0.0);
        let candidates = vec![
            base_schema("alpha"),
            base_schema("beta"),
            base_schema("gamma"),
            base_schema("delta"),
            base_schema("epsilon"),
        ];
        let q = "beta route please";
        let first = r.shortlist(q, &candidates, 3);
        let second = r.shortlist(q, &candidates, 3);
        let names: Vec<String> = first.iter().map(|s| s.name.clone()).collect();
        assert_eq!(
            names,
            second.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            "same query + candidates must yield the same order"
        );
        assert!(names.len() <= 3);
    }

    #[test]
    fn index_rebuilds_when_candidates_change() {
        let r = retriever(128, 0.0);
        let a = vec![base_schema("aaa"), base_schema("bbb")];
        let q = "aaa";
        let first = r.shortlist(q, &a, 2);
        assert_eq!(first.len(), 2);
        // A second, larger candidate set triggers a rebuild.
        let b = vec![base_schema("aaa"), base_schema("bbb"), base_schema("ccc")];
        let second = r.shortlist(q, &b, 2);
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].name, "aaa");
    }

    #[test]
    fn index_persists_and_reuses_at_index_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tool_index.sqlite").display().to_string();
        let r = HnswToolRetriever::new(Arc::new(HashEmbedder::new(128)), Some(path.clone()), 0.0);
        let candidates = vec![schema("weather", "weather forecast", &["what is the weather today"])];
        let short = r.shortlist("what is the weather today", &candidates, 1);
        assert_eq!(short.len(), 1);
        assert_eq!(short[0].name, "weather");
        assert!(
            dir.path().join("tool_index.sqlite").exists(),
            "vectors must be persisted at index_path"
        );
    }

    #[test]
    fn identity_returns_at_most_k_in_order() {
        let r = IdentityToolRetriever;
        let candidates = vec![base_schema("a"), base_schema("b"), base_schema("c")];
        let short = r.shortlist("q", &candidates, 2);
        assert_eq!(short.len(), 2);
        assert_eq!(short[0].name, "a");
        assert_eq!(short[1].name, "b");
        assert!(short.len() <= candidates.len());
    }

    #[test]
    fn identity_never_exceeds_candidates() {
        let r = IdentityToolRetriever;
        let candidates = vec![base_schema("a"), base_schema("b")];
        let short = r.shortlist("q", &candidates, 5);
        assert_eq!(short.len(), 2);
    }
}