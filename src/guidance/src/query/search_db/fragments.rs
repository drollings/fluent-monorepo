//! Fragment recall rows, filter pushdown, and FTS match building.
//!
//! Moved verbatim from `search-vector`: `FileRecord`/`Fragment*` row
//! shapes, `NodeSyncFingerprint`, `FragmentFilter` + `append_fragment_filter`,
//! and `build_fts_match`. Row `from_row` constructors and the filter helper
//! are crate-visible for the `GuidanceSearchDb` queries in `search_db`; everything
//! else keeps its exact prior visibility. No SQL text changed.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use fluent_db::error::DbError;
use fluent_db::vector;
use rusqlite::params;

use super::VectorDbError;

// -- P1 fragment records, filter, and FTS match builder ---------------------

/// Indexed file row (mirrors the P0 `FileInfo` contract).
#[derive(Debug, Clone, PartialEq, Eq)]
/// -- P1 fragment records, filter, and FTS match builder ---------------------
/// Indexed file row (mirrors the P0 `FileInfo` contract).
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
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
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
/// The mtime is carried at nanosecond resolution — a millisecond stamp
/// cannot separate two same-length rewrites that land inside one
/// millisecond, so a doc edited that way would read as unchanged and be
/// skipped. Same risk class as all mtime discipline here: a delete+add
/// pair that keeps the count with an older-or-equal max mtime reads as
/// fresh (change detection, not content addressing).
///
/// `gen` is the disposable-index marker: a hash over the sidecar header
/// manifest (one first line per file, so the gate never pays for bodies)
/// that refuses a mismatched index and rebuilds. Headerless legacy files
/// contribute path, mtime, and size instead, documented as content-blind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeSyncFingerprint {
    /// Newest member-doc mtime (ns epoch), 0 when unreadable.
    pub(crate) max_mtime_ns: i64,
    /// Member-doc file count.
    pub(crate) file_count: usize,
    /// GEN manifest hash over the sidecar headers.
    pub(crate) gen: String,
}

/// First line of a file without parsing it: the GEN manifest reads
/// headers only, never bodies.
fn first_line_raw(path: &std::path::Path) -> Option<String> {
    use std::io::BufRead as _;
    let file = std::fs::File::open(path).ok()?;
    std::io::BufReader::new(file).lines().next()?.ok()
}

pub(crate) fn node_sync_fingerprint(json_files: &[std::path::PathBuf]) -> NodeSyncFingerprint {
    let mut max_mtime_ns = 0i64;
    let mut manifest = String::new();
    let mut sorted: Vec<&std::path::PathBuf> = json_files.iter().collect();
    sorted.sort();
    for path in sorted {
        let ns = common_core::io::mtime(path).map_or(0, common_core::io::system_time_ns);
        max_mtime_ns = max_mtime_ns.max(ns.min(i64::MAX as u64) as i64);
        manifest.push_str(&path.to_string_lossy());
        manifest.push('\0');
        if let Some(line) = first_line_raw(path) {
            manifest.push_str(&line);
        } else {
            let size = std::fs::metadata(path).map_or(0, |m| m.len());
            let _ = write!(manifest, "{ns}:{size}");
        }
        manifest.push('\n');
    }
    NodeSyncFingerprint {
        max_mtime_ns,
        file_count: json_files.len(),
        gen: common_core::hash::blake3_hex(manifest.as_bytes()),
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
/// Lemma row for the `fragment_lemmas` L2 table.
pub struct FragmentLemma {
    /// Owning fragment id.
    pub fragment_id: String,
    /// Lemma string.
    pub lemma: String,
    /// Parse confidence.
    pub confidence: f64,
}

/// Identity row for one definition: the content-stable symbol key
/// plus the interface fingerprint that separates interface edits
/// from body edits. One row per (file, symbol, scope); window
/// chunks of one entity share the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRecord {
    /// Stable symbol id (`file#scope::name`, no positional index).
    pub symbol_id: String,
    /// Owning file id.
    pub file_id: String,
    /// Declared name, if any.
    pub name: Option<String>,
    /// Lexical scope breadcrumb.
    pub scope: Option<String>,
    /// Hash of the interface (name, type, scope, signature).
    pub interface_hash: String,
}

impl SymbolRecord {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            symbol_id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            scope: row.get(3)?,
            interface_hash: row.get(4)?,
        })
    }
}

