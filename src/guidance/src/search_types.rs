//! Search value types + pure search helpers (P0 contracts, no I/O).
//!
//! Wire shapes mirror `src/engine/types.ts` kinds (`serde` renames carry the
//! exact `"file"`/`"text"`/`"code"`/`"fts+vector"` tags). Pure helpers are
//! behavior-preserving ports of `pipeline/search/index.ts`
//! (`extractSymbolNames`, `symbolNameFromToken`, `fuseCandidates`,
//! `deriveMatchedBy`, `publicEntityId(s)`) and `storage/zvec.ts`
//! (`validateFragmentGroups`).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Ranges, content, metadata (mirror types.ts)
// ---------------------------------------------------------------------------

/// Fragment range. `serde` tags carry the exact zvec `kind` strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FragmentSpan {
    /// Whole-file range.
    File,
    /// Line/offset range.
    Text {
        /// 1-based start line.
        start_line: u32,
        /// 1-based end line.
        end_line: u32,
        /// Byte start offset.
        start_offset: u64,
        /// Byte end offset.
        end_offset: u64,
    },
    /// Raw byte range.
    Byte {
        /// Byte start offset.
        start_offset: u64,
        /// Byte end offset.
        end_offset: u64,
    },
    /// Whole-page range.
    Page {
        /// 1-based page number.
        page: u32,
    },
    /// Page text slice.
    PageText {
        /// 1-based page number.
        page: u32,
        /// Byte start offset within the page text.
        start_offset: u64,
        /// Byte end offset within the page text.
        end_offset: u64,
    },
    /// Page region (image fragments).
    PageRegion {
        /// 1-based page number.
        page: u32,
        /// Region origin + size.
        x: u32,
        /// Region origin + size.
        y: u32,
        /// Region origin + size.
        width: u32,
        /// Region origin + size.
        height: u32,
    },
}

/// Fragment content payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FragmentContent {
    /// UTF-8 text.
    Text {
        /// Fragment text.
        text: String,
    },
    /// Binary image bytes.
    Image {
        /// Raw image bytes.
        data: Vec<u8>,
        /// Image container format.
        format: ImageFormat,
    },
}

/// Image container formats (closed set from types.ts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// WebP.
    Webp,
    /// GIF.
    Gif,
}

/// Source file kinds (closed set from types.ts). Canonical home:
/// `fluent_types::file_kind::FileKind` — re-exported here so existing
/// `search_types::FileKind` paths keep working during the migration.
pub use fluent_types::file_kind::FileKind;

/// Code symbol types — the closed 6-set (roadmap C.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeSymbolType {
    /// Module / namespace.
    Module,
    /// Class.
    Class,
    /// Interface / trait / protocol.
    Interface,
    /// Function / method.
    Function,
    /// Value / constant / variable.
    Value,
    /// Alias / type alias.
    Alias,
}

impl CodeSymbolType {
    /// The closed 6-set, for vocabulary pins and fixture generation.
    #[must_use]
    pub fn all() -> &'static [Self] {
        use CodeSymbolType::{Alias, Class, Function, Interface, Module, Value};
        &[Module, Class, Interface, Function, Value, Alias]
    }
}

/// Code entity modifiers — the closed 7-set (roadmap C.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeEntityModifier {
    /// Exported from its module.
    Exported,
    /// Async function.
    Async,
    /// Static member.
    Static,
    /// Public visibility.
    Public,
    /// Private visibility.
    Private,
    /// Protected visibility.
    Protected,
    /// Internal visibility.
    Internal,
}

impl CodeEntityModifier {
    /// The closed 7-set, for vocabulary pins and fixture generation.
    #[must_use]
    pub fn all() -> &'static [Self] {
        use CodeEntityModifier::{Async, Exported, Internal, Private, Protected, Public, Static};
        &[Exported, Async, Static, Public, Private, Protected, Internal]
    }
}

