//! Capability brief storage: one derived row per doc chunk plus its
//! own lexical index and lemma postings.
//!
//! Briefs are generated commentary, never source: they live in a
//! separate table and FTS index so source citations stay source-only.
//! `fragments_fts` remains source-only; `capabilities_fts` is the only
//! brief-text lexical index, joined back to `capabilities` by rowid.

use super::VectorDbError;

/// One stored capability brief: a normalized doc chunk plus the
/// model-written purpose and its SVO normalization.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityRecord {
    /// Stable chunk id (`source_path#anchor`).
    pub id: String,
    /// Workspace identity (root directory name).
    pub project: String,
    /// Owning crate from the path decomposition (`unknown` fallback).
    pub crate_name: String,
    /// Doc domain (first `doc/` segment, crate name, or `meta`).
    pub domain: String,
    /// Card title from the external corpus (empty for legacy rows).
    pub title: String,
    /// Model-written purpose brief.
    pub purpose: String,
    /// SVO predicate lemma (empty when the brief yields none).
    pub predicate: String,
    /// JSON array of SVO role lemmas.
    pub roles_json: String,
    /// JSON array of corpus keywords.
    pub keywords_json: String,
    /// JSON array of workspace-relative code paths the card points at.
    pub code_paths_json: String,
    /// JSON array of symbols the card points at.
    pub symbols_json: String,
    /// Workspace-relative doc path.
    pub source_path: String,
    /// Section anchor within the doc.
    pub anchor: String,
    /// Blake3 of the chunk text (incremental cache key input).
    pub content_hash: String,
    /// Model identity that produced the brief (`external` for imports).
    pub model: String,
    /// Imported-card schema version (was: generation prompt version).
    pub prompt_version: i64,
    /// Where the card came from (`external:<manifest>` or `calibration`).
    pub origin: String,
    /// Blake3 of the imported corpus file (empty for legacy rows).
    pub corpus_hash: String,
    /// Derivation status (`ready` | `held` | `gap`).
    pub status: String,
    /// Failure or hold reason (`None` for ready rows).
    pub gap_reason: Option<String>,
    /// Generation time (ms epoch).
    pub generated_at: i64,
}

/// Imported-card schema version stored in `prompt_version`.
pub const CAPABILITY_SCHEMA_VERSION: i64 = 7;

impl CapabilityRecord {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            project: row.get(1)?,
            crate_name: row.get(2)?,
            domain: row.get(3)?,
            title: row.get(4)?,
            purpose: row.get(5)?,
            predicate: row.get(6)?,
            roles_json: row.get(7)?,
            keywords_json: row.get(8)?,
            code_paths_json: row.get(9)?,
            symbols_json: row.get(10)?,
            source_path: row.get(11)?,
            anchor: row.get(12)?,
            content_hash: row.get(13)?,
            model: row.get(14)?,
            prompt_version: row.get(15)?,
            origin: row.get(16)?,
            corpus_hash: row.get(17)?,
            status: row.get(18)?,
            gap_reason: row.get(19)?,
            generated_at: row.get(20)?,
        })
    }
}

/// One ranked capability hit (rank assignment stays with the recall loop).
#[derive(Debug, Clone, PartialEq)]
/// One ranked capability hit (rank assignment stays with the recall loop).
pub struct ScoredCapability {
    /// Capability id.
    pub id: String,
    /// Raw score (higher is better; scale varies by axis).
    pub score: f64,
}

/// Lemma row for the `capability_lemmas` L2 table.
#[derive(Debug, Clone, PartialEq)]
/// Lemma row for the `capability_lemmas` L2 table.
pub struct CapabilityLemma {
    /// Owning capability id.
    pub capability_id: String,
    /// Lemma string.
    pub lemma: String,
    /// Parse confidence.
    pub confidence: f64,
}

/// Build the capability FTS `MATCH` expression: ASCII tokens hit the
/// title, purpose, predicate, and keyword columns. Returns `None`
/// when nothing is indexable — callers return empty (never match-all).
#[must_use]
pub fn build_capability_fts_match(query: &str) -> Option<String> {
    use common_core::string::{fts_match_tokens, fts_quote};
    let ascii = fts_match_tokens(query);
    if ascii.is_empty() {
        return None;
    }
    let or: Vec<String> = ascii.iter().map(|t| fts_quote(t)).collect();
    Some(format!(
        "{{title purpose predicate keywords_json}} : ({})",
        or.join(" OR ")
    ))
}

