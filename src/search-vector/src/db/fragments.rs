//! Fragment recall rows, filter pushdown, and FTS match building.
//!
//! Moved verbatim from `db.rs` (M6 split): `FileRecord`/`Fragment*` row
//! shapes, `NodeSyncFingerprint`, `FragmentFilter` + `append_fragment_filter`,
//! and `build_fts_match`. Row `from_row` constructors and the filter helper
//! are crate-visible for the `GuidanceDb` queries in `db.rs`; everything
//! else keeps its exact prior visibility. No SQL text changed.

use std::fmt::Write as _;

// -- P1 fragment records, filter, and FTS match builder ---------------------

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NodeSyncFingerprint {
    /// Newest member-doc mtime (ns epoch), 0 when unreadable.
    pub(crate) max_mtime_ns: i64,
    /// Member-doc file count.
    pub(crate) file_count: usize,
}

pub(crate) fn node_sync_fingerprint(json_files: &[std::path::PathBuf]) -> NodeSyncFingerprint {
    let mut max_mtime_ns = 0i64;
    for path in json_files {
        let ns = common_core::io::mtime(path).map_or(0, common_core::io::system_time_ns);
        max_mtime_ns = max_mtime_ns.max(ns.min(i64::MAX as u64) as i64);
    }
    NodeSyncFingerprint {
        max_mtime_ns,
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

/// Identity row for one definition: the stable symbol key plus the two
/// fingerprints that separate interface edits from body edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRecord {
    /// Stable symbol id (fragment id of the defining chunk).
    pub symbol_id: String,
    /// Owning file id.
    pub file_id: String,
    /// Declared name, if any.
    pub name: Option<String>,
    /// Lexical scope breadcrumb.
    pub scope: Option<String>,
    /// One-line signature.
    pub signature: Option<String>,
    /// Hash of the interface (name, type, scope, signature).
    pub interface_hash: String,
    /// Hash of the full fragment text.
    pub content_hash: String,
    /// Serialized range.
    pub range_json: String,
}

impl SymbolRecord {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            symbol_id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            scope: row.get(3)?,
            signature: row.get(4)?,
            interface_hash: row.get(5)?,
            content_hash: row.get(6)?,
            range_json: row.get(7)?,
        })
    }
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
            "{{content_text symbol_name}} : ({})",
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