/// Entity metadata (code or markdown branch).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum EntityMetadata {
    /// Code symbol metadata.
    Code {
        /// Closed symbol type.
        #[serde(rename = "symbolType")]
        symbol_type: CodeSymbolType,
        /// Declared name, if any.
        #[serde(rename = "symbolName")]
        symbol_name: Option<String>,
        /// Lexical scope breadcrumb.
        scope: Option<String>,
        /// Grammar node type.
        #[serde(rename = "nodeType")]
        node_type: Option<String>,
        /// One-line signature.
        signature: Option<String>,
        /// Leading doc comment.
        doc: Option<String>,
        /// Declared modifiers.
        modifiers: Vec<CodeEntityModifier>,
    },
    /// Markdown section metadata.
    Markdown {
        /// Section heading text, if any.
        heading: Option<String>,
        /// Heading level, if any.
        level: Option<u32>,
        /// Section scope breadcrumb.
        scope: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Files, entities, fragments
// ---------------------------------------------------------------------------

/// Per-file index status (mirrors `FileIndexStatus`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileIndexStatus {
    /// Index timestamp (ms epoch), if indexed.
    pub indexed_time: Option<i64>,
    /// Entity count for the file.
    pub entity_count: u64,
    /// Token count, when measured.
    pub token_count: Option<u64>,
    /// Truncated fragments, when measured.
    pub truncated_fragment_count: Option<u64>,
    /// Last failure, stored as data (never an aborted run).
    pub error: Option<String>,
}

/// Indexed file record (mirrors `FileInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
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
    /// Content hash, when computed.
    pub content_hash: Option<String>,
    /// File kind.
    pub kind: Option<FileKind>,
    /// Format tag (language / container).
    pub format: String,
    /// Index status, when indexed.
    pub index_status: Option<FileIndexStatus>,
}

/// A public search entity (mirrors `Entity`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    /// Public entity id (`group ?? id`).
    pub id: String,
    /// Owning file id.
    pub file_id: String,
    /// Entity range.
    pub range: FragmentSpan,
    /// Entity content.
    pub content: FragmentContent,
    /// Entity metadata, if any.
    pub metadata: Option<EntityMetadata>,
}

/// A stored fragment: group member or standalone entity (mirrors
/// `EntityFragment`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityFragment {
    /// Fragment id (unique per file).
    pub id: String,
    /// Group id; the major fragment carries its own id. An empty string
    /// behaves as no group (zvec rewrites the `""` major marker to the
    /// fragment id at write time — same observable result).
    pub group: Option<String>,
    /// Owning file id.
    pub file_id: String,
    /// Fragment range.
    pub range: FragmentSpan,
    /// Fragment content.
    pub content: FragmentContent,
    /// Fragment metadata, if any.
    pub metadata: Option<EntityMetadata>,
}

// ---------------------------------------------------------------------------
// Search plans, traces, hits (mirror types.ts search section)
// ---------------------------------------------------------------------------

/// Fused RRF composite score: an ordinal over within-route positions,
/// not a confidence and not a magnitude. Canonical home is
/// `search_vector::fusion::RrfScore` (M6) — this re-export keeps the P0
/// contract surface stable. See the canonical item for the full stance
/// (no arithmetic, `PartialOrd` ordering, [`RrfScore::value`] display
/// reads, transparent wire serialization).
pub use search_vector::fusion::RrfScore;

/// Recall route mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchPlanRouteMode {
    /// Lexical route.
    Fts,
    /// Vector route.
    Vector,
}

/// One recall route in a search plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPlanRoute {
    /// Route mode.
    pub mode: SearchPlanRouteMode,
    /// Route query text.
    pub query: String,
}

/// A resolved route (plan validation assigns `id`s: `fts`, `fts-2`, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSearchPlanRoute {
    /// Assigned route id.
    pub id: String,
    /// Route mode.
    pub mode: SearchPlanRouteMode,
    /// Route query text.
    pub query: String,
}

/// A search plan (mirrors `SearchPlan`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchPlan {
    /// Recall routes (≥1 after validation).
    pub routes: Vec<SearchPlanRoute>,
    /// Result limit (defaults to `DEFAULT_LIMIT`).
    pub limit: Option<usize>,
    /// Trace collection flag.
    pub trace: bool,
    /// Force-appended entity id.
    pub track_entity_id: Option<String>,
    /// Symbol preference flag.
    pub prefer_symbol: Option<bool>,
    /// Symbol-type filter.
    pub symbol_types: Option<Vec<CodeSymbolType>>,
    /// Path inclusion filters.
    pub include_paths: Option<Vec<String>>,
    /// Path exclusion filters.
    pub exclude_paths: Option<Vec<String>>,
    /// Glob filters (rg-style, later entries override).
    pub globs: Option<Vec<String>>,
    /// Case-insensitive glob filters.
    pub insensitive_globs: Option<Vec<String>>,
    /// File-type filters.
    pub file_types: Option<Vec<String>>,
    /// Excluded file types.
    pub excluded_file_types: Option<Vec<String>>,
    /// Modification-time window (ms epoch).
    pub modified_after: Option<i64>,
    /// Modification-time window (ms epoch).
    pub modified_before: Option<i64>,
}