const CAPABILITY_COLUMNS: &str = "id, project, crate_name, domain, title, purpose,
        predicate, roles_json, keywords_json, code_paths_json, symbols_json,
        source_path, anchor, content_hash, model, prompt_version,
        origin, corpus_hash, status, gap_reason, generated_at";

impl super::GuidanceSearchDb {
    /// Insert or replace capability rows without touching fragments.
    pub fn replace_capabilities(&self, rows: &[CapabilityRecord]) -> Result<usize, VectorDbError> {
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                for row in rows {
                    fluent_db::query::execute(
                        tx,
                        "INSERT OR REPLACE INTO capabilities
                         (id, project, crate_name, domain, title, purpose,
                          predicate, roles_json, keywords_json, code_paths_json,
                          symbols_json, source_path, anchor, content_hash, model,
                          prompt_version, origin, corpus_hash, status, gap_reason,
                          generated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                                 ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                        rusqlite::params![
                            row.id,
                            row.project,
                            row.crate_name,
                            row.domain,
                            row.title,
                            row.purpose,
                            row.predicate,
                            row.roles_json,
                            row.keywords_json,
                            row.code_paths_json,
                            row.symbols_json,
                            row.source_path,
                            row.anchor,
                            row.content_hash,
                            row.model,
                            row.prompt_version,
                            row.origin,
                            row.corpus_hash,
                            row.status,
                            row.gap_reason,
                            row.generated_at,
                        ],
                    )?;
                }
                Ok(rows.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Capability rows for one source doc (the incremental cache check).
    pub fn capabilities_for_source(
        &self,
        source_path: &str,
    ) -> Result<Vec<CapabilityRecord>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                &format!("SELECT {CAPABILITY_COLUMNS} FROM capabilities WHERE source_path = ?1"),
                &[&source_path],
                CapabilityRecord::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Capability rows for explicit ids (the branch-1 candidate
    /// fetch). Only `ready` rows participate: held or gap rows never
    /// fire, however they read.
    pub fn capabilities_for_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<CapabilityRecord>, VectorDbError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT {CAPABILITY_COLUMNS} FROM capabilities
             WHERE id IN ({placeholders}) AND status = 'ready'"
        );
        let values: Vec<rusqlite::types::Value> = ids
            .iter()
            .map(|id| rusqlite::types::Value::Text(id.clone()))
            .collect();
        self.docs
            .sqlite_store()
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(
                    conn,
                    &sql,
                    values,
                    CapabilityRecord::from_row,
                )
            })
            .map_err(VectorDbError::from)
    }

    /// Ready brief count (the branch-1 candidate pool size). The read
    /// path uses it to distinguish "tier unavailable" (zero rows) from
    /// "no match" (rows exist, none ranked) — an explicit absence, never
    /// a silent skip. Same `status = 'ready'` predicate the branch
    /// recalls, so the count and the recall can never disagree.
    pub fn capability_ready_count(&self) -> Result<i64, VectorDbError> {
        let count = self
            .docs
            .sqlite_store()
            .query_row(
                "SELECT COUNT(*) FROM capabilities WHERE status = 'ready'",
                &[],
                |row| row.get::<_, i64>(0),
            )
            .map_err(VectorDbError::from)?;
        Ok(count.unwrap_or(0))
    }