/// Full fragment row for hit materialization.
#[derive(Debug, Clone, PartialEq)]
/// Full fragment row for hit materialization.
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
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
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
/// Recall filter pushed into fragment queries.
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
/// One ranked recall hit (rank assignment stays with the recall loop).
pub struct ScoredFragment {
    /// Fragment id.
    pub id: String,
    /// Owning file id.
    pub file_id: String,
    /// Raw score (higher is better; scale varies by axis).
    pub score: f64,
}

/// One fragment vector row: id, file id, legacy FP32 blob, int8 blob.
pub(crate) type FragmentVectorRow = (String, String, Option<Vec<u8>>, Option<Vec<u8>>);

/// Shared pushdown: file allow-list, symbol allow-lists, mtime window.
/// `frag_alias`/`file_alias` qualify the joined `fragments`/`files`.
pub(crate) fn append_fragment_filter(
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
        let placeholders: Vec<String> =
            (0..ids.len()).map(|i| format!("?{}", *param + i)).collect();
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
/// empty (never match-all). Tokenizing/quoting compose the shared
/// `common_core::string` helpers; only the column bindings are
/// fragment-specific.
#[must_use]
pub fn build_fts_match(query: &str) -> Option<String> {
    use common_core::string::{cjk_bigrams, fts_match_tokens, fts_quote};
    let ascii = fts_match_tokens(query);
    let bigrams = cjk_bigrams(query);
    let mut parts = Vec::new();
    if !ascii.is_empty() {
        let or: Vec<String> = ascii.iter().map(|t| fts_quote(t)).collect();
        parts.push(format!(
            "{{content_text symbol_name doc}} : ({})",
            or.join(" OR ")
        ));
    }
    if !bigrams.is_empty() {
        let or: Vec<String> = bigrams.iter().map(|b| fts_quote(b)).collect();
        parts.push(format!("cjk_text : ({})", or.join(" OR ")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" OR "))
    }
}

impl super::GuidanceSearchDb {
    /// Fragment rows committed: the live retrieval-unit count behind the
    /// MCP `node_count` key (the shared `nodes` document store is
    /// written by `sync_from_dir`, not by the fragment sync).
    pub fn fragment_count(&self) -> Result<i64, VectorDbError> {
        let count =
            self.docs
                .sqlite_store()
                .query_row("SELECT COUNT(*) FROM fragments", &[], |row| {
                    row.get::<_, i64>(0)
                })?;
        Ok(count.unwrap_or(0))
    }

    /// Fragments carrying a vector (FP32 or quantized): the live count
    /// behind the MCP `embedding_count` key.
    pub fn embedded_fragment_count(&self) -> Result<i64, VectorDbError> {
        let count = self.docs.sqlite_store()
            .query_row(
                "SELECT COUNT(*) FROM fragments WHERE embedding IS NOT NULL OR embedding_q8 IS NOT NULL",
                &[],
                |row| row.get::<_, i64>(0),
            )?;
        Ok(count.unwrap_or(0))
    }

    /// Stored fragments with no vector (FP32 or quantized), id-ordered
    /// and bounded: the embed backfill's candidate pool. Callers select
    /// semantic units from these rows; the status table decides which
    /// ones are actually due. `None` reads the whole pool (SQLite
    /// `LIMIT -1`); prefer a bound on interactive paths.
    pub fn fragments_without_vectors(
        &self,
        limit: Option<usize>,
    ) -> Result<Vec<FragmentRow>, VectorDbError> {
        let bound = limit.map_or(-1, |value| value as i64);
        self.docs
            .sqlite_store()
            .query_rows(
                "SELECT id, grp, file_id, range_json, content_kind, content_text,
                        symbol_type, symbol_name, scope, signature, doc, modifiers,
                        heading, heading_level
                 FROM fragments
                 WHERE embedding IS NULL AND embedding_q8 IS NULL
                 ORDER BY id ASC LIMIT ?1",
                params![bound],
                FragmentRow::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Store backfill vectors: quantized bytes only (the same convention
    /// as the ingest writer), FP32 cleared. One transaction; ids are
    /// unique so exactly one row updates per input.
    pub fn store_fragment_vectors(
        &self,
        rows: &[(String, Vec<f32>)],
    ) -> Result<usize, VectorDbError> {
        if rows.is_empty() {
            return Ok(0);
        }
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                for (id, vector) in rows {
                    let quantized = vector::QuantizedEmbedding::from_f32(vector);
                    let bytes = vector::quantized_to_bytes(&quantized);
                    fluent_db::query::execute(
                        tx,
                        "UPDATE fragments SET embedding = NULL, embedding_q8 = ?1 WHERE id = ?2",
                        params![bytes, id],
                    )?;
                }
                Ok(rows.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Sync all JSON files from a directory into the shared `nodes`
    /// document store. Walks JSON files, parses each document, upserts
    /// into the database with a change-gate fingerprint, and rebuilds
    /// the HNSW index after sync. Generic primitive: any consumer may
    /// call it; guidance's live sync path does not.
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

        // Build the whole batch first: duplicates are a property of the docs,
        // so one set replaces a SELECT per member; one transaction writes it.
        struct NodeRow {
            name: String,
            source: String,
            signature: Option<String>,
            comment: Option<String>,
            module: String,
            language: String,
        }
        let mut rows: Vec<NodeRow> = Vec::new();
        let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
        for path in &json_files {
            let content = common_core::io::read_to_string_err(path)
                .map_err(|e| DbError::Other(format!("failed to read {}: {e}", path.display())))?;
            // Sidecar receipts ride on the first line; only the JSON
            // body parses here. The header manifest feeds the GEN gate.
            let (_header, body) = fluent_types::sidecar::strip_header(&content);
            if body.trim().is_empty() {
                continue;
            }

            let doc: serde_json::Value = serde_json::from_str(body)
                .map_err(|e| DbError::Other(format!("failed to parse {}: {e}", path.display())))?;

            let source = doc["meta"]["source"].as_str().unwrap_or("").to_string();
            let module = doc["meta"]["module"].as_str().unwrap_or("").to_string();
            let language = doc["meta"]["language"]
                .as_str()
                .unwrap_or("zig")
                .to_string();
            let comment = doc["comment"].as_str().map(str::to_string);

            // Skip duplicates where (name, signature) already exists. This
            // handles multiple JSON files for the same source (e.g.
            // `guidance/src/...` vs `src/guidance/src/...`).
            if let Some(members) = doc["members"].as_array() {
                for member in members {
                    let name = member["name"].as_str().unwrap_or("").to_string();
                    let signature = member["signature"].as_str().map(str::to_string);
                    if !seen.insert((name.clone(), signature.clone())) {
                        continue;
                    }
                    let member_comment = member["comment"].as_str().map(str::to_string);
                    rows.push(NodeRow {
                        name,
                        source: source.clone(),
                        signature,
                        comment: member_comment.or_else(|| comment.clone()),
                        module: module.clone(),
                        language: language.clone(),
                    });
                }
            }
        }

        let synced = self.docs.sqlite_store().transaction(|tx| {
            // Clear existing nodes before re-sync to avoid stale duplicates.
            fluent_db::query::execute(tx, "DELETE FROM nodes", [])?;
            let mut insert = tx.prepare(
                "INSERT INTO nodes (name, source, signature, comment, module, language)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for row in &rows {
                insert.execute(rusqlite::params![
                    row.name,
                    row.source,
                    row.signature,
                    row.comment,
                    row.module,
                    row.language
                ])?;
            }
            Ok(rows.len())
        })?;

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
    fn read_node_sync_watermark(&self) -> Result<Option<NodeSyncFingerprint>, VectorDbError> {
        let value: Option<String> = self
            .docs
            .sqlite_store()
            .query_row(
                "SELECT value FROM sync_state WHERE key = 'node_sync'",
                &[],
                |row| row.get(0),
            )
            .map_err(VectorDbError::from)?;
        Ok(value.and_then(|value| {
            let mut parts = value.splitn(3, ':');
            Some(NodeSyncFingerprint {
                max_mtime_ns: parts.next()?.parse().ok()?,
                file_count: parts.next()?.parse().ok()?,
                gen: parts.next()?.to_string(),
            })
        }))
    }

    /// Persist the node-sync watermark after a successful rebuild.
    fn write_node_sync_watermark(
        &self,
        fingerprint: &NodeSyncFingerprint,
    ) -> Result<(), VectorDbError> {
        let value = format!(
            "{}:{}:{}",
            fingerprint.max_mtime_ns, fingerprint.file_count, fingerprint.gen
        );
        self.docs
            .sqlite_store()
            .execute(
                "INSERT OR REPLACE INTO sync_state (key, value) VALUES ('node_sync', ?1)",
                rusqlite::params![value],
            )
            .map_err(VectorDbError::from)?;
        Ok(())
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
        self.docs.sqlite_store()
            .transaction(|tx| {
                use rusqlite::params;
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM fragment_lemmas WHERE fragment_rowid IN (SELECT rowid FROM fragments WHERE file_id = ?1)",
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
                // One prepared statement per file: the lemma batch is the
                // hot path (hundreds of rows per source file). Fragment and
                // term ids resolve to integers once per file and cache.
                let mut rowid_of: HashMap<&str, i64> = HashMap::new();
                for fragment in fragments {
                    let rowid: Option<i64> = fluent_db::query::query_row(
                        tx,
                        "SELECT rowid FROM fragments WHERE id = ?1",
                        params![fragment.id],
                        |row| row.get(0),
                    )?;
                    if let Some(rowid) = rowid {
                        rowid_of.insert(fragment.id.as_str(), rowid);
                    }
                }
                let mut term_of: HashMap<String, i64> = HashMap::new();
                let mut insert_lemma = tx.prepare_cached(
                    "INSERT OR REPLACE INTO fragment_lemmas (fragment_rowid, lemma_id, confidence)
                     VALUES (?1, ?2, ?3)",
                )?;
                for lemma in lemmas {
                    let Some(fragment_rowid) = rowid_of.get(lemma.fragment_id.as_str()) else {
                        continue;
                    };
                    let lemma_id = Self::lemma_term_id(tx, &mut term_of, &lemma.lemma)?;
                    insert_lemma.execute(params![fragment_rowid, lemma_id, lemma.confidence])?;
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
            .docs
            .sqlite_store()
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

    /// Freshness fingerprints for every indexed file in one pass: the
    /// bulk form of [`GuidanceDb::fragment_freshness`] for the sync
    /// change gate. One file-table scan plus one embedded-set scan
    /// instead of a probe per file — a per-file loop here costs seconds
    /// on thousand-file trees while the scans cost milliseconds.
    pub fn all_fragment_freshness(
        &self,
    ) -> Result<HashMap<String, FragmentFreshness>, VectorDbError> {
        let rows: Vec<(String, i64, i64)> = self
            .docs
            .sqlite_store()
            .query_rows(
                "SELECT id, size_bytes, last_modified_time FROM files",
                &[],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(VectorDbError::from)?;
        let embedded: HashSet<String> = self
            .docs
            .sqlite_store()
            .query_rows(
                "SELECT DISTINCT file_id FROM fragments
                 WHERE embedding IS NOT NULL OR embedding_q8 IS NOT NULL",
                &[],
                |row| row.get(0),
            )
            .map_err(VectorDbError::from)?
            .into_iter()
            .collect();
        Ok(rows
            .into_iter()
            .map(|(id, size, mtime)| {
                (
                    id.clone(),
                    FragmentFreshness {
                        size_bytes: u64::try_from(size).unwrap_or(u64::MAX),
                        last_modified_time: mtime,
                        embedded: embedded.contains(&id),
                    },
                )
            })
            .collect())
    }

    /// Merge FTS segments left by bulk inserts. Call once per bulk pass;
    /// per-file callers would pay a whole-index merge each time.
    pub fn optimize_fts(&self) -> Result<(), VectorDbError> {
        self.docs.sqlite_store().execute(
            "INSERT INTO fragments_fts(fragments_fts) VALUES('optimize')",
            &[],
        )?;
        Ok(())
    }

    /// Insert or replace lemma rows without touching fragments.
    pub fn insert_fragment_lemmas(&self, lemmas: &[FragmentLemma]) -> Result<usize, VectorDbError> {
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                let mut term_of: HashMap<String, i64> = HashMap::new();
                let mut insert = tx.prepare_cached(
                    "INSERT OR REPLACE INTO fragment_lemmas (fragment_rowid, lemma_id, confidence)
                     VALUES (?1, ?2, ?3)",
                )?;
                for lemma in lemmas {
                    let fragment_rowid: Option<i64> = fluent_db::query::query_row(
                        tx,
                        "SELECT rowid FROM fragments WHERE id = ?1",
                        params![lemma.fragment_id],
                        |row| row.get(0),
                    )?;
                    let Some(fragment_rowid) = fragment_rowid else {
                        continue;
                    };
                    let lemma_id = Self::lemma_term_id(tx, &mut term_of, &lemma.lemma)?;
                    insert
                        .execute(params![fragment_rowid, lemma_id, lemma.confidence])
                        .map_err(DbError::from)?;
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
        // Deterministic tie-break independent of ingest order and
        // rowids: parallel waves insert in unstable order, so rank
        // ties must never resolve by rowid. Relative path and range
        // are content-stable (absolute ids are not — they embed the
        // workspace root), and the fragment id settles the rest.
        let _ = write!(
            sql,
            " ORDER BY r ASC, fl.relative_path ASC, f.range_json ASC, f.id ASC LIMIT ?{param}"
        );
        values.push(rusqlite::types::Value::Integer(limit as i64));
        self.docs
            .sqlite_store()
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

    /// Lemma dictionary entry point: the id for one lemma string, creating
    /// the term row on first use. Callers batch through a per-file cache.
    pub(super) fn lemma_term_id(
        tx: &rusqlite::Transaction<'_>,
        cache: &mut HashMap<String, i64>,
        lemma: &str,
    ) -> Result<i64, DbError> {
        if let Some(id) = cache.get(lemma) {
            return Ok(*id);
        }
        tx.execute(
            "INSERT OR IGNORE INTO lemma_terms(lemma) VALUES (?1)",
            rusqlite::params![lemma],
        )
        .map_err(DbError::from)?;
        let id: i64 = tx
            .query_row(
                "SELECT id FROM lemma_terms WHERE lemma = ?1",
                rusqlite::params![lemma],
                |row| row.get(0),
            )
            .map_err(DbError::from)?;
        cache.insert(lemma.to_string(), id);
        Ok(id)
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
            .docs
            .sqlite_store()
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
        self.docs
            .sqlite_store()
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
        // `fragment_lemmas` is WITHOUT ROWID; ties break on
        // content-stable keys, never ingest order: parallel waves
        // insert in unstable order, so rowids must not leak into
        // ranks (same rule as the FTS route).
        let sql = format!(
            "SELECT f.id FROM fragments f \
             JOIN fragment_lemmas l ON l.fragment_rowid = f.rowid \
             JOIN lemma_terms d ON d.id = l.lemma_id \
             JOIN files fl ON fl.id = f.file_id \
             WHERE d.lemma IN ({}) \
             GROUP BY f.id, f.rowid ORDER BY COUNT(*) DESC, fl.relative_path ASC, f.range_json ASC, f.id ASC LIMIT ?{}",
            placeholders.join(","),
            lemmas.len() + 1
        );
        let mut values: Vec<rusqlite::types::Value> = lemmas
            .iter()
            .map(|l| rusqlite::types::Value::Text(l.clone()))
            .collect();
        values.push(rusqlite::types::Value::Integer(limit as i64));
        self.docs
            .sqlite_store()
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(conn, &sql, values, |row| row.get(0))
            })
            .map_err(VectorDbError::from)
    }

    /// Major entity ids for a file (major = `grp = id`, or ungrouped).
    /// Diagnostic + adapter input for entity listing.
    pub fn entity_ids_for_file(&self, file_id: &str) -> Result<Vec<String>, VectorDbError> {
        self.docs
            .sqlite_store()
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
            fluent_db::sqlite::in_clause(file_ids.len())
        );
        let values: Vec<rusqlite::types::Value> = file_ids
            .iter()
            .map(|id| rusqlite::types::Value::Text(id.clone()))
            .collect();
        self.docs
            .sqlite_store()
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(conn, &sql, values, |row| row.get(0))
            })
            .map_err(VectorDbError::from)
    }
}