/// A validated search plan (mirrors `ResolvedSearchPlan`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedSearchPlan {
    /// Resolved routes with assigned ids.
    pub routes: Vec<ResolvedSearchPlanRoute>,
    /// Result limit.
    pub limit: Option<usize>,
    /// Trace collection flag.
    pub trace: bool,
    /// Force-appended entity id.
    pub track_entity_id: Option<String>,
    /// Symbol preference flag.
    pub prefer_symbol: Option<bool>,
    /// Symbol-type filter.
    pub symbol_types: Option<Vec<CodeSymbolType>>,
    /// Path inclusion filters.
    pub include_paths: Option<Vec<String>>,
    /// Path exclusion filters.
    pub exclude_paths: Option<Vec<String>>,
    /// Glob filters.
    pub globs: Option<Vec<String>>,
    /// Case-insensitive glob filters.
    pub insensitive_globs: Option<Vec<String>>,
    /// File-type filters.
    pub file_types: Option<Vec<String>>,
    /// Excluded file types.
    pub excluded_file_types: Option<Vec<String>>,
    /// Modification-time window (ms epoch).
    pub modified_after: Option<i64>,
    /// Modification-time window (ms epoch).
    pub modified_before: Option<i64>,
}

/// Which recall path produced a trace. `Lemma` arrived in P1 (the zvec
/// vocabulary is `fts`/`vector`; lemma participation is an upgrade axis
/// visible in traces — `derive_matched_by` stays zvec-exact over the two
/// original paths).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecallPath {
    /// Lexical recall.
    Fts,
    /// Vector recall.
    Vector,
    /// Lemma-token recall (P1 L2).
    Lemma,
}

impl From<SearchPlanRouteMode> for RecallPath {
    /// Route modes and recall paths share the `"fts"` / `"vector"` vocabulary.
    fn from(mode: SearchPlanRouteMode) -> Self {
        match mode {
            SearchPlanRouteMode::Fts => Self::Fts,
            SearchPlanRouteMode::Vector => Self::Vector,
        }
    }
}

/// Per-route recall trace (mirrors `SearchRecallTrace`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRecallTrace {
    /// Recall path.
    pub path: RecallPath,
    /// Route id, when routed.
    pub route_id: Option<String>,
    /// Route query, when routed.
    pub query: Option<String>,
    /// Whether the route found this candidate.
    pub found: bool,
    /// Force-append flag.
    pub forced: Option<bool>,
    /// 0-based recall rank, when found.
    pub rank: Option<usize>,
    /// Raw recall score, when found.
    pub score: Option<f64>,
    /// Miss reason, when not found.
    pub reason: Option<String>,
}

/// Fusion/ranking stage trace (mirrors `SearchStageTrace`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchStageTrace {
    /// 1-based stage rank.
    pub rank: usize,
    /// Stage score (fused composite — ordinal, see [`RrfScore`]).
    pub score: RrfScore,
    /// Force-append flag.
    pub forced: Option<bool>,
}

/// Final-cut trace (mirrors `SearchFinalTrace`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFinalTrace {
    /// Whether the limit cut the list (false for force-appended tracks).
    pub returned_by_limit: bool,
    /// 1-based cutoff rank.
    pub cutoff_rank: usize,
}

/// Full per-hit trace (mirrors `SearchHitTrace`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHitTrace {
    /// Per-route recall traces.
    pub recall: Vec<SearchRecallTrace>,
    /// Fusion stage position.
    pub fusion: Option<SearchStageTrace>,
    /// Ranking stage position.
    pub ranking: Option<SearchStageTrace>,
    /// Final cut.
    #[serde(rename = "final")]
    pub final_trace: SearchFinalTrace,
}