    /// Stored capability ids for one import origin (the removal sweep).
    pub fn capability_ids_with_origin(&self, origin: &str) -> Result<Vec<String>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                "SELECT id FROM capabilities WHERE origin = ?1 ORDER BY id ASC",
                &[&origin],
                |row| row.get(0),
            )
            .map_err(VectorDbError::from)
    }

    /// Delete capability rows by id (manifest removals).
    pub fn delete_capabilities(&self, ids: &[String]) -> Result<usize, VectorDbError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!("DELETE FROM capabilities WHERE id IN ({placeholders})");
        let values: Vec<rusqlite::types::Value> = ids
            .iter()
            .map(|id| rusqlite::types::Value::Text(id.clone()))
            .collect();
        self.docs
            .sqlite_store()
            .with_conn(|conn| {
                conn.execute(&sql, rusqlite::params_from_iter(values))
                    .map_err(fluent_db::error::DbError::from)
            })
            .map_err(VectorDbError::from)?;
        Ok(ids.len())
    }

    /// Ranked lexical recall over the brief index. Scores are `-bm25`
    /// (higher is better). Only `ready` rows participate: held or gap
    /// rows never fire, however they read. An unindexable query yields
    /// no rows (never match-all).
    pub fn search_capabilities(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ScoredCapability>, VectorDbError> {
        let Some(match_expr) = build_capability_fts_match(query) else {
            return Ok(Vec::new());
        };
        let sql = String::from(
            "SELECT c.id, bm25(capabilities_fts) AS r
             FROM capabilities_fts
             JOIN capabilities c ON c.rowid = capabilities_fts.rowid
             WHERE capabilities_fts MATCH ?1 AND c.status = 'ready'
             ORDER BY r ASC LIMIT ?2",
        );
        self.docs
            .sqlite_store()
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(
                    conn,
                    &sql,
                    vec![
                        rusqlite::types::Value::Text(match_expr),
                        rusqlite::types::Value::Integer(limit as i64),
                    ],
                    |row| {
                        let rank_value: f64 = row.get(1)?;
                        Ok(ScoredCapability {
                            id: row.get(0)?,
                            score: -rank_value,
                        })
                    },
                )
            })
            .map_err(VectorDbError::from)
    }

    /// Insert or replace lemma rows without touching capabilities.
    pub fn insert_capability_lemmas(
        &self,
        lemmas: &[CapabilityLemma],
    ) -> Result<usize, VectorDbError> {
        self.docs.sqlite_store()
            .transaction(|tx| {
                let mut term_of: std::collections::HashMap<String, i64> =
                    std::collections::HashMap::new();
                let mut insert = tx.prepare_cached(
                    "INSERT OR REPLACE INTO capability_lemmas (capability_rowid, lemma_id, confidence)
                     VALUES (?1, ?2, ?3)",
                )?;
                for lemma in lemmas {
                    let capability_rowid: Option<i64> = fluent_db::query::query_row(
                        tx,
                        "SELECT rowid FROM capabilities WHERE id = ?1",
                        rusqlite::params![lemma.capability_id],
                        |row| row.get(0),
                    )?;
                    let Some(capability_rowid) = capability_rowid else {
                        continue;
                    };
                    let lemma_id = Self::lemma_term_id(tx, &mut term_of, &lemma.lemma)?;
                    insert
                        .execute(rusqlite::params![capability_rowid, lemma_id, lemma.confidence])
                        .map_err(fluent_db::error::DbError::from)?;
                }
                Ok(lemmas.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Capability ids carrying any of the given lemmas (stable rowid
    /// order). Capability postings are unique per `(rowid, lemma)`, so
    /// `COUNT(*)` is the number of distinct query lemmas matched.
    pub fn capabilities_for_lemmas(
        &self,
        lemmas: &[String],
        limit: usize,
    ) -> Result<Vec<String>, VectorDbError> {
        if lemmas.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = lemmas.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        // Capability ids are workspace-relative (`source#anchor`), so
        // id order is content-stable across indexes of one tree.
        let sql = format!(
            "SELECT c.id, COUNT(*) AS matches FROM capability_lemmas l \
              JOIN lemma_terms d ON d.id = l.lemma_id \
              JOIN capabilities c ON c.rowid = l.capability_rowid \
              WHERE d.lemma IN ({placeholders}) AND c.status = 'ready' \
              GROUP BY c.id ORDER BY matches DESC, c.id ASC LIMIT ?"
        );
        let mut values: Vec<rusqlite::types::Value> = lemmas
            .iter()
            .map(|lemma| rusqlite::types::Value::Text(lemma.clone()))
            .collect();
        values.push(rusqlite::types::Value::Integer(limit as i64));
        self.docs
            .sqlite_store()
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(conn, &sql, values, |row| row.get(0))
            })
            .map_err(VectorDbError::from)
    }
}

/// One deterministic project card: workspace, crate, or domain facts
/// derived from README, Cargo, component gates, and jumps — never
/// model-written. Cards link doc anchors to code paths.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectCard {
    /// Stable key (`kind:key`, e.g. `crate:guidance-core`).
    pub key: String,
    /// `workspace` | `crate` | `domain`.
    pub kind: String,
    /// Card title.
    pub title: String,
    /// Card body (facts with anchors and paths).
    pub body: String,
    /// Workspace-relative doc path the card anchors to.
    pub source_path: String,
    /// Section anchor within the doc (may be empty).
    pub anchor: String,
    /// JSON array of alias names.
    pub aliases_json: String,
    /// JSON array of workspace-relative code paths.
    pub code_paths_json: String,
    /// JSON array of symbols.
    pub symbols_json: String,
    /// Blake3 of the card body (incremental cache key input).
    pub content_hash: String,
    /// Generation time (ms epoch).
    pub generated_at: i64,
}

impl ProjectCard {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            key: row.get(0)?,
            kind: row.get(1)?,
            title: row.get(2)?,
            body: row.get(3)?,
            source_path: row.get(4)?,
            anchor: row.get(5)?,
            aliases_json: row.get(6)?,
            code_paths_json: row.get(7)?,
            symbols_json: row.get(8)?,
            content_hash: row.get(9)?,
            generated_at: row.get(10)?,
        })
    }
}

