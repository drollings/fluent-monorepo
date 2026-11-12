//! `search` command (P5): thin shell over the P1 recall core.
//!
//! One core (`guidance-core` index + query), thin execution shells — this
//! module owns CLI parsing and rendering only. Dispatch builds a
//! [`SearchPlan`] and runs [`run_recall`] over [`GuidanceDbStorage`]; the
//! `--rg` route runs the standalone L0 [`RgBackend`]. L0 is never fused.

use std::path::PathBuf;

use guidance_core::graph_index::{ContextDirection, ContextEdge, ContextFamily, ResolutionMethod};
use guidance_core::query::db_storage::GuidanceDbStorage;
use guidance_core::query::hybrid::{plan_from_query, stages_from_hits};
use guidance_core::query::ingest::LazyNlp;
use guidance_core::query::recall::run_recall;
use guidance_core::query::rg_backend::{RgBackend, RgHit, RgOptions};
use guidance_core::query::strategy::FsmEngine;
use guidance_core::query_engine::{OutputFormat, QueryEngine};
use guidance_core::search_types::{
    CodeSymbolType, SearchHit, SearchPlan, SearchPlanRoute, SearchPlanRouteMode,
};
use search_vector::GuidanceDb;

/// Recall route selection. At most one non-default mode flag may be set
/// (one-probe discipline: combined mode flags are rejected, never merged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Recall route selection. At most one non-default mode flag may be set
/// (one-probe discipline: combined mode flags are rejected, never merged).
pub enum SearchMode {
    /// RRF fusion across FTS + vector + lemma routes.
    #[default]
    Fuse,
    /// Lexical FTS route only.
    Fts,
    /// Vector route only (declines without an embedder).
    Vector,
    /// Standalone managed-rg L0 (never fused, index-independent).
    Rg,
}

/// How many top hits render full code snippets (the rest list lines).
pub const EXPLAIN_SNIPPET_TOP_K: usize = 3;

/// Max snippet lines per top hit (signature head first, never whole files).
pub const EXPLAIN_SNIPPET_MAX_LINES: usize = 40;

/// Top doc paragraphs per explain call.
pub const EXPLAIN_DOC_TOP_K: usize = 3;

/// Max excerpt chars per doc paragraph.
pub const EXPLAIN_DOC_EXCERPT_CHARS: usize = 500;

/// Doc-walk caps: bounded cost before any model call.
pub const EXPLAIN_DOC_MAX_FILES: usize = 200;
/// Doc-walk caps: bounded cost before any model call.
pub const EXPLAIN_DOC_MAX_BYTES: u64 = 1_000_000;

/// Max output tokens for the concise answer.
pub const EXPLAIN_ANSWER_MAX_TOKENS: usize = 256;

/// Resolve the mode from the `--fts` / `--vector` / `--rg` / `--fuse` flags.
/// More than one mode flag is an error (rejected, never merged).
pub fn resolve_mode(fts: bool, vector: bool, rg: bool, fuse: bool) -> Result<SearchMode, String> {
    let count = [fts, vector, rg, fuse].iter().filter(|flag| **flag).count();
    if count > 1 {
        return Err("only one of --fts, --vector, --rg, --fuse may be set".to_string());
    }
    if fts {
        Ok(SearchMode::Fts)
    } else if vector {
        Ok(SearchMode::Vector)
    } else if rg {
        Ok(SearchMode::Rg)
    } else {
        Ok(SearchMode::Fuse)
    }
}

/// Parse `--symbol-type` values against the closed 6-set (lowercase).
pub fn parse_symbol_type(value: &str) -> Result<CodeSymbolType, String> {
    match value {
        "module" => Ok(CodeSymbolType::Module),
        "class" => Ok(CodeSymbolType::Class),
        "interface" => Ok(CodeSymbolType::Interface),
        "function" => Ok(CodeSymbolType::Function),
        "value" => Ok(CodeSymbolType::Value),
        "alias" => Ok(CodeSymbolType::Alias),
        _ => Err(format!(
            "unknown symbol type '{value}' (module|class|interface|function|value|alias)"
        )),
    }
}

/// Build the recall plan for a fused or single-route search. Pure.
#[must_use]
/// Build the recall plan for a fused or single-route search. Pure.
pub fn build_search_plan(
    query: &str,
    mode: SearchMode,
    limit: usize,
    trace: bool,
    prefer_symbol: bool,
    globs: &[String],
    symbol_types: &[CodeSymbolType],
) -> SearchPlan {
    let globs_opt = (!globs.is_empty()).then(|| globs.to_vec());
    let symbol_types_opt = (!symbol_types.is_empty()).then(|| symbol_types.to_vec());
    match mode {
        SearchMode::Fuse => {
            let mut fsm = FsmEngine::new();
            let intent = fsm.run(query).intent;
            let mut plan = plan_from_query(query, intent, limit);
            plan.trace = trace;
            plan.prefer_symbol = Some(prefer_symbol);
            plan.globs = globs_opt;
            plan.symbol_types = symbol_types_opt;
            plan
        }
        SearchMode::Fts | SearchMode::Vector => {
            let route_mode = match mode {
                SearchMode::Fts => SearchPlanRouteMode::Fts,
                _ => SearchPlanRouteMode::Vector,
            };
            SearchPlan {
                routes: vec![SearchPlanRoute {
                    mode: route_mode,
                    query: query.to_string(),
                }],
                limit: Some(limit),
                trace,
                prefer_symbol: Some(prefer_symbol),
                globs: globs_opt,
                symbol_types: symbol_types_opt,
                ..Default::default()
            }
        }
        SearchMode::Rg => SearchPlan {
            // Unused by the rg path (L0 never enters recall); kept valid so
            // callers can log one plan shape unconditionally.
            routes: vec![SearchPlanRoute {
                mode: SearchPlanRouteMode::Fts,
                query: query.to_string(),
            }],
            limit: Some(limit),
            trace,
            ..Default::default()
        },
    }
}

/// MCP cap: fused/CLI limits clamp to 50 results.
pub const MAX_LIMIT: usize = 50;