/// Per-fragment evidence (mirrors `SearchHitEvidence`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHitEvidence {
    /// Evidence range.
    pub range: FragmentSpan,
    /// Evidence content.
    pub content: FragmentContent,
    /// Evidence metadata, if any.
    pub metadata: Option<EntityMetadata>,
    /// Whether the fragment is the group major.
    pub is_entity: bool,
    /// Recall path that surfaced this evidence.
    pub path: RecallPath,
    /// Route id, when routed.
    pub route_id: Option<String>,
    /// Route query, when routed.
    pub query: Option<String>,
    /// 0-based recall rank.
    pub rank: Option<usize>,
    /// Raw recall score.
    pub score: Option<f64>,
    /// Force-append flag.
    pub forced: Option<bool>,
}

/// What recalled a hit (mirrors `SearchMatchedBy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchMatchedBy {
    /// Lexical only.
    #[serde(rename = "fts")]
    Fts,
    /// Vector only.
    #[serde(rename = "vector")]
    Vector,
    /// Both paths.
    #[serde(rename = "fts+vector")]
    FtsAndVector,
}

/// A fused search hit (mirrors `SearchHit`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// Hit entity.
    pub entity: Entity,
    /// Owning file.
    pub file: FileInfo,
    /// Rank-ordered evidence.
    pub evidence: Vec<SearchHitEvidence>,
    /// 1-based fused rank.
    pub rank: usize,
    /// Fused RRF score (ordinal composite — see [`RrfScore`]).
    pub score: RrfScore,
    /// Recall provenance.
    pub matched_by: SearchMatchedBy,
    /// Trace, when requested.
    pub trace: Option<SearchHitTrace>,
}

/// Timing entry (mirrors `TimingEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingEntry {
    /// Stage name.
    pub name: String,
    /// Elapsed milliseconds.
    pub duration_ms: f64,
    /// Item count, when applicable.
    pub count: Option<u64>,
}

/// A search result (mirrors `SearchPlanResult`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPlanResult {
    /// Resolved plan.
    pub plan: ResolvedSearchPlan,
    /// Ranked hits.
    pub hits: Vec<SearchHit>,
    /// Force-appended track, when requested.
    pub tracked_hit: Option<SearchHit>,
    /// Stage timings.
    pub timings: Vec<TimingEntry>,
}

// ---------------------------------------------------------------------------
// Storage filter + hit (P0 value surface for the P1 trait)
// ---------------------------------------------------------------------------

/// Path/symbol/time selection pushed into storage queries. `file_ids` and
/// `group_ids` arrived in P1 (recall resolution + force-track targeting).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StorageFilter {
    /// File allow-list (`None` = no path selection; empty = no files).
    pub file_ids: Option<Vec<String>>,
    /// Group allow-list (force-track targeting).
    pub group_ids: Vec<String>,
    /// Required symbol names (`Owner::name` forms).
    pub symbol_names: Vec<String>,
    /// Required symbol types.
    pub symbol_types: Vec<CodeSymbolType>,
    /// Path inclusion prefixes.
    pub include_paths: Vec<String>,
    /// Path exclusion prefixes.
    pub exclude_paths: Vec<String>,
    /// Glob filters.
    pub globs: Vec<String>,
    /// File-type filters.
    pub file_types: Vec<String>,
    /// Modification-time window (ms epoch).
    pub modified_after: Option<i64>,
    /// Modification-time window (ms epoch).
    pub modified_before: Option<i64>,
}

/// One storage recall hit (fragment + file + raw position).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageHit {
    /// Hit fragment.
    pub fragment: EntityFragment,
    /// Owning file.
    pub file: FileInfo,
    /// 0-based recall rank.
    pub rank: usize,
    /// Raw recall score.
    pub score: f64,
}

// ---------------------------------------------------------------------------
// Errors (GUIDANCE ENGINE.STORAGE codes)
// ---------------------------------------------------------------------------

/// Machine-readable group/storage failure codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentErrorCode {
    /// Fragment belongs to a different file.
    FragmentFileMismatch,
    /// Duplicate fragment id within a file.
    DuplicateFragmentId,
    /// Group without exactly one major fragment.
    InvalidFragmentGroup,
}

impl FragmentErrorCode {
    /// The `GUIDANCE.ENGINE.STORAGE.*` code string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FragmentFileMismatch => "GUIDANCE.ENGINE.STORAGE.FRAGMENT_FILE_MISMATCH",
            Self::DuplicateFragmentId => "GUIDANCE.ENGINE.STORAGE.DUPLICATE_FRAGMENT_ID",
            Self::InvalidFragmentGroup => "GUIDANCE.ENGINE.STORAGE.INVALID_FRAGMENT_GROUP",
        }
    }
}