const PROJECT_CARD_COLUMNS: &str = "key, kind, title, body, source_path, anchor,
        aliases_json, code_paths_json, symbols_json, content_hash, generated_at";

impl super::GuidanceSearchDb {
    /// Insert or replace project cards without touching fragments.
    pub fn replace_project_cards(&self, rows: &[ProjectCard]) -> Result<usize, VectorDbError> {
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                for row in rows {
                    fluent_db::query::execute(
                        tx,
                        "INSERT OR REPLACE INTO project_cards
                         (key, kind, title, body, source_path, anchor,
                          aliases_json, code_paths_json, symbols_json,
                          content_hash, generated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        rusqlite::params![
                            row.key,
                            row.kind,
                            row.title,
                            row.body,
                            row.source_path,
                            row.anchor,
                            row.aliases_json,
                            row.code_paths_json,
                            row.symbols_json,
                            row.content_hash,
                            row.generated_at,
                        ],
                    )?;
                }
                Ok(rows.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Every stored project card, ordered by key (deterministic render).
    pub fn project_cards_all(&self) -> Result<Vec<ProjectCard>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                &format!("SELECT {PROJECT_CARD_COLUMNS} FROM project_cards ORDER BY key ASC"),
                &[],
                ProjectCard::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Project-card FTS match over title/body/aliases (the table's columns).
    fn build_project_card_fts_match(query: &str) -> Option<String> {
        use common_core::string::{fts_match_tokens, fts_quote};
        let tokens = fts_match_tokens(query);
        if tokens.is_empty() {
            return None;
        }
        let or: Vec<String> = tokens.iter().map(|token| fts_quote(token)).collect();
        Some(format!(
            "{{title body aliases_json}} : ({})",
            or.join(" OR ")
        ))
    }

    /// Every stored project card of one kind, ordered by key.
    pub fn project_cards_by_kind(&self, kind: &str) -> Result<Vec<ProjectCard>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                &format!(
                "SELECT {PROJECT_CARD_COLUMNS} FROM project_cards WHERE kind = ?1 ORDER BY key ASC"
            ),
                &[&kind],
                ProjectCard::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// FTS over project-card title/body, filtered to one kind.
    pub fn search_project_cards(
        &self,
        kind: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ProjectCard>, VectorDbError> {
        let Some(match_expr) = Self::build_project_card_fts_match(query) else {
            return Ok(Vec::new());
        };
        let sql = "SELECT c.* FROM project_cards_fts
             JOIN project_cards c ON c.rowid = project_cards_fts.rowid
             WHERE project_cards_fts MATCH ?1 AND c.kind = ?2
             ORDER BY bm25(project_cards_fts) ASC LIMIT ?3"
            .to_string();
        self.docs
            .sqlite_store()
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(
                    conn,
                    &sql,
                    vec![
                        rusqlite::types::Value::Text(match_expr),
                        rusqlite::types::Value::Text(kind.to_string()),
                        rusqlite::types::Value::Integer(limit as i64),
                    ],
                    ProjectCard::from_row,
                )
            })
            .map_err(VectorDbError::from)
    }

    /// Delete project cards of one kind whose keys left the importer set.
    pub fn delete_project_cards_not_in(
        &self,
        kind: &str,
        keep: &[String],
    ) -> Result<usize, VectorDbError> {
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                let existing: Vec<String> = fluent_db::query::query_rows_from_iter(
                    tx,
                    "SELECT key FROM project_cards WHERE kind = ?1",
                    vec![rusqlite::types::Value::Text(kind.to_string())],
                    |row| row.get(0),
                )?;
                let keep: std::collections::HashSet<&str> =
                    keep.iter().map(String::as_str).collect();
                let mut removed = 0usize;
                for key in existing {
                    if !keep.contains(key.as_str()) {
                        fluent_db::query::execute(
                            tx,
                            "DELETE FROM project_cards WHERE key = ?1",
                            rusqlite::params![key],
                        )?;
                        removed += 1;
                    }
                }
                Ok(removed)
            })
            .map_err(VectorDbError::from)
    }

    /// Stored project-card count (orientation-plane coverage signal).
    pub fn project_card_count(&self) -> Result<i64, VectorDbError> {
        let count = self.docs.sqlite_store().query_row(
            "SELECT COUNT(*) FROM project_cards",
            &[],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.unwrap_or(0))
    }
}