/// Explain context budget (max `### Context` lines): calibrated by the
/// M4c sweep — 12 is the smallest cap with full marginal gain on the
/// calibration split (uncapped/12: +0.923; 10: +0.769; 8: +0.615;
/// 5: +0.385; 3: +0.231; seeded distractors absent at every cap).
pub const EXPLAIN_CONTEXT_BUDGET: usize = 12;

/// CLI default limit when `--limit` is absent and the mode is not L0
/// (today's CLI default, preserved).
pub const CLI_DEFAULT_LIMIT: usize = 10;

/// Clamp a requested limit into `[1, MAX_LIMIT]`.
#[must_use]
/// Clamp a requested limit into `[1, MAX_LIMIT]`.
pub fn clamp_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_LIMIT)
}

/// L0 display options: context capture and mtime post-filter.
#[derive(Debug, Clone, Default)]
/// L0 display options: context capture and mtime post-filter.
pub struct RgDisplayOptions {
    /// `-B` lines of leading context.
    pub before: usize,
    /// `-A` lines of trailing context.
    pub after: usize,
    /// Modification-time floor (ms epoch).
    pub mtime_after_ms: Option<i64>,
    /// Modification-time ceiling (ms epoch).
    pub mtime_before_ms: Option<i64>,
}

/// Render standalone L0 hits in the shared `path:line:col` shape, headed by
/// the coverage vocabulary (`rg_exhaustive` when the sweep completed,
/// `rg_truncated` when early-kill cut it at the limit). Context lines
/// follow indented.
#[must_use]
/// Render standalone L0 hits in the shared `path:line:col` shape, headed by
/// the coverage vocabulary (`rg_exhaustive` when the sweep completed,
/// `rg_truncated` when early-kill cut it at the limit). Context lines
/// follow indented.
pub fn render_rg_hits(hits: &[RgHit], limit: usize) -> String {
    let coverage = if hits.len() < limit {
        crate::mcp::Coverage::RgExhaustive
    } else {
        crate::mcp::Coverage::RgTruncated
    };
    let mut out = format!("coverage: {}\n", coverage.as_str());
    for hit in hits {
        out.push_str(&format!(
            "{}:{}:{}: {}\n",
            hit.path,
            hit.line,
            hit.column,
            hit.text.trim_end()
        ));
        for line in &hit.context_before {
            out.push_str(&format!("  {line}"));
            if !line.ends_with('\n') {
                out.push('\n');
            }
        }
        for line in &hit.context_after {
            out.push_str(&format!("  {line}"));
            if !line.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

/// Run the `search` command: recall shell (fused/single-route) or L0 rg.
/// Thin exit-wrapper over [`run_search`]. An absent limit (`None`) means
/// the mode default: exhaustive for L0 (zg parity — the full sweep is
/// millisecond-scale; an explicit `--limit` keeps the bounded contract),
/// [`CLI_DEFAULT_LIMIT`] for recall modes.
#[allow(clippy::too_many_arguments)]
/// Run the `search` command: recall shell (fused/single-route) or L0 rg.
/// Thin exit-wrapper over [`run_search`]. An absent limit (`None`) means
/// the mode default: exhaustive for L0 (zg parity — the full sweep is
/// millisecond-scale; an explicit `--limit` keeps the bounded contract),
/// [`CLI_DEFAULT_LIMIT`] for recall modes.
pub fn cmd_search(
    query: &str,
    workspace: &str,
    db_path: &str,
    limit: Option<usize>,
    fts: bool,
    vector: bool,
    rg: bool,
    fuse: bool,
    trace: bool,
    compact: bool,
    prefer_symbol: bool,
    globs: &[String],
    symbol_type_names: &[String],
    rg_options: &RgDisplayOptions,
) {
    match run_search(
        query,
        workspace,
        db_path,
        limit,
        fts,
        vector,
        rg,
        fuse,
        trace,
        compact,
        prefer_symbol,
        globs,
        symbol_type_names,
        rg_options,
    ) {
        Ok(rendered) => print!("{rendered}"),
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    }
}

/// Execute a search and return the rendered output (testable; no process exit
/// except on conflicting mode flags, which is a CLI contract violation).
#[allow(clippy::too_many_arguments)]
/// Execute a search and return the rendered output (testable; no process exit
/// except on conflicting mode flags, which is a CLI contract violation).
pub fn run_search(
    query: &str,
    workspace: &str,
    db_path: &str,
    limit: Option<usize>,
    fts: bool,
    vector: bool,
    rg: bool,
    fuse: bool,
    trace: bool,
    compact: bool,
    prefer_symbol: bool,
    globs: &[String],
    symbol_type_names: &[String],
    rg_options: &RgDisplayOptions,
) -> Result<String, String> {
    let mode = match resolve_mode(fts, vector, rg, fuse) {
        Ok(mode) => mode,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    let mut symbol_types = Vec::new();
    for name in symbol_type_names {
        symbol_types.push(parse_symbol_type(name)?);
    }
    // L3 embedder: resolved from the workspace project config
    // (`models.embed`, provider table, dims). Construction is offline;
    // absence degrades through the existing embedder-absent paths.
    // Explicit `--vector` without a backend is a named decline, never
    // a silent lemma fallback.
    let embedder = match mode {
        SearchMode::Fuse | SearchMode::Vector => {
            let cfg = guidance_core::config::load_config(std::path::Path::new(workspace))
                .unwrap_or_default();
            crate::embed::embedder_from_config(&cfg)
        }
        SearchMode::Fts | SearchMode::Rg => None,
    };
    if mode == SearchMode::Vector && embedder.is_none() {
        return Err(
            "search --vector needs an embedding backend: set models.embed \
             (e.g. \"llama:embed\") with its provider base_url and embed dims \
             in .guidance/guidance-config.json"
                .to_string(),
        );
    }
    // Per-mode limit default: L0 drains the sweep unless the caller
    // bounds it explicitly (line-budgeted early-kill dropped files
    // nondeterministically under parallel rg); recall modes keep the
    // CLI default. `usize::MAX` never trips the backend's
    // `hits.len() >= limit` kill, and renders as `rg_exhaustive`.
    let limit = match (mode, limit) {
        (SearchMode::Rg, None) => usize::MAX,
        (_, None) => CLI_DEFAULT_LIMIT,
        (_, Some(requested)) => clamp_limit(requested),
    };

    if mode == SearchMode::Rg {
        return run_rg_search(query, workspace, limit, rg_options);
    }

    let plan = build_search_plan(
        query,
        mode,
        limit,
        trace,
        prefer_symbol,
        globs,
        &symbol_types,
    );
    // The Fuse route runs the shared recall assembly below; single-route
    // modes keep their explicit embedder wiring (Fts: none, Vector: required).
    if mode == SearchMode::Fuse {
        return match recall_on_index(workspace, db_path, &plan) {
            Ok(hits) => {
                if hits.is_empty() {
                    return Ok("No results found.\n".to_string());
                }
                let stages = stages_from_hits(&hits);
                let format = if trace {
                    OutputFormat::Debug
                } else if compact {
                    OutputFormat::Compact
                } else {
                    OutputFormat::Markdown
                };
                let mut out = QueryEngine::format_stages(&stages, format);
                // Hash-verified citations over live bytes, rank order:
                // recall excerpts never travel without their receipts.
                out.push_str("\n### Citations\n");
                for cite in cite_hits(&hits, workspace) {
                    out.push_str("- ");
                    out.push_str(&cite);
                    out.push('\n');
                }
                Ok(out)
            }
            Err(error) => Err(error),
        };
    }

    let db_file = PathBuf::from(db_path);
    if !db_file.exists() {
        return Err(format!(
            "index not found at {db_path} (run `guidance index` first)"
        ));
    }
    let db = match GuidanceDb::open(&db_file) {
        Ok(db) => db,
        Err(error) => return Err(format!("cannot open index at {db_path}: {error}")),
    };
    let storage = GuidanceDbStorage::new(&db);
    let plan = build_search_plan(
        query,
        mode,
        limit,
        trace,
        prefer_symbol,
        globs,
        &symbol_types,
    );
    // Rule-lemmatizer pipeline (no model): the L2 route collapses
    // inflections at zero embedding cost; `None` degrades to L1-only.
    // Built lazily on lemma need — recall always needs it here.
    let nlp = LazyNlp::new();
    match run_recall(&plan, &storage, embedder.as_deref(), nlp.get()) {
        Ok(output) => {
            if output.hits.is_empty() {
                return Ok("No results found.\n".to_string());
            }
            let stages = stages_from_hits(&output.hits);
            let format = if trace {
                OutputFormat::Debug
            } else if compact {
                OutputFormat::Compact
            } else {
                OutputFormat::Markdown
            };
            Ok(QueryEngine::format_stages(&stages, format))
        }
        Err(error) => Err(format!("search failed: {error}")),
    }
}

/// Shared Fuse recall assembly: open the index at `db_path`, resolve the
/// embedder from the workspace config, and run a caller-built plan through
/// [`run_recall`]. Both `run_search` (Fuse path) and [`fused_hits`] enter
/// here — one assembly, one set of error strings — while plan construction
/// and rendering stay with the caller.
fn recall_on_index(
    workspace: &str,
    db_path: &str,
    plan: &SearchPlan,
) -> Result<Vec<SearchHit>, String> {
    let db_file = PathBuf::from(db_path);
    if !db_file.exists() {
        return Err(format!(
            "index not found at {db_path} (run `guidance index` first)"
        ));
    }
    let open_phase = crate::profile::Phase::new("recall.db_open");
    let db = match GuidanceDb::open(&db_file) {
        Ok(db) => db,
        Err(error) => return Err(format!("cannot open index at {db_path}: {error}")),
    };
    drop(open_phase);
    // L3 embedder: resolved from the workspace project config
    // (`models.embed`, provider table, dims). Construction is offline;
    // absence degrades through the existing embedder-absent paths.
    let cfg_phase = crate::profile::Phase::new("recall.config");
    let cfg =
        guidance_core::config::load_config(std::path::Path::new(workspace)).unwrap_or_default();
    let embedder = crate::embed::embedder_from_config(&cfg);
    drop(cfg_phase);
    let storage = GuidanceDbStorage::new(&db);
    // Rule-lemmatizer pipeline (no model): the L2 route collapses
    // inflections at zero embedding cost; `None` degrades to L1-only.
    // Built lazily on lemma need — recall always needs it here.
    let nlp_phase = crate::profile::Phase::new("recall.nlp_build");
    let nlp = LazyNlp::new();
    let nlp_ref = nlp.get();
    drop(nlp_phase);
    let _recall_phase = crate::profile::Phase::new("recall.run");
    match run_recall(plan, &storage, embedder.as_deref(), nlp_ref) {
        Ok(output) => Ok(output.hits),
        Err(error) => Err(format!("search failed: {error}")),
    }
}

/// Fused recall hits for programmatic consumers (the explain port):
/// the caller-default plan through the shared [`recall_on_index`]
/// assembly, no rendering.
pub fn fused_hits(
    query: &str,
    workspace: &str,
    db_path: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let plan = build_search_plan(query, SearchMode::Fuse, limit, false, false, &[], &[]);
    recall_on_index(workspace, db_path, &plan)
}

/// Context lines for programmatic consumers of fused hits (the
/// explain command and the MCP explain tool): the top-N fused files
/// seed the depth-1 expansion over the hydrated graph, and lines name
/// only files the hits table does not already cover — the budget pays
/// for new files, never repeats — capped at [`EXPLAIN_CONTEXT_BUDGET`].
/// Pure composition of [`anchor_files`], the shared hydration helper,
/// [`ContextEdge`] expansion, and [`build_context_lines`]: the one
/// context assembly, never a second builder. An empty anchor set
/// yields no lines; hydration failure is the caller's to report (the
/// CLI warns, the MCP tool degrades silently).
pub fn context_lines_for_hits(
    hits: &[SearchHit],
    db: &GuidanceDb,
    workspace: &str,
) -> Result<Vec<String>, String> {
    let anchors = anchor_files(hits, guidance_core::graph_index::CLOSURE_ANCHOR_FILES);
    if anchors.is_empty() {
        return Ok(Vec::new());
    }
    let graph = crate::index_cmd::hydrated_graph(db, &[workspace.to_string()])?;
    let mut edges = graph.expand_shallow(&anchors);
    let covered: std::collections::HashSet<&str> = hits
        .iter()
        .map(|hit| hit.file.absolute_path.as_str())
        .collect();
    edges.retain(|edge| !covered.contains(edge.file.as_str()));
    Ok(build_context_lines(
        &edges,
        EXPLAIN_CONTEXT_BUDGET,
        workspace,
    ))
}

/// Top-N distinct fused files (hit order) seeding the closure.
/// Pure; the cap is [`CLOSURE_ANCHOR_FILES`](guidance_core::graph_index::CLOSURE_ANCHOR_FILES).
#[must_use]
/// Top-N distinct fused files (hit order) seeding the closure.
/// Pure; the cap is [`CLOSURE_ANCHOR_FILES`](guidance_core::graph_index::CLOSURE_ANCHOR_FILES).
pub fn anchor_files(hits: &[SearchHit], count: usize) -> Vec<String> {
    let mut anchors = Vec::new();
    for hit in hits {
        let path = hit.file.absolute_path.clone();
        if !anchors.contains(&path) {
            anchors.push(path);
        }
        if anchors.len() >= count {
            break;
        }
    }
    anchors
}

/// Hash-verified citation for one fused hit: the cited range hashed
/// over live bytes (`rel:Lstart-Lend @ b3:hash`), or `unverified` when
/// the range cannot be read (never an invented hash). Whole-file
/// fragments cite the whole file. Pure I/O over the hit's own path.
#[must_use]
/// Hash-verified citation for one fused hit: the cited range hashed
/// over live bytes (`rel:Lstart-Lend @ b3:hash`), or `unverified` when
/// the range cannot be read (never an invented hash). Whole-file
/// fragments cite the whole file. Pure I/O over the hit's own path.
pub fn cite_hit(hit: &SearchHit, workspace: &str) -> String {
    let range = hit
        .evidence
        .iter()
        .find_map(|evidence| match evidence.range {
            guidance_core::search_types::FragmentSpan::Text {
                start_line,
                end_line,
                ..
            } => Some((start_line as usize, end_line as usize)),
            guidance_core::search_types::FragmentSpan::File => Some((1, usize::MAX)),
            _ => None,
        });
    let Some((start, end)) = range else {
        return "unverified".to_string();
    };
    let Ok(bytes) = std::fs::read(&hit.file.absolute_path) else {
        return "unverified".to_string();
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() || start == 0 || start > lines.len() {
        return "unverified".to_string();
    }
    let end = end.min(lines.len());
    let cited = lines[start - 1..end].join("\n") + "\n";
    let hash = common_core::hash::blake3_hex(cited.as_bytes());
    let shown = PathBuf::from(&hit.file.absolute_path)
        .strip_prefix(workspace)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| hit.file.absolute_path.clone());
    format!("{shown}:{}-{end} @ b3:{hash}", start.min(end))
}

/// Citations for a hit list, rank order. Length always matches `hits`.
#[must_use]
/// Citations for a hit list, rank order. Length always matches `hits`.
pub fn cite_hits(hits: &[SearchHit], workspace: &str) -> Vec<String> {
    hits.iter().map(|hit| cite_hit(hit, workspace)).collect()
}

/// Absolute paths behind a hit list whose sidecar is missing or not
/// newer than the source: the mirror has not caught up, so rows from
/// these files are stale. The sidecar mapping is per source root.
#[must_use]
/// Absolute paths behind a hit list whose sidecar is missing or not
/// newer than the source: the mirror has not caught up, so rows from
/// these files are stale. The sidecar mapping is per source root.
pub fn stale_hit_sources(
    hits: &[SearchHit],
    guidance_dir: &str,
    src_dirs: &[PathBuf],
) -> Vec<String> {
    let mut out = Vec::new();
    for hit in hits {
        let abs = PathBuf::from(&hit.file.absolute_path);
        let stale = src_dirs
            .iter()
            .find(|root| {
                abs.strip_prefix(root).is_ok() || root.as_path() == std::path::Path::new(".")
            })
            .map(|root| {
                let rel = abs.strip_prefix(root).unwrap_or(&abs);
                PathBuf::from(guidance_dir)
                    .join("src")
                    .join(format!("{}.json", rel.display()))
            })
            .is_none_or(|sidecar| {
                let side_mtime = common_core::io::mtime(&sidecar);
                let source_mtime = common_core::io::mtime(&abs);
                match (side_mtime, source_mtime) {
                    (Some(side), Some(source)) => side <= source,
                    _ => true,
                }
            });
        if stale && !out.contains(&hit.file.absolute_path) {
            out.push(hit.file.absolute_path.clone());
        }
    }
    out
}

/// Test-only hit over a real file with one text evidence range.
#[cfg(test)]
#[must_use]
/// Test-only hit over a real file with one text evidence range.
pub fn hit_for_test(abs: &str, rel: &str, start_line: u32, end_line: u32) -> SearchHit {
    use guidance_core::search_types::{
        Entity, FileInfo, FragmentContent, FragmentSpan, RecallPath, SearchHitEvidence,
        SearchMatchedBy,
    };
    SearchHit {
        entity: Entity {
            id: abs.to_string(),
            file_id: abs.to_string(),
            range: FragmentSpan::File,
            content: FragmentContent::Text {
                text: String::new(),
            },
            metadata: None,
        },
        file: FileInfo {
            id: abs.to_string(),
            absolute_path: abs.to_string(),
            relative_path: rel.to_string(),
            root_path: String::new(),
            size_bytes: 0,
            last_modified_time: 0,
            content_hash: None,
            kind: None,
            format: "rust".to_string(),
            index_status: None,
        },
        evidence: vec![SearchHitEvidence {
            range: FragmentSpan::Text {
                start_line,
                end_line,
                start_offset: 0,
                end_offset: 0,
            },
            content: FragmentContent::Text {
                text: String::new(),
            },
            metadata: None,
            is_entity: true,
            path: RecallPath::Fts,
            route_id: None,
            query: None,
            rank: None,
            score: None,
            forced: None,
        }],
        rank: 1,
        score: guidance_core::search_types::RrfScore::new(0.05),
        matched_by: SearchMatchedBy::Fts,
        trace: None,
    }
}

/// Build explain rows: one per fused hit, rank order. The table carries
/// hits only — expansion never perturbs it (additive default); context
/// renders separately below. Cites pair by index (short lists fill
/// `unverified`, never shift rows). Pure.
#[must_use]
/// Build explain rows: one per fused hit, rank order. The table carries
/// hits only — expansion never perturbs it (additive default); context
/// renders separately below. Cites pair by index (short lists fill
/// `unverified`, never shift rows). Pure.
pub fn build_explain_rows(hits: &[SearchHit], cites: &[String]) -> Vec<ExplainRow> {
    hits.iter()
        .enumerate()
        .map(|(index, hit)| {
            // File-level fragments carry no symbol metadata, so the
            // shared title rule falls back to the entity id (the
            // absolute path). Render those as basenames — CLI-only
            // fallback, the MCP title rule itself stays untouched.
            let title = crate::mcp::hit_title(hit);
            let name = if title == hit.file.absolute_path {
                basename_of(&hit.file.relative_path)
            } else {
                title
            };
            ExplainRow {
                name,
                source: hit.file.relative_path.clone(),
                // Lossy by design (see `ExplainRow`): the ordinal leaves
                // the ranking pipeline here and becomes display text.
                score: hit.score.value(),
                cite: cites
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| "unverified".to_string()),
            }
        })
        .collect()
}

/// Grouped context provenance: one line per (file, anchor, direction),
/// families merged. Keeps the builder below within type-complexity bounds.
type ContextGroup<'a> = (
    (&'a str, &'a str, ContextDirection),
    Vec<(ContextFamily, &'a str, ResolutionMethod)>,
);

/// Build ranked context lines from depth-1 expansion edges: one line
/// per (file, anchor, direction), families merged (`import "s", call
/// "c"`) so provenance stays complete without doubling lines. Sorted
/// edge order, capped at `cap` lines. Pure.
#[must_use]
/// Build ranked context lines from depth-1 expansion edges: one line
/// per (file, anchor, direction), families merged (`import "s", call
/// "c"`) so provenance stays complete without doubling lines. Sorted
/// edge order, capped at `cap` lines. Pure.
pub fn build_context_lines(edges: &[ContextEdge], cap: usize, workspace: &str) -> Vec<String> {
    // Group by (file, anchor, direction), merging families in sort order.
    let mut groups: Vec<ContextGroup<'_>> = Vec::new();
    for edge in edges {
        let key = (edge.file.as_str(), edge.anchor.as_str(), edge.direction);
        match groups.iter_mut().find(|(group, _)| *group == key) {
            Some((_, members)) => members.push((edge.family, edge.via.as_str(), edge.resolution)),
            None => groups.push((key, vec![(edge.family, edge.via.as_str(), edge.resolution)])),
        }
    }
    groups
        .into_iter()
        .take(cap)
        .map(|((file, anchor, direction), mut members)| {
            members.sort();
            let direction = match direction {
                ContextDirection::Dependent => "dependent of",
                ContextDirection::Dependency => "dependency of",
            };
            let vias: Vec<String> = members
                .iter()
                .map(|(family, via, resolution)| {
                    let family = match family {
                        ContextFamily::Import => "import",
                        ContextFamily::Call => "call",
                    };
                    match resolution.render_note() {
                        Some(note) => format!("{family} \"{via}\" ({note})"),
                        None => format!("{family} \"{via}\""),
                    }
                })
                .collect();
            format!(
                "- {} — {direction} {} via {}",
                relativize(file, workspace),
                relativize(anchor, workspace),
                vias.join(", ")
            )
        })
        .collect()
}

/// Workspace-relative rendering (absolute fallback when the file escapes
/// the workspace — never a panic on prefixes).
fn relativize(path: &str, workspace: &str) -> String {
    std::path::Path::new(path)
        .strip_prefix(workspace)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

fn basename_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// One rendered explain row: member/symbol title, file path, score,
/// hash-verified citation over live bytes.
#[derive(Debug, Clone, PartialEq)]
/// One rendered explain row: member/symbol title, file path, score,
/// hash-verified citation over live bytes.
pub struct ExplainRow {
    /// Symbol/member title (entity id fallback).
    pub name: String,
    /// Workspace-relative file path (absolute fallback).
    pub source: String,
    /// Row score: the fused RRF ordinal (positions, not a magnitude);
    /// kept on the row for threshold decisions, never rendered.
    pub score: f64,
    /// Verified cite (`rel:Lstart-Lend @ b3:hash`) or `unverified`.
    pub cite: String,
}

/// Symbol identity behind one fused hit: the first evidence (else the
/// entity) carrying code metadata with a declared name.
#[must_use]
/// Symbol identity behind one fused hit: the first evidence (else the
/// entity) carrying code metadata with a declared name.
pub fn hit_symbol(hit: &SearchHit) -> Option<(String, CodeSymbolType)> {
    use guidance_core::search_types::EntityMetadata;
    let from = |meta: &EntityMetadata| match meta {
        EntityMetadata::Code {
            symbol_name: Some(name),
            symbol_type,
            ..
        } => Some((name.clone(), *symbol_type)),
        _ => None,
    };
    hit.evidence
        .iter()
        .filter_map(|evidence| evidence.metadata.as_ref().and_then(from))
        .next()
        .or_else(|| hit.entity.metadata.as_ref().and_then(from))
}

/// Tree-sitter member kind for one closed symbol type. Unmatched kinds
/// (`Value`, `Alias`, `Module`) decline: callers fall back to the
/// stored evidence range instead of guessing a span.
#[must_use]
/// Tree-sitter member kind for one closed symbol type.
pub fn member_type_for_symbol(symbol: CodeSymbolType) -> Option<fluent_types::MemberType> {
    use fluent_types::MemberType;
    match symbol {
        CodeSymbolType::Function => Some(MemberType::FnDecl),
        CodeSymbolType::Class | CodeSymbolType::Interface => Some(MemberType::Struct),
        _ => None,
    }
}

/// Live line bounds for one hit, resolved by parsing the current file
/// with tree-sitter on the fly. Stored ranges go stale when the file is
/// edited after indexing; the parsed span never does. `None` when the
/// hit carries no symbol or the symbol is gone.
#[must_use]
/// Live line bounds for one hit, resolved by parsing the current file.
pub fn live_hit_lines(hit: &SearchHit, workspace: &std::path::Path) -> Option<(u32, u32)> {
    // Workspace is accepted for a uniform call signature; resolution
    // reads the hit's absolute path directly.
    let _ = workspace;
    let (name, symbol) = hit_symbol(hit)?;
    let member = member_type_for_symbol(symbol)?;
    let path = PathBuf::from(&hit.file.absolute_path);
    guidance_core::ast_parser::resolve_span(&path, &name, member)
        .map(|span| (span.start_line, span.end_line))
}

/// Stored evidence range for one hit (line bounds only), read live.
/// The fallback when a hit carries no symbol metadata.
fn stored_hit_lines(hit: &SearchHit) -> Option<(u32, u32)> {
    use guidance_core::search_types::FragmentSpan;
    hit.evidence
        .iter()
        .find_map(|evidence| match evidence.range {
            FragmentSpan::Text {
                start_line,
                end_line,
                ..
            } => Some((start_line, end_line)),
            FragmentSpan::File => Some((1, 1)),
            _ => None,
        })
}

/// Live citation for one hit, in `path : lines start-end` form over
/// live bytes resolved by tree-sitter on the fly. No hashes, no
/// verdicts — just where to look right now. A recorded symbol that no
/// longer parses cites as moved or removed; hits without symbol
/// metadata cite their stored range; unreadable files cite
/// `unverified`.
#[must_use]
/// Live citation for one hit: absolute path plus live line range.
pub fn cite_live(hit: &SearchHit, workspace: &std::path::Path) -> String {
    let abs = hit.file.absolute_path.clone();
    if let Some((_, symbol)) = hit_symbol(hit) {
        if member_type_for_symbol(symbol).is_some() {
            if let Some((start, end)) = live_hit_lines(hit, workspace) {
                return format!("- {abs} : lines {start}-{end}");
            }
            if std::path::Path::new(&abs).exists() {
                return format!("- {abs} : moved or removed since index");
            }
        }
    }
    match stored_hit_lines(hit).filter(|_| std::path::Path::new(&abs).exists()) {
        Some((start, end)) => format!("- {abs} : lines {start}-{end}"),
        None => format!("- {abs} : unverified"),
    }
}

/// Display bounds for dedup: live tree-sitter positions for real
/// symbols, stored bounds otherwise, `None` when the symbol is gone
/// (the marker line dedupes by exact text instead).
fn cite_bounds(hit: &SearchHit, workspace: &std::path::Path) -> Option<(u32, u32)> {
    if let Some((_, symbol)) = hit_symbol(hit) {
        if member_type_for_symbol(symbol).is_some() {
            return live_hit_lines(hit, workspace);
        }
    }
    stored_hit_lines(hit)
}

/// Live citations for fused hits, rank order, dual rows collapsed: the
/// same (canonical file, bounds) cites once, identical markers cite
/// once, and bare `unverified` lines never collapse (they carry no
/// file identity to compare).
#[must_use]
/// Live citations for fused hits, rank order, deduped.
pub fn cite_hits_live(hits: &[SearchHit], workspace: &std::path::Path) -> Vec<String> {
    use std::collections::HashSet;
    let mut seen_keys: HashSet<(String, u32, u32)> = HashSet::new();
    let mut seen_lines: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for hit in hits {
        if let Some((start, end)) = cite_bounds(hit, workspace) {
            if !seen_keys.insert((canonical_hit_path(hit, workspace), start, end)) {
                continue;
            }
        }
        let line = cite_live(hit, workspace);
        if line != "unverified" && !seen_lines.insert(line.clone()) {
            continue;
        }
        out.push(line);
    }
    out
}

/// Human drill-downs from the top snippets: one `callers of` query per
/// named code symbol, capped. Module rows (`$file`) and non-code hits
/// (docs, prose) carry no callable symbol, so they never suggest.
#[must_use]
/// Human drill-downs from the top snippets: one per named code symbol.
pub fn suggest_drill_downs(
    snippets: &[(String, String, u32, u32, String)],
    limit: usize,
) -> Vec<String> {
    fn is_code(path: &str) -> bool {
        matches!(
            std::path::Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str()),
            Some("rs") | Some("zig") | Some("py")
        )
    }
    snippets
        .iter()
        .filter(|(name, file, _, _, _)| !name.starts_with('$') && is_code(file))
        .take(limit)
        .map(|(name, _, _, _, _)| format!("guidance explain \"callers of {name}\""))
        .collect()
}

/// Minimum decision score (0-100) for a doc paragraph to be cited.
pub const EXPLAIN_DOC_MIN_SCORE: u32 = 60;

/// Doc paragraphs judged per call: bounds decision cost.
pub const EXPLAIN_DOC_JUDGE_K: usize = 5;

/// Decision-gated doc paragraphs: the local decision model scores each
/// candidate 0-100 and only items at or above the floor survive, best
/// score first. Unparseable or failed judgments omit everything —
/// never a cited paragraph the judge did not pass.
pub fn judge_doc_paragraphs(
    query: &str,
    docs: &[(String, String)],
    backend: &dyn fluent_llm::client::ChatBackend,
) -> Vec<(String, String)> {
    if docs.is_empty() {
        return Vec::new();
    }
    let mut body = format!("Question: {query}\n\nParagraphs:\n");
    for (index, (path, excerpt)) in docs.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(body, "{}. {path}\n{excerpt}\n\n", index + 1);
    }
    let messages = vec![
        fluent_llm::protocol::ChatMessage::with_role(
            fluent_llm::roles::ChatRole::System,
            "Rate how relevant each paragraph is to the question, 0-100. \
             Respond with ONLY a strict JSON object mapping 1-based paragraph \
             number to integer score, e.g. {\"1\": 85}.",
        ),
        fluent_llm::protocol::ChatMessage::with_role(fluent_llm::roles::ChatRole::User, body),
    ];
    let extras = serde_json::json!({
        "temperature": 0.0,
        "max_tokens": 64,
    });
    let text = match backend.chat_complete_with_extras(&messages, &extras) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    let scores: std::collections::BTreeMap<String, serde_json::Value> =
        serde_json::from_str(text.trim()).unwrap_or_default();
    let mut kept: Vec<(u64, usize)> = Vec::new();
    for (key, value) in &scores {
        let Ok(one_based) = key.parse::<usize>() else {
            continue;
        };
        let Some(score) = value.as_u64() else {
            continue;
        };
        if one_based >= 1 && one_based <= docs.len() && score >= u64::from(EXPLAIN_DOC_MIN_SCORE) {
            kept.push((score, one_based - 1));
        }
    }
    kept.sort_by(|a, b| b.0.cmp(&a.0));
    kept.into_iter()
        .map(|(_, index)| docs[index].clone())
        .collect()
}

/// Code snippet for one top hit with live bounds: the node's current
/// span when symbol metadata resolves, else the stored evidence range
/// sliced from live bytes. Capped to [`EXPLAIN_SNIPPET_MAX_LINES`];
/// unreadable files yield nothing, never a stale guess.
#[must_use]
/// Code snippet for one top hit with live bounds, capped.
pub fn snippet_for_hit(hit: &SearchHit, workspace: &std::path::Path) -> Option<(String, u32, u32)> {
    let max_lines = EXPLAIN_SNIPPET_MAX_LINES;
    if let Some((name, symbol)) = hit_symbol(hit) {
        if let Some(member) = member_type_for_symbol(symbol) {
            if let Some((snippet, start, end)) = guidance_core::ast_parser::extract_code_snippet(
                &hit.file.relative_path,
                &name,
                member,
                workspace,
                max_lines,
            ) {
                return Some((snippet, start, end));
            }
        }
    }
    let (mut start, mut end) = stored_hit_lines(hit)?;
    let bytes = std::fs::read(&hit.file.absolute_path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }
    start = start.max(1).min(lines.len() as u32);
    end = end
        .clamp(start, lines.len() as u32)
        .min(start + max_lines as u32 - 1);
    let snippet = lines[(start as usize - 1)..(end as usize)].join("\n");
    if snippet.trim().is_empty() {
        return None;
    }
    Some((snippet, start, end))
}

/// Ranked doc paragraphs for one query over a markdown tree: walk once
/// (capped), split blank-line paragraphs, score by shared query-token
/// overlap. Returns `(doc-rel-path, excerpt)` in rank order. Pure file
/// I/O, no index writes — the docs stay outside the fragment index.
#[must_use]
/// Ranked doc paragraphs for one query over a markdown tree.
pub fn rank_doc_paragraphs(
    query: &str,
    docs_dir: &std::path::Path,
    workspace: &std::path::Path,
    limit: usize,
) -> Vec<(String, String)> {
    use search_vector::tokens::split_tokens;
    let query_tokens: Vec<String> = {
        let mut tokens = Vec::new();
        for token in split_tokens(query) {
            search_vector::tokens::lower_dedup_push(&mut tokens, &token);
        }
        tokens
    };
    if query_tokens.is_empty() {
        return Vec::new();
    }
    let mut files = Vec::new();
    let mut total_bytes: u64 = 0;
    collect_markdown(docs_dir, &mut files, &mut total_bytes);
    let mut scored: Vec<(usize, String, String)> = Vec::new();
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for (index, paragraph) in text.split("\n\n").enumerate() {
            let paragraph = paragraph.trim();
            if paragraph.len() < 40 {
                continue;
            }
            let overlap = split_tokens(paragraph)
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
                .intersection(&query_tokens.iter().cloned().collect())
                .count();
            if overlap == 0 {
                continue;
            }
            let rel = path
                .strip_prefix(workspace)
                .map(|rel| rel.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            let mut end = paragraph.len().min(EXPLAIN_DOC_EXCERPT_CHARS);
            while !paragraph.is_char_boundary(end) {
                end -= 1;
            }
            scored.push((
                overlap,
                format!("{rel}#{}", index),
                paragraph[..end].to_string(),
            ));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, path, excerpt)| (path, excerpt))
        .collect()
}

/// One recursive markdown walk, bounded by file and byte caps.
fn collect_markdown(dir: &std::path::Path, out: &mut Vec<PathBuf>, total_bytes: &mut u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    names.sort();
    for path in names {
        if path.is_dir() {
            if out.len() < EXPLAIN_DOC_MAX_FILES {
                collect_markdown(&path, out, total_bytes);
            }
            continue;
        }
        if out.len() >= EXPLAIN_DOC_MAX_FILES || *total_bytes >= EXPLAIN_DOC_MAX_BYTES {
            return;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > 100_000 {
                continue;
            }
            *total_bytes += meta.len();
        }
        out.push(path);
    }
}

/// Answer prompt: the question plus fenced excerpts as the only
/// evidence. The model must answer concisely from the excerpts and
/// cite every claim as `file:line`.
#[must_use]
/// Answer prompt: the question plus fenced excerpts as the only evidence.
pub fn answer_messages(
    query: &str,
    context: &[(String, String, u32, u32, String)],
) -> Vec<fluent_llm::protocol::ChatMessage> {
    use fluent_llm::protocol::ChatMessage;
    use fluent_llm::roles::ChatRole;
    let system = ChatMessage::with_role(
        ChatRole::System,
        "You answer questions about a codebase using ONLY the provided excerpts. \
         Be concise (150 words or fewer). Cite every claim as file:line. \
         If the excerpts do not answer the question, say so.",
    );
    let mut body = format!("Question: {query}\n\nExcerpts:\n");
    for (name, file, start, end, snippet) in context {
        use std::fmt::Write as _;
        let _ = write!(
            body,
            "## {name} ({file}:{start}-{end})\n```code\n{snippet}\n```\n"
        );
    }
    vec![system, ChatMessage::with_role(ChatRole::User, body)]
}

/// Concise model answer over the excerpt context. `None` when there is
/// no context or the model says nothing: the caller renders the
/// unavailable line instead of an invented answer.
pub fn answer_with_backend(
    query: &str,
    context: &[(String, String, u32, u32, String)],
    backend: &dyn fluent_llm::client::ChatBackend,
    model: &str,
) -> Result<Option<String>, String> {
    if context.is_empty() {
        return Ok(None);
    }
    let messages = answer_messages(query, context);
    let extras = serde_json::json!({
        "temperature": 0.0,
        "max_tokens": EXPLAIN_ANSWER_MAX_TOKENS,
    });
    let text = backend
        .chat_complete_with_extras(&messages, &extras)
        .map_err(|error| error.to_string())?;
    let _ = model;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed))
}