/// Group-discipline failure (per-file isolation: failure is stored data).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FragmentError {
    /// Fragment belongs to a different file.
    #[error("entity fragment belongs to the wrong file: {context}")]
    FragmentFileMismatch {
        /// `fileId=… fragmentId=… fragmentFileId=…` context.
        context: String,
    },
    /// Duplicate fragment id within a file.
    #[error("duplicate entity fragment id: {context}")]
    DuplicateFragmentId {
        /// `fileId=… fragmentId=…` context.
        context: String,
    },
    /// Group without exactly one major fragment.
    #[error("fragment group must have exactly one major fragment: {context}")]
    InvalidFragmentGroup {
        /// `fileId=… group=… majorCount=…` context.
        context: String,
    },
}

impl FragmentError {
    /// Machine-readable code for the failure.
    #[must_use]
    pub fn code(&self) -> FragmentErrorCode {
        match self {
            Self::FragmentFileMismatch { .. } => FragmentErrorCode::FragmentFileMismatch,
            Self::DuplicateFragmentId { .. } => FragmentErrorCode::DuplicateFragmentId,
            Self::InvalidFragmentGroup { .. } => FragmentErrorCode::InvalidFragmentGroup,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (ported verbatim)
// ---------------------------------------------------------------------------

/// Public entity id: `group ?? id` (empty group behaves as no group).
/// Source: `storage/zvec.ts` (`publicEntityId`).
#[must_use]
pub fn public_entity_id(fragment: &EntityFragment) -> &str {
    fragment
        .group
        .as_deref()
        .filter(|group| !group.is_empty())
        .unwrap_or(&fragment.id)
}

/// Public entity ids: majors + ungrouped fragments.
/// Source: `storage/zvec.ts` (`publicEntityIds`).
#[must_use]
pub fn public_entity_ids(fragments: &[EntityFragment]) -> Vec<String> {
    let mut ids = HashSet::new();
    let mut ordered = Vec::new();
    for fragment in fragments {
        let group = fragment.group.as_deref().filter(|g| !g.is_empty());
        if group.is_none() || group == Some(fragment.id.as_str()) {
            let id = public_entity_id(fragment).to_string();
            if ids.insert(id.clone()) {
                ordered.push(id);
            }
        }
    }
    ordered
}

/// Group-discipline validation: file ownership, id uniqueness, exactly one
/// major (`id == group`) per group.
/// Source: `storage/zvec.ts` (`validateFragmentGroups`).
pub fn validate_fragment_groups(
    file_id: &str,
    fragments: &[EntityFragment],
) -> Result<(), FragmentError> {
    let mut ids = HashSet::new();
    let mut groups: HashMap<&str, Vec<&EntityFragment>> = HashMap::new();
    for fragment in fragments {
        if fragment.file_id != file_id {
            return Err(FragmentError::FragmentFileMismatch {
                context: format!(
                    "fileId={file_id} fragmentId={} fragmentFileId={}",
                    fragment.id, fragment.file_id
                ),
            });
        }
        if !ids.insert(fragment.id.as_str()) {
            return Err(FragmentError::DuplicateFragmentId {
                context: format!("fileId={file_id} fragmentId={}", fragment.id),
            });
        }
        if let Some(group) = fragment.group.as_deref().filter(|g| !g.is_empty()) {
            groups.entry(group).or_default().push(fragment);
        }
    }
    for (group_id, group) in &groups {
        let major_count = group.iter().filter(|f| f.id == *group_id).count();
        if major_count != 1 {
            return Err(FragmentError::InvalidFragmentGroup {
                context: format!(
                    "fileId={file_id} group={group_id} majorCount={major_count}"
                ),
            });
        }
    }
    Ok(())
}

/// Query tokens that name code symbols, minus the keyword stoplist.
/// Source: `pipeline/search/index.ts` (`extractSymbolNames`).
/// Canonical implementation lives in `search_vector::tokens` (M13); this
/// wrapper keeps the P0 contract surface stable.
#[must_use]
pub fn extract_symbol_names(query: &str) -> Vec<String> {
    search_vector::tokens::symbol_names(query)
}

/// Owner-aware symbol name: keeps `Owner::name` only for uppercase owners.
/// Source: `pipeline/search/index.ts` (`symbolNameFromToken`).
/// Canonical implementation lives in `search_vector::tokens` (M13); this
/// wrapper keeps the P0 contract surface stable.
#[must_use]
pub fn symbol_name_from_token(token: &str) -> Option<String> {
    search_vector::tokens::symbol_name_from_token(token)
}

/// One fused candidate: id + RRF score + 1-based rank. Canonical home is
/// `search_vector::fusion::FusedHit` (M6) — re-exported here for surface
/// stability.
pub use search_vector::fusion::FusedHit;

/// N-route RRF fusion: `score = Σ 1/(K + rank)` over every found recall,
/// lexicographic id tie-break, 1-based ranks. Canonical implementation
/// lives in `search_vector::fusion::rrf_fuse` (M6); this re-export keeps
/// the P0 contract surface stable.
pub use search_vector::fusion::rrf_fuse;

/// Recall provenance from the contributing paths.
/// Source: `pipeline/search/index.ts` (`deriveMatchedBy`; empty defaults
/// to `fts`).
#[must_use]
pub fn derive_matched_by(paths: &[RecallPath]) -> SearchMatchedBy {
    let fts = paths.contains(&RecallPath::Fts);
    let vector = paths.contains(&RecallPath::Vector);
    if fts && vector {
        SearchMatchedBy::FtsAndVector
    } else if vector {
        SearchMatchedBy::Vector
    } else {
        SearchMatchedBy::Fts
    }
}

// NOTE (M13): the bespoke `floor_char_boundary` helper lived here and
// duplicated `str::floor_char_boundary` (std) exactly — the M13.1 battery
// proved them equal on every input — so it is deleted. Call
// `text.floor_char_boundary(index)` (the bin crate already did). The zvec
// `code.test.mjs:327` provenance stays pinned by the golden in
// `tests/search_types.rs`.

/// Whether a char is CJK (Han, Hiragana, Katakana, Hangul syllables + Jamo).
/// Canonical home is `common_core::string` (second consumer: the
/// search-vector FTS query side); this wrapper keeps the P0 contract
/// surface stable.
#[must_use]
pub fn is_cjk_char(c: char) -> bool {
    common_core::string::is_cjk_char(c)
}

/// Overlapping character bigrams over CJK runs — the application-level half
/// of the P0 CJK contract (`FTS_CJK_STRATEGY = "cjk-bigram-expansion"`).
/// Canonical implementation lives in `common_core::string` (shared by the
/// index side and the FTS query side in search-vector).
#[must_use]
pub fn cjk_bigrams(text: &str) -> Vec<String> {
    common_core::string::cjk_bigrams(text)
}

// ---------------------------------------------------------------------------
// Trait skeletons (signatures only; P1+ provides behavior)
// ---------------------------------------------------------------------------

/// Index storage contract: lexical + vector recall, entity lookup, file
/// listing. P1 implements it over `GuidanceDb` (FTS5 + HNSW).
pub trait WorkspaceIndexStorage: Send + Sync {
    /// Ranked lexical recall (0-based ranks, best first).
    fn search_fts(
        &self,
        query: &str,
        filter: &StorageFilter,
        limit: usize,
    ) -> Result<Vec<StorageHit>, FragmentError>;
    /// Ranked vector recall over a query embedding.
    fn search_vector(
        &self,
        embedding: &[f32],
        filter: &StorageFilter,
        limit: usize,
    ) -> Result<Vec<StorageHit>, FragmentError>;
    /// Entity + file lookup by public entity id.
    fn get_entity(&self, entity_id: &str) -> Result<Option<(Entity, FileInfo)>, FragmentError>;
    /// All indexed files.
    fn list_files(&self) -> Result<Vec<FileInfo>, FragmentError>;
}

/// Per-language extraction adapter. One impl per language (P2); the
/// dispatcher resolves adapters by registry, never by match (roadmap P0.2).
pub trait LanguageAdapter: Send + Sync {
    /// Canonical format id (e.g. `"rust"`).
    fn format(&self) -> &'static str;
    /// File extensions routed to this adapter (e.g. `&["rs"]`).
    fn extensions(&self) -> &'static [&'static str];
    /// Scope breadcrumb separator (e.g. `"::"`).
    fn scope_separator(&self) -> &'static str;
}

#[cfg(test)]
#[path = "../tests/search_types.rs"]
mod tests;