/// Top snippets for fused hits, rank order, byte-identical duplicates
/// removed: the first occurrence of one file plus live bounds wins, the
/// rest add no new evidence. Headers reuse the row titles so citations
/// cross-reference the snippets.
#[must_use]
/// Top snippets for fused hits, deduped.
pub fn top_snippets(
    rows: &[ExplainRow],
    hits: &[SearchHit],
    workspace: &std::path::Path,
) -> Vec<(String, String, u32, u32, String)> {
    use std::collections::HashSet;
    let mut seen: HashSet<(String, u32, u32)> = HashSet::new();
    let mut snippets = Vec::new();
    for (index, hit) in hits.iter().enumerate() {
        if snippets.len() >= EXPLAIN_SNIPPET_TOP_K {
            break;
        }
        // Display bounds once per hit: live tree-sitter positions for
        // real symbols, stored bounds otherwise.
        let bounds = match hit_symbol(hit) {
            Some((_, symbol)) if member_type_for_symbol(symbol).is_some() => {
                live_hit_lines(hit, workspace)
            }
            _ => stored_hit_lines(hit),
        };
        let Some((start, end)) = bounds else {
            continue;
        };
        // Identity is the canonical absolute path: dual rows (one
        // indexed under a relative workspace, one absolute) are the
        // same file and must not both render.
        if !seen.insert((canonical_hit_path(hit, workspace), start, end)) {
            continue;
        }
        if let Some((text, _, _)) = snippet_for_hit(hit, workspace) {
            let Some(row) = rows.get(index) else {
                continue;
            };
            snippets.push((row.name.clone(), row.source.clone(), start, end, text));
        }
    }
    snippets
}

/// Canonical identity for one hit's file: absolute paths verbatim,
/// workspace-relative rows joined to the workspace, `/./` and interior
/// `..` collapsed lexically. Dual rows from mixed relative/absolute
/// invocations resolve to one identity without touching stored ids.
fn canonical_hit_path(hit: &SearchHit, workspace: &std::path::Path) -> String {
    use std::path::Component;
    let abs = PathBuf::from(&hit.file.absolute_path);
    let joined = if abs.is_absolute() {
        abs
    } else {
        workspace.join(abs)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(component.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

/// Render the full explain output in reference order: answer, docs,
/// top snippets (fenced, blank-line separated), table, citations,
/// drill-down footer. Empty recall keeps the legacy empty state
/// exactly: no sections on nothing.
pub fn render_explain_full(
    query: &str,
    rows: &[ExplainRow],
    snippets: &[(String, String, u32, u32, String)],
    citations: &[String],
    docs: &[(String, String)],
    answer: Option<&str>,
    suggestions: &[String],
) -> String {
    let mut out = format!("## Explain: {query}\n\n");
    if rows.is_empty() {
        out.push_str("No results found.\n");
        return out;
    }
    if let Some(text) = answer {
        out.push_str("### Answer\n");
        out.push_str(text);
        out.push('\n');
    }
    if !docs.is_empty() {
        out.push_str("\n### From the docs\n");
        for (path, excerpt) in docs {
            out.push_str(&format!("- {path}: {excerpt}\n"));
        }
    }
    if !snippets.is_empty() {
        out.push_str("\n### Top matches\n");
        for (index, (name, file, start, end, snippet)) in snippets.iter().enumerate() {
            let lang = match std::path::Path::new(file)
                .extension()
                .and_then(|ext| ext.to_str())
            {
                Some("rs") => "rust",
                Some("zig") => "zig",
                Some("py") => "py",
                Some("md") => "md",
                _ => "",
            };
            out.push_str(&format!(
                "#### {}. {name} — {file}:{start}-{end}\n",
                index + 1
            ));
            out.push_str(&format!("```{lang}\n{snippet}\n```\n\n"));
        }
    }
    if !citations.is_empty() {
        out.push_str("\n### Citations\n");
        for cite in citations {
            out.push_str(cite);
            out.push('\n');
        }
    }
    if !suggestions.is_empty() {
        out.push_str("\nIf you need details, try:\n");
        for suggestion in suggestions {
            out.push_str(&format!("- {suggestion}\n"));
        }
    }
    out
}

/// Standalone L0 search: managed rg under an `FsCapability` scope, rendered
/// Operator CLI enters the scope explicitly — backends stay fail-closed.
fn run_rg_search(
    query: &str,
    workspace: &str,
    limit: usize,
    rg_options: &RgDisplayOptions,
) -> Result<String, String> {
    let root = PathBuf::from(workspace);
    if !root.is_dir() {
        return Err(format!("workspace not found: {workspace}"));
    }
    let backend = RgBackend {
        root: root.clone(),
        exe_override: None,
        path_override: None,
    };
    let rg_query = query.to_string();
    let rg_limit = limit;
    let before = rg_options.before;
    let after = rg_options.after;
    let mtime_after_ms = rg_options.mtime_after_ms;
    let mtime_before_ms = rg_options.mtime_before_ms;
    let caps = fluent_concurrency::capability::default_capability_set();
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(fluent_concurrency::scope::CURRENT_CAPS.scope(
            caps,
            async move {
                let options = RgOptions {
                    context_before: before,
                    context_after: after,
                    mtime_after_ms,
                    mtime_before_ms,
                };
                let hits = backend.search_with_options(&rg_query, rg_limit, &options)?;
                Ok::<_, guidance_core::query::rg_backend::RgError>(hits)
            },
        ))
    });
    match result {
        Ok(hits) => {
            if hits.is_empty() {
                Ok("No results found.\n".to_string())
            } else {
                Ok(render_rg_hits(&hits, limit))
            }
        }
        Err(error) => Err(format!("rg search failed: {error}")),
    }
}

#[cfg(test)]
#[path = "../tests/search_cmd.rs"]
mod tests;
