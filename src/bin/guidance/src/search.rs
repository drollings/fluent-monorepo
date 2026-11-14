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

/// Collapse dual-indexed rows at the recall merge: hits sharing one
/// canonical file identity and one entity range render once (the
/// first keeps its rank and score). Provenance merges — extra
/// evidence and recall traces ride along — so the collapse is
/// visible only in trace output, never as a second row. Distinct
/// files and distinct spans never collapse. Pure; reuses the
/// shared `canonical_hit_path` file rule (same module, no second
/// normalizer).
#[must_use]
/// Collapse dual-indexed rows at the recall merge: hits sharing one
/// canonical file identity and one entity range render once (the
/// first keeps its rank and score). Provenance merges — extra
/// evidence and recall traces ride along — so the collapse is
/// visible only in trace output, never as a second row. Distinct
/// files and distinct spans never collapse. Pure; reuses the
/// shared `canonical_hit_path` file rule (same module, no second
/// normalizer).
pub fn dedupe_dual_hits(hits: Vec<SearchHit>, workspace: &std::path::Path) -> Vec<SearchHit> {
    let mut out: Vec<SearchHit> = Vec::with_capacity(hits.len());
    let mut keys: Vec<(String, guidance_core::search_types::FragmentSpan)> = Vec::new();
    for hit in hits {
        let key = (
            canonical_hit_path(&hit, workspace),
            hit.entity.range.clone(),
        );
        match keys.iter().position(|known| *known == key) {
            Some(index) => {
                let target = &mut out[index];
                for evidence in hit.evidence {
                    if !target
                        .evidence
                        .iter()
                        .any(|known| known.range == evidence.range && known.path == evidence.path)
                    {
                        target.evidence.push(evidence);
                    }
                }
                match (&mut target.trace, hit.trace) {
                    (Some(target_trace), Some(source_trace)) => {
                        for entry in source_trace.recall {
                            if !target_trace.recall.iter().any(|known| {
                                known.path == entry.path && known.route_id == entry.route_id
                            }) {
                                target_trace.recall.push(entry);
                            }
                        }
                    }
                    (None, Some(source_trace)) => {
                        target.trace = Some(source_trace);
                    }
                    _ => {}
                }
            }
            None => {
                keys.push(key);
                out.push(hit);
            }
        }
    }
    out
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
        Ok(output) => Ok(dedupe_dual_hits(
            output.hits,
            std::path::Path::new(workspace),
        )),
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
    // Identifier-extracting routes ride along: queries naming a symbol
    // get symbol-filtered candidates fused into rank, so the defining
    // file surfaces instead of drowning in prose matches. Queries
    // without identifiers build byte-identical plans.
    let plan = build_search_plan(query, SearchMode::Fuse, limit, false, true, &[], &[]);
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
        // Sidecars mirror their source root: absolutize both sides so a
        // relative workspace still prefixes absolute hit paths, then map
        // through the same `guidance_dir/src/<rel>.json` shape the
        // pipeline writes.
        let stale = src_dirs
            .iter()
            .find(|root| {
                let root_abs = guidance_core::sync_engine::absolute_or((*root).clone());
                abs.strip_prefix(&root_abs).is_ok()
            })
            .map(|root| {
                let root_abs = guidance_core::sync_engine::absolute_or(root.clone());
                let rel = abs.strip_prefix(&root_abs).unwrap_or(&abs);
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
    let mut seen_unresolved: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for hit in hits {
        if let Some((start, end)) = cite_bounds(hit, workspace) {
            if !seen_keys.insert((canonical_hit_path(hit, workspace), start, end)) {
                continue;
            }
        } else {
            // No live bounds (moved, removed, or unreadable): the key
            // above never fires, so dual rows cite the same file twice —
            // collapse on canonical identity instead.
            if !seen_unresolved.insert(canonical_hit_path(hit, workspace)) {
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

/// Parse a `callers of <symbol>` query: case-insensitive prefix,
/// surrounding whitespace and quotes trimmed. `Some` carries the
/// symbol (`""` when nothing follows — an unknown, never a keyword
/// fallback); `None` means an ordinary query.
#[must_use]
/// Parse a `callers of <symbol>` query: case-insensitive prefix,
/// surrounding whitespace and quotes trimmed. `Some` carries the
/// symbol (`""` when nothing follows — an unknown, never a keyword
/// fallback); `None` means an ordinary query.
pub fn parse_callers_query(query: &str) -> Option<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_lowercase();
    let prefix = "callers of";
    if lowered == prefix {
        return Some(String::new());
    }
    let rest = lowered
        .strip_prefix(prefix)
        .filter(|tail| tail.starts_with(char::is_whitespace))?;
    let raw = trimmed[prefix.len()..].trim();
    if raw.is_empty() {
        return Some(String::new());
    }
    let _ = rest;
    let unquoted = raw
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            raw.strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        })
        .unwrap_or(raw);
    Some(unquoted.trim().to_string())
}

/// Callers of one symbol over persisted call edges: exact callee-name
/// match, sorted by (caller file, caller symbol) for determinism.
/// Homonyms expand (every caller of the name); unknown or empty
/// symbols yield nothing. Pure over the edge slice.
#[must_use]
/// Callers of one symbol over persisted call edges: exact callee-name
/// match, sorted by (caller file, caller symbol) for determinism.
/// Homonyms expand (every caller of the name); unknown or empty
/// symbols yield nothing. Pure over the edge slice.
pub fn callers_for_symbol(
    edges: &[guidance_core::graph_index::CallEdge],
    symbol: &str,
) -> Vec<(
    String,
    Option<String>,
    guidance_core::graph_index::ResolutionMethod,
)> {
    if symbol.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(
        String,
        Option<String>,
        guidance_core::graph_index::ResolutionMethod,
    )> = edges
        .iter()
        .filter(|edge| edge.callee == symbol)
        .map(|edge| {
            (
                edge.caller_file.clone(),
                edge.caller_symbol.clone(),
                edge.resolution,
            )
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Graph-backed hits for a `callers of` symbol: hydrate from persisted
/// rows (no file reads, no parsing), resolve callers, and materialize
/// one hit per caller file/symbol for the shared citation machinery.
/// Unknown symbols yield no hits (honest empty upstream), never a
/// keyword search.
pub fn graph_callers_hits(
    db: &GuidanceDb,
    workspace: &str,
    symbol: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    use guidance_core::search_types::{
        CodeSymbolType, Entity, EntityMetadata, FileInfo, FragmentContent, FragmentSpan,
        RecallPath, SearchHitEvidence, SearchMatchedBy,
    };
    if symbol.is_empty() {
        return Ok(Vec::new());
    }
    let graph = crate::index_cmd::hydrated_graph(db, &[workspace.to_string()])
        .map_err(|error| format!("graph hydration failed ({error})"))?;
    let callers = callers_for_symbol(graph.call_edges(), symbol);
    if callers.is_empty() {
        return Ok(Vec::new());
    }
    let files = db
        .list_files()
        .map_err(|error| format!("list files: {error}"))?;
    let by_path: std::collections::HashMap<&str, &search_vector::db::FileRecord> = files
        .iter()
        .map(|record| (record.absolute_path.as_str(), record))
        .collect();
    let mut hits = Vec::new();
    for (index, (caller_file, caller_symbol, _)) in
        callers.into_iter().take(limit.max(1)).enumerate()
    {
        let record = by_path.get(caller_file.as_str());
        let (file_id, relative_path, root_path) = match record {
            Some(record) => (
                record.id.clone(),
                record.relative_path.clone(),
                record.root_path.clone(),
            ),
            None => (
                caller_file.clone(),
                std::path::Path::new(&caller_file)
                    .strip_prefix(workspace)
                    .map(|rel| rel.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| caller_file.clone()),
                workspace.to_string(),
            ),
        };
        let format = std::path::Path::new(&caller_file)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_string();
        // Persisted range for the caller symbol's owning fragment;
        // otherwise the whole file (live resolution still pins lines).
        // The symbols table carries identity only — the span reads from
        // the fragment row, which keeps its range.
        let mut range = FragmentSpan::File;
        if let (Some(record), Some(name)) = (record, caller_symbol.as_deref()) {
            if let Ok(Some((_, range_json))) = db.symbol_fragment(&record.id, name, None) {
                range = serde_json::from_str(&range_json).unwrap_or(FragmentSpan::File);
            }
        }
        let metadata = caller_symbol.as_deref().map(|name| EntityMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some(name.to_string()),
            scope: None,
            node_type: None,
            signature: None,
            doc: None,
            modifiers: Vec::new(),
        });
        let entity_id = match caller_symbol.as_deref() {
            Some(name) => format!("{caller_file}::{name}"),
            None => caller_file.clone(),
        };
        let file = FileInfo {
            id: file_id.clone(),
            absolute_path: caller_file.clone(),
            relative_path,
            root_path,
            size_bytes: 0,
            last_modified_time: 0,
            content_hash: None,
            kind: None,
            format,
            index_status: None,
        };
        let entity = Entity {
            id: entity_id,
            file_id: file_id.clone(),
            range: range.clone(),
            content: FragmentContent::Text {
                text: String::new(),
            },
            metadata: metadata.clone(),
        };
        hits.push(SearchHit {
            entity,
            file,
            evidence: vec![SearchHitEvidence {
                range,
                content: FragmentContent::Text {
                    text: String::new(),
                },
                metadata,
                is_entity: true,
                path: RecallPath::Fts,
                route_id: None,
                query: None,
                rank: Some(index),
                score: None,
                forced: None,
            }],
            rank: index + 1,
            score: guidance_core::search_types::RrfScore::new(1.0 / (index + 1) as f64),
            matched_by: SearchMatchedBy::Fts,
            trace: None,
        });
    }
    Ok(hits)
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
    // Think-wrapped or prose-wrapped bodies never survive whole-body
    // parsing; extract the first balanced value first (fail-closed
    // shape unchanged: unparseable still drops everything).
    let stripped = guidance_core::model_text::strip_think_tags(&text);
    let scores: std::collections::BTreeMap<String, serde_json::Value> =
        guidance_core::model_text::extract_json_value(stripped.trim())
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
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

/// Minimum decision score (0-100) for a capability brief to tier
/// `strong`. Same floor shape as the docs judge.
pub const EXPLAIN_CAPABILITY_MIN_SCORE: u32 = 60;

/// Capability judge prompt version fenced into shadow records.
pub const CAPABILITY_JUDGE_VERSION: u64 = 1;

/// One judged brief: the ranked brief plus its optional 0-100 score.
/// `None` keeps deterministic SVO order — the fail-open contrast with
/// the docs judge, which drops everything unparseable.
#[derive(Debug, Clone)]
/// One judged brief: the ranked brief plus its optional 0-100 score.
/// `None` keeps deterministic SVO order — the fail-open contrast with
/// the docs judge, which drops everything unparseable.
pub struct ScoredBrief {
    /// The SVO-ranked brief.
    pub brief: guidance_core::query::capability_match::RankedBrief,
    /// Judge score 0-100 (`None` = unjudged, renders `considered`).
    pub score: Option<u8>,
}

/// Brief tier: `strong` briefs feed synthesis context, `considered`
/// briefs render without feeding it. Unjudged briefs render as
/// `considered` — never filtered, until a calibrated gate earns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Brief tier: `strong` briefs feed synthesis context, `considered`
/// briefs render without feeding it. Unjudged briefs render as
/// `considered` — never filtered, until a calibrated gate earns it.
pub enum CapabilityTier {
    /// At or above the floor: feeds synthesis context.
    Strong,
    /// Below the floor or unjudged: renders without feeding synthesis.
    Considered,
}

/// Tier one score: the floor decides; unjudged reads `considered`.
#[must_use]
/// Tier one score: the floor decides; unjudged reads `considered`.
pub fn capability_tier(score: Option<u8>) -> CapabilityTier {
    match score {
        Some(score) if u32::from(score) >= EXPLAIN_CAPABILITY_MIN_SCORE => CapabilityTier::Strong,
        _ => CapabilityTier::Considered,
    }
}

/// Tier label for rendering (`strong` / `considered`).
#[must_use]
/// Tier label for rendering (`strong` / `considered`).
pub fn capability_tier_label(tier: CapabilityTier) -> &'static str {
    match tier {
        CapabilityTier::Strong => "strong",
        CapabilityTier::Considered => "considered",
    }
}

/// Pure capability-judge prompt constructor: byte-stable prompts for
/// one query, its SVO understanding, and labelled candidates. The
/// judge calls this, so the wire format cannot drift from the
/// goldens; the agent loop reuses it for its own judging.
#[must_use]
/// Pure capability-judge prompt constructor: byte-stable prompts for
/// one query, its SVO understanding, and labelled candidates. The
/// judge calls this, so the wire format cannot drift from the
/// goldens; the agent loop reuses it for its own judging.
pub fn build_capability_judge_prompt(
    query: &str,
    understanding_svo: &[String],
    candidates: &[(String, String)],
) -> Vec<fluent_llm::protocol::ChatMessage> {
    let mut body = format!("Question: {query}\n\nBriefs:\n");
    for (index, (label, purpose)) in candidates.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(body, "{}. {label}\n{purpose}\n\n", index + 1);
    }
    let signals = if understanding_svo.is_empty() {
        "(none)".to_string()
    } else {
        understanding_svo.join(", ")
    };
    body.push_str(&format!("Query signals: {signals}\n"));
    vec![
        fluent_llm::protocol::ChatMessage::with_role(
            fluent_llm::roles::ChatRole::System,
            "Rate how relevant each brief is to the question, 0-100. \
             Respond with ONLY a strict JSON object mapping 1-based brief \
             number to integer score, e.g. {\"1\": 85}.",
        ),
        fluent_llm::protocol::ChatMessage::with_role(fluent_llm::roles::ChatRole::User, body),
    ]
}

/// Machine-readable capability brief: the agent-handoff shape
/// (crate/domain/SVO/tier), pinned by schema goldens. Pure.
#[must_use]
/// Machine-readable capability brief: the agent-handoff shape
/// (crate/domain/SVO/tier), pinned by schema goldens. Pure.
pub fn scored_brief_json(scored: &ScoredBrief) -> serde_json::Value {
    serde_json::json!({
        "anchor": scored.brief.anchor,
        "crate": scored.brief.crate_name,
        "domain": scored.brief.domain,
        "id": scored.brief.id,
        "predicate": scored.brief.predicate,
        "purpose": scored.brief.purpose,
        "rank": scored.brief.rank,
        "roles": scored.brief.roles,
        "score": scored.score,
        "source_path": scored.brief.source_path,
        "tier": capability_tier_label(capability_tier(scored.score)),
    })
}

/// Decision-judged capability briefs:/// Decision-judged capability briefs: the local decision model scores
/// each brief 0-100 and scores tier the render. Ranks never move —
/// the judge is uncalibrated, so it tiers but neither filters nor
/// reorders. Unparseable, unknown-id, or failed judgments keep every
/// brief unjudged in deterministic order (fail open by design — the
/// section is a foothold, and dropping it on judge failure repeats
/// the docs bug).
pub fn judge_capability_briefs(
    query: &str,
    briefs: &[guidance_core::query::capability_match::RankedBrief],
    backend: &dyn fluent_llm::client::ChatBackend,
) -> Vec<ScoredBrief> {
    if briefs.is_empty() {
        return Vec::new();
    }
    let understanding = significant_query_tokens(query);
    let candidates: Vec<(String, String)> = briefs
        .iter()
        .map(|brief| {
            (
                format!(
                    "{}/{} ({}#{})",
                    brief.crate_name, brief.domain, brief.source_path, brief.anchor
                ),
                brief.purpose.clone(),
            )
        })
        .collect();
    let messages = build_capability_judge_prompt(query, &understanding, &candidates);
    let extras = serde_json::json!({
        "temperature": 0.0,
        "max_tokens": 64,
        "prompt_version": CAPABILITY_JUDGE_VERSION,
    });
    let unjudged = || {
        briefs
            .iter()
            .map(|brief| ScoredBrief {
                brief: brief.clone(),
                score: None,
            })
            .collect()
    };
    let text = match backend.chat_complete_with_extras(&messages, &extras) {
        Ok(text) => text,
        Err(_) => return unjudged(),
    };
    let stripped = guidance_core::model_text::strip_think_tags(&text);
    let scores: std::collections::BTreeMap<String, serde_json::Value> =
        guidance_core::model_text::extract_json_value(stripped.trim())
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
    if scores.keys().any(|key| {
        key.parse::<usize>()
            .map_or(true, |n| n < 1 || n > briefs.len())
    }) {
        return unjudged();
    }
    briefs
        .iter()
        .enumerate()
        .map(|(index, brief)| {
            let score = scores
                .get(&(index + 1).to_string())
                .and_then(serde_json::Value::as_u64)
                .and_then(|score| u8::try_from(score).ok());
            ScoredBrief {
                brief: brief.clone(),
                score,
            }
        })
        .collect()
}

/// Whether one explain row's source names a stale file: row sources
/// are relative against their ingest root (a src dir), while the stale
/// set is absolute. Candidates resolve against the workspace plus every
/// src dir, so `src/` layouts match exactly like root layouts do. Pure.
#[must_use]
/// Whether one explain row's source names a stale file: row sources
/// are relative against their ingest root (a src dir), while the stale
/// set is absolute. Candidates resolve against the workspace plus every
/// src dir, so `src/` layouts match exactly like root layouts do. Pure.
pub fn explain_row_is_stale(
    row_source: &str,
    workspace: &str,
    src_dirs: &[PathBuf],
    stale: &[String],
) -> bool {
    let mut candidates = Vec::with_capacity(src_dirs.len() + 1);
    candidates.push(
        PathBuf::from(workspace)
            .join(row_source)
            .to_string_lossy()
            .into_owned(),
    );
    candidates.extend(
        src_dirs
            .iter()
            .map(|root| root.join(row_source).to_string_lossy().into_owned()),
    );
    candidates
        .iter()
        .any(|candidate| stale.iter().any(|path| path == candidate))
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
            // Absolute path: relative_path is src-root-relative, which
            // workspace.join would misresolve in src/ layouts (stale
            // fallback under a live header). Absolute join is a no-op.
            if let Some((snippet, start, end)) = guidance_core::ast_parser::extract_code_snippet(
                &hit.file.absolute_path,
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

/// Source files behind doc ranking: the configured tree walk plus,
/// when enabled, the repo-root orientation files. One home for the
/// ranked set — rank and fingerprint both consume it, so a file the
/// ranker sees is always fingerprinted (no stale-serving) and a file
/// it skips never invalidates (no wasted recompute). Root files
/// bypass the tree count/byte caps (exactly two bounded files) but
/// keep the per-file size cap; an already-listed root (docs tree
/// rooted at the workspace) dedupes by canonical path.
fn doc_source_files(
    docs_dir: Option<&std::path::Path>,
    workspace: &std::path::Path,
    include_roots: bool,
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut total_bytes: u64 = 0;
    if let Some(dir) = docs_dir {
        collect_markdown(dir, &mut files, &mut total_bytes);
    }
    if include_roots {
        for name in ["README.md", "AGENTS.md"] {
            let candidate = workspace.join(name);
            let listed = files.iter().any(|listed| {
                std::fs::canonicalize(listed)
                    .ok()
                    .zip(std::fs::canonicalize(&candidate).ok())
                    .is_some_and(|(a, b)| a == b)
            });
            if listed || !candidate.is_file() {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&candidate) {
                if meta.len() > 100_000 {
                    continue;
                }
            }
            files.push(candidate);
        }
    }
    files
}

/// Ranked doc paragraphs for one query over a markdown tree: walk once
/// (capped), split blank-line paragraphs, score by shared query-token
/// overlap. Returns `(doc-rel-path, excerpt)` in rank order. Pure file
/// I/O, no index writes — the docs stay outside the fragment index.
#[must_use]
/// Ranked doc paragraphs for one query over a markdown tree, plus the
/// repo-root orientation files when enabled.
pub fn rank_doc_paragraphs(
    query: &str,
    docs_dir: Option<&std::path::Path>,
    workspace: &std::path::Path,
    limit: usize,
    include_roots: bool,
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
    let files = doc_source_files(docs_dir, workspace, include_roots);
    score_ranked_files(&files, &query_tokens, workspace, limit)
}

/// Score one file list (shared by rank entry points): split
/// blank-line paragraphs, keep overlaps, rank by (overlap desc,
/// path asc), take the limit. Pure.
fn score_ranked_files(
    files: &[PathBuf],
    query_tokens: &[String],
    workspace: &std::path::Path,
    limit: usize,
) -> Vec<(String, String)> {
    use search_vector::tokens::split_tokens;
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

/// Max cached query rankings in the on-disk docs rank cache.
pub const DOC_RANK_CACHE_MAX_ENTRIES: usize = 128;

/// Cache file name inside the guidance dir. Never ingested (JSON
/// under `.guidance/`, outside every selection root) and never
/// treated as a sidecar (not under `src/`).
pub const DOC_RANK_CACHE_FILE: &str = "doc_rank_cache.json";

/// Fingerprint of the ranked file set for cache validation:
/// sorted `(workspace-relative path, mtime ms, size bytes)`.
/// Computed over exactly the files `doc_source_files` returns, so a
/// file the ranker sees is always fingerprinted (no stale serving)
/// and a file it skips never invalidates (no wasted recompute).
/// Cheap stat walk — no reads.
#[must_use]
/// Fingerprint of the ranked file set for cache validation:
/// sorted `(workspace-relative path, mtime ms, size bytes)`.
/// Computed over exactly the files `doc_source_files` returns, so a
/// file the ranker sees is always fingerprinted (no stale serving)
/// and a file it skips never invalidates (no wasted recompute).
/// Cheap stat walk — no reads.
pub fn doc_tree_fingerprint(
    files: &[PathBuf],
    workspace: &std::path::Path,
) -> Vec<(String, i64, u64)> {
    let mut out: Vec<(String, i64, u64)> = files
        .iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(path).ok()?;
            let rel = path
                .strip_prefix(workspace)
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_millis() as i64);
            Some((rel, mtime, meta.len()))
        })
        .collect();
    out.sort();
    out
}

/// Order-insensitive cache key: sorted significant query tokens
/// plus the rank limit and the root-files flag. Rank scoring is
/// token-set based with path-ordered tiebreaks, so token order never
/// changes the output; the flag separates scoped from unscoped runs
/// over the same query.
#[must_use]
/// Order-insensitive cache key: sorted significant query tokens
/// plus the rank limit and the root-files flag. Rank scoring is
/// token-set based with path-ordered tiebreaks, so token order never
/// changes the output; the flag separates scoped from unscoped runs
/// over the same query.
pub fn doc_rank_cache_key(query: &str, limit: usize, include_roots: bool) -> String {
    use search_vector::tokens::split_tokens;
    let mut tokens: Vec<String> = {
        let mut seen = Vec::new();
        for token in split_tokens(query) {
            search_vector::tokens::lower_dedup_push(&mut seen, &token);
        }
        seen
    };
    tokens.sort();
    tokens.push(format!("limit={limit}"));
    tokens.push(format!("roots={include_roots}"));
    tokens.join("\x1f")
}

/// One cached ranking: the query key plus the exact ranked list
/// `rank_doc_paragraphs` returned (post-take, byte-identical).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// One cached ranking: the query key plus the exact ranked list
/// `rank_doc_paragraphs` returned (post-take, byte-identical).
struct DocRankCacheEntry {
    query: String,
    ranked: Vec<(String, String)>,
}

/// On-disk memo of doc rankings: the tree fingerprint the entries
/// were computed from, plus bounded query→ranking rows. All methods
/// are total and hermetic: missing/corrupt input is an empty cache,
/// never an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// On-disk memo of doc rankings: the tree fingerprint the entries
/// were computed from, plus bounded query→ranking rows. All methods
/// are total and hermetic: missing/corrupt input is an empty cache,
/// never an error.
pub struct DocRankCache {
    files: Vec<(String, i64, u64)>,
    entries: Vec<DocRankCacheEntry>,
}

impl DocRankCache {
    /// Load from disk; missing or unparsable files read as empty
    /// (fail open to a fresh rank — a corrupt cache must never
    /// break explain or serve stale paragraphs as current).
    #[must_use]
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Persist atomically. Callers save only after a miss changed
    /// the cache, so read commands stay write-free on hits.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        common_core::io::write_atomic(path, text.as_bytes())
    }

    /// Number of cached query rankings (bounded by
    /// [`DOC_RANK_CACHE_MAX_ENTRIES`]). Test introspection only;
    /// production code keys off lookup hits, not sizes.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no ranking is cached. Test introspection only.
    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Hit only when the tree fingerprint matches exactly AND the
    /// query key is present. Anything else is a miss (re-rank).
    #[must_use]
    pub fn lookup(
        &self,
        fingerprint: &[(String, i64, u64)],
        key: &str,
    ) -> Option<Vec<(String, String)>> {
        if self.files != fingerprint {
            return None;
        }
        self.entries
            .iter()
            .find(|entry| entry.query == key)
            .map(|entry| entry.ranked.clone())
    }

    /// Record a ranking, keyed by query. Returns true when the
    /// cache content changed (caller saves only then). Oldest
    /// entries evict past the cap; re-storing an identical ranking
    /// refreshes recency without reporting a change.
    pub fn store(
        &mut self,
        fingerprint: Vec<(String, i64, u64)>,
        key: String,
        ranked: Vec<(String, String)>,
    ) -> bool {
        let mut changed = self.files != fingerprint;
        self.files = fingerprint;
        if let Some(index) = self.entries.iter().position(|entry| entry.query == key) {
            let entry = self.entries.remove(index);
            if entry.ranked != ranked {
                changed = true;
            }
            self.entries.push(DocRankCacheEntry { query: key, ranked });
        } else {
            changed = true;
            self.entries.push(DocRankCacheEntry { query: key, ranked });
        }
        while self.entries.len() > DOC_RANK_CACHE_MAX_ENTRIES {
            self.entries.remove(0);
            changed = true;
        }
        changed
    }
}

/// Ranked doc paragraphs with a persistent memo: fingerprint the tree
/// (stat walk, no reads); on fingerprint + query-key hit return the
/// stored list byte-identical, else rank fresh and record it.
/// Returns the ranking plus whether the cache changed (callers save
/// only then, keeping read commands write-free on hits).
#[must_use]
/// Ranked doc paragraphs with a persistent memo: fingerprint the tree
/// (stat walk, no reads); on fingerprint + query-key hit return the
/// stored list byte-identical, else rank fresh and record it.
/// Returns the ranking plus whether the cache changed (callers save
/// only then, keeping read commands write-free on hits).
pub fn rank_doc_paragraphs_cached(
    query: &str,
    docs_dir: Option<&std::path::Path>,
    workspace: &std::path::Path,
    limit: usize,
    include_roots: bool,
    cache: &mut DocRankCache,
) -> (Vec<(String, String)>, bool) {
    let key = doc_rank_cache_key(query, limit, include_roots);
    let files = doc_source_files(docs_dir, workspace, include_roots);
    let fingerprint = doc_tree_fingerprint(&files, workspace);
    if let Some(hit) = cache.lookup(&fingerprint, &key) {
        return (hit, false);
    }
    let ranked = rank_doc_paragraphs(query, docs_dir, workspace, limit, include_roots);
    let changed = cache.store(fingerprint, key, ranked.clone());
    (ranked, changed)
}

/// Answer prompt: the question plus fenced excerpts as the only
/// evidence. The model must answer concisely from the excerpts and
/// cite every claim as `file:line`.
#[must_use]
/// Answer prompt: the question plus fenced excerpts as the only evidence.
pub fn answer_messages(
    query: &str,
    context: &[(String, String, u32, u32, String)],
    briefs: &[String],
) -> Vec<fluent_llm::protocol::ChatMessage> {
    use fluent_llm::protocol::ChatMessage;
    use fluent_llm::roles::ChatRole;
    let system = ChatMessage::with_role(
        ChatRole::System,
        "You answer questions about a codebase using ONLY the provided excerpts and briefs. \
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
    if !briefs.is_empty() {
        use std::fmt::Write as _;
        body.push_str("\nCapability briefs:\n");
        for purpose in briefs {
            let _ = write!(body, "- {purpose}\n");
        }
    }
    vec![system, ChatMessage::with_role(ChatRole::User, body)]
}

/// Max answer characters for post-processing: the repo's token
/// estimate convention (tokens * 4), mirroring the summary budget.
pub const EXPLAIN_ANSWER_MAX_CHARS: usize = EXPLAIN_ANSWER_MAX_TOKENS * 4;

/// Clean one model answer: strip think/reasoning wrappers, then cut an
/// over-long answer at the last in-cap sentence boundary (never
/// mid-word). Falls back to the hard char-boundary cut only when no
/// boundary exists; short answers pass through byte-identical.
#[must_use]
/// Clean one model answer: strip think/reasoning wrappers, then cut an
/// over-long answer at the last in-cap sentence boundary (never
/// mid-word). Falls back to the hard char-boundary cut only when no
/// boundary exists; short answers pass through byte-identical.
fn clean_answer_text(text: &str) -> String {
    let stripped = guidance_core::model_text::strip_think_tags(text);
    let trimmed = stripped.trim();
    if trimmed.len() <= EXPLAIN_ANSWER_MAX_CHARS {
        return trimmed.to_string();
    }
    let mut end = EXPLAIN_ANSWER_MAX_CHARS.min(trimmed.len());
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = &trimmed[..end];
    let mut boundary: Option<usize> = None;
    for (index, cell) in prefix.char_indices() {
        if matches!(cell, '.' | '!' | '?') {
            let next = prefix[index + cell.len_utf8()..].chars().next();
            if next.is_none_or(char::is_whitespace) {
                boundary = Some(index + cell.len_utf8());
            }
        }
    }
    match boundary {
        Some(cut) => prefix[..cut].trim_end().to_string(),
        None => prefix.to_string(),
    }
}

/// Concise model answer over the excerpt context. `None` when there is
/// no context or the model says nothing: the caller renders the
/// unavailable line instead of an invented answer.
pub fn answer_with_backend(
    query: &str,
    context: &[(String, String, u32, u32, String)],
    briefs: &[String],
    backend: &dyn fluent_llm::client::ChatBackend,
    model: &str,
) -> Result<Option<String>, String> {
    if context.is_empty() {
        return Ok(None);
    }
    let messages = answer_messages(query, context, briefs);
    let extras = serde_json::json!({
        "temperature": 0.0,
        "max_tokens": EXPLAIN_ANSWER_MAX_TOKENS,
    });
    let text = backend
        .chat_complete_with_extras(&messages, &extras)
        .map_err(|error| error.to_string())?;
    let _ = model;
    let cleaned = clean_answer_text(&text);
    if cleaned.is_empty() {
        return Ok(None);
    }
    Ok(Some(cleaned))
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

/// Significant query tokens: lowercase word tokens of length 3 or more,
/// minus scaffolding stopwords. The relevance gate's vocabulary:
/// distinctive names survive on one token, prose needs two.
/// Delegates to the shared funnel rule so the two can never drift.
#[must_use]
/// Significant query tokens: lowercase word tokens of length 3 or more,
/// minus scaffolding stopwords. The relevance gate's vocabulary:
/// distinctive names survive on one token, prose needs two.
/// Delegates to the shared funnel rule so the two can never drift.
pub fn significant_query_tokens(query: &str) -> Vec<String> {
    guidance_core::query::funnel::significant_terms(query)
}

/// Lowercase haystack behind one hit: symbol name, file stem, and every
/// evidence/entity text. Structural identity (name, stem) and prose
/// (texts) share one string; the caller decides the bar.
fn hit_search_text(hit: &SearchHit) -> String {
    fn push_content(out: &mut String, content: &guidance_core::search_types::FragmentContent) {
        use guidance_core::search_types::FragmentContent;
        if let FragmentContent::Text { text } = content {
            out.push(' ');
            out.push_str(&text.to_lowercase());
        }
    }
    let mut out = String::new();
    if let Some((name, _)) = hit_symbol(hit) {
        out.push_str(&name.to_lowercase());
    }
    out.push(' ');
    if let Some(stem) = std::path::Path::new(&hit.file.absolute_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
    {
        out.push_str(&stem.to_lowercase());
    }
    for evidence in &hit.evidence {
        push_content(&mut out, &evidence.content);
    }
    push_content(&mut out, &hit.entity.content);
    out
}

/// Whether one fused hit is relevant to the query's significant tokens:
/// any token in the symbol name (structural match), or two distinct
/// tokens anywhere in name, file stem, or evidence text (prose match).
/// File stems alone never qualify: any `*components*.rs` file would
/// otherwise survive its own name token on a broad query and feed the
/// answer confabulation. Single-token queries keep everything — a lone
/// token cannot prove irrelevance, so narrow intent keeps today's
/// behavior byte-identically.
#[must_use]
/// Whether one fused hit is relevant to the query's significant tokens:
/// any token in the symbol name (structural match), or two distinct
/// tokens anywhere in name, file stem, or evidence text (prose match).
/// File stems alone never qualify: any `*components*.rs` file would
/// otherwise survive its own name token on a broad query and feed the
/// answer confabulation. Single-token queries keep everything — a lone
/// token cannot prove irrelevance, so narrow intent keeps today's
/// behavior byte-identically.
pub fn hit_is_relevant(hit: &SearchHit, tokens: &[String]) -> bool {
    if tokens.len() <= 1 {
        return true;
    }
    let structural = hit_symbol(hit).is_some_and(|(name, _)| {
        let name = name.to_lowercase();
        tokens.iter().any(|token| name.contains(token.as_str()))
    });
    if structural {
        return true;
    }
    let haystack = hit_search_text(hit);
    let distinct = tokens
        .iter()
        .filter(|token| haystack.contains(token.as_str()))
        .count();
    distinct >= 2
}

/// Keep the relevant subset of fused hits, rank order. Pure; the caller
/// decides what gated-empty means.
#[must_use]
/// Keep the relevant subset of fused hits, rank order. Pure; the caller
/// decides what gated-empty means.
pub fn filter_relevant_hits(hits: &[SearchHit], tokens: &[String]) -> Vec<SearchHit> {
    hits.iter()
        .filter(|hit| hit_is_relevant(hit, tokens))
        .cloned()
        .collect()
}

/// Canonical identity for one hit's file: absolute paths verbatim,
/// workspace-relative rows joined to the workspace, `/./` and interior
/// `..` collapsed lexically. A relative workspace (`.` or `src`) resolves
/// against the process working directory first, so dual rows (one
/// indexed relative, one absolute) collapse to one identity instead of
/// rendering twice.
fn canonical_hit_path(hit: &SearchHit, workspace: &std::path::Path) -> String {
    use std::path::Component;
    let abs = PathBuf::from(&hit.file.absolute_path);
    let joined = if abs.is_absolute() {
        abs
    } else {
        workspace.join(abs)
    };
    // A relative workspace (`.` or `src`) must not leave a relative
    // identity: resolve against the working directory first, best
    // effort, so dual rows collapse instead of rendering twice.
    let joined = if joined.is_absolute() {
        joined
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&joined))
            .unwrap_or(joined)
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

/// Code-thin bar: fewer relevant code hits than this opens the
/// capability branch (branch 1).
pub const CAPABILITY_TRIGGER_HITS: usize = 3;

/// Multi-route agreement floor: a top fused score below this means no
/// code hit ranked in more than one route (single-route RRF peaks at
/// ~1/61), which also opens the capability branch.
pub const CAPABILITY_TOP_SCORE_FLOOR: f64 = 0.02;

/// Brief candidate pool per axis (FTS, lemma) before SVO ranking.
pub const CAPABILITY_CANDIDATE_LIMIT: usize = 20;

/// Scoped code hits pulled per traversed brief (branch 2).
pub const CAPABILITY_BRANCH2_LIMIT: usize = 5;

/// One branch-2 scoped pull: the traversed brief's code hits, tagged
/// with the brief's crate for provenance.
#[derive(Debug, Clone)]
/// One branch-2 scoped pull: the traversed brief's code hits, tagged
/// with the brief's crate for provenance.
pub struct BranchPull {
    /// Traversed brief's crate (the scope the pull ran under).
    pub crate_name: String,
    /// Scoped code hits passing the relaxed gate.
    pub hits: Vec<SearchHit>,
}

/// Branch outcome: the ranked briefs plus the branch-2 pulls (at most
/// one — the rank-1 pre-calibration pull). An untriggered query reads
/// as empty briefs and pulls.
#[derive(Debug, Clone, Default)]
/// Branch outcome: the ranked briefs plus the branch-2 pulls (at most
/// one — the rank-1 pre-calibration pull). An untriggered query reads
/// as empty briefs and pulls.
pub struct CapabilityBranch {
    /// Ranked briefs, capped by the shared ranker.
    pub briefs: Vec<guidance_core::query::capability_match::RankedBrief>,
    /// Scoped code pulls (rank-1 brief only, pre-calibration).
    pub pulls: Vec<BranchPull>,
}

/// Whether code-thin recall opens the capability branch: fewer
/// relevant hits than the bar, or a top fused score with no
/// multi-route agreement. Pure; the caller decides what fires.
#[must_use]
/// Whether code-thin recall opens the capability branch: fewer
/// relevant hits than the bar, or a top fused score with no
/// multi-route agreement. Pure; the caller decides what fires.
pub fn capability_trigger(hits: &[SearchHit]) -> bool {
    if hits.len() < CAPABILITY_TRIGGER_HITS {
        return true;
    }
    hits.first()
        .is_some_and(|hit| hit.score.value() < CAPABILITY_TOP_SCORE_FLOOR)
}

/// Whether a ranked brief opens branch-2 traversal: the rank-1 brief
/// only (pre-calibration exception — unjudged briefs count as weak,
/// and the single best guess keeps the tree functional before any
/// judge is calibrated).
#[must_use]
/// Whether a ranked brief opens branch-2 traversal: the rank-1 brief
/// only (pre-calibration exception — unjudged briefs count as weak,
/// and the single best guess keeps the tree functional before any
/// judge is calibrated).
pub fn brief_opens_traversal(rank: usize) -> bool {
    rank == 1
}

/// Branch-2 scope for a brief's crate: the package's registered prefix
/// as a glob. `None` (unknown or unmapped crate) never traverses —
/// an unscoped pull is just keyword recall again.
#[must_use]
/// Branch-2 scope for a brief's crate: the package's registered prefix
/// as a glob. `None` (unknown or unmapped crate) never traverses —
/// an unscoped pull is just keyword recall again.
pub fn branch_scope_glob(crate_name: &str) -> Option<String> {
    if crate_name.is_empty() || crate_name == "unknown" {
        return None;
    }
    guidance_core::component_map::prefix_for_package(crate_name)
        .map(|prefix| format!("{prefix}/**"))
}

/// Whether a branch-2 hit passes the relaxed gate: the originating
/// brief already established *where to look*, so the text bar drops
/// from two significant tokens to one (name, stem, or text).
/// Single-token queries keep everything, like the standard gate.
#[must_use]
/// Whether a branch-2 hit passes the relaxed gate: the originating
/// brief already established *where to look*, so the text bar drops
/// from two significant tokens to one (name, stem, or text).
/// Single-token queries keep everything, like the standard gate.
pub fn branch_hit_passes_gate(hit: &SearchHit, tokens: &[String]) -> bool {
    if tokens.len() <= 1 {
        return true;
    }
    let haystack = hit_search_text(hit);
    tokens.iter().any(|token| haystack.contains(token.as_str()))
}

/// Merge branch-2 pulls into the base hits with a visited-set on
/// canonical file identity: a file rendering in both branches renders
/// once (first wins), annotated with both provenances. Returns the
/// merged hits, per-hit branch tags (aligned, `None` for base hits),
/// and dual-provenance tags for base-covered files.
#[must_use]
/// Merge branch-2 pulls into the base hits with a visited-set on
/// canonical file identity: a file rendering in both branches renders
/// once (first wins), annotated with both provenances. Returns the
/// merged hits, per-hit branch tags (aligned, `None` for base hits),
/// and dual-provenance tags for base-covered files.
pub fn merge_branch_hits(
    base: Vec<SearchHit>,
    pulls: &[BranchPull],
    workspace: &std::path::Path,
) -> (Vec<SearchHit>, Vec<Option<String>>, Vec<(String, String)>) {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = base
        .iter()
        .map(|hit| canonical_hit_path(hit, workspace))
        .collect();
    let mut merged = base;
    let mut tags: Vec<Option<String>> = vec![None; merged.len()];
    let mut duals: Vec<(String, String)> = Vec::new();
    for pull in pulls {
        for hit in &pull.hits {
            if seen.insert(canonical_hit_path(hit, workspace)) {
                tags.push(Some(pull.crate_name.clone()));
                merged.push(hit.clone());
            } else {
                duals.push((
                    hit.file.relative_path.clone(),
                    format!("via capability: {}", pull.crate_name),
                ));
            }
        }
    }
    (merged, tags, duals)
}

/// Per-snippet provenance tags from file-level branch tags: every
/// snippet whose file a branch-2 pull covered carries the pull's tag.
/// Multiple crates join with `, `; uncovered snippets read `""`.
#[must_use]
/// Per-snippet provenance tags from file-level branch tags: every
/// snippet whose file a branch-2 pull covered carries the pull's tag.
/// Multiple crates join with `, `; uncovered snippets read `""`.
pub fn tag_snippets_by_file(
    snippets: &[(String, String, u32, u32, String)],
    file_tags: &[(String, String)],
) -> Vec<String> {
    snippets
        .iter()
        .map(|(_, file, _, _, _)| {
            let mut crates: Vec<&str> = file_tags
                .iter()
                .filter(|(tagged, _)| tagged == file)
                .map(|(_, tag)| tag.as_str())
                .collect();
            crates.sort();
            crates.dedup();
            crates.join(", ")
        })
        .collect()
}

/// Branch-1 recall over stored briefs: SVO overlap over the funneled
/// query signals plus FTS/lemma over brief text, ranked never
/// filtered. Empty on any failure (old index without the table reads
/// as no briefs, never an error).
#[must_use]
/// Branch-1 recall over stored briefs: SVO overlap over the funneled
/// query signals plus FTS/lemma over brief text, ranked never
/// filtered. Empty on any failure (old index without the table reads
/// as no briefs, never an error).
pub fn recall_capability_briefs(
    query: &str,
    db: &GuidanceDb,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Vec<guidance_core::query::capability_match::RankedBrief> {
    use guidance_core::query::capability_match::{
        funneled_query_signals, rank_briefs, BriefCandidate,
    };
    use guidance_core::query::funnel::funnel_terms;
    let funnel = funnel_terms(query, &search_vector::SemanticAliases::new(), None);
    if funnel.is_empty() {
        return Vec::new();
    }
    let signals = funneled_query_signals(&funnel, nlp);
    let expanded = funnel.expanded.join(" ");
    let mut fts_scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    if let Ok(hits) = db.search_capabilities(&expanded, CAPABILITY_CANDIDATE_LIMIT) {
        for hit in hits {
            fts_scores.insert(hit.id, hit.score);
        }
    }
    let mut ids: std::collections::HashSet<String> = fts_scores.keys().cloned().collect();
    let lemmas = guidance_core::query::ingest::query_lemmas(nlp, &expanded);
    if !lemmas.is_empty() {
        if let Ok(lemma_ids) = db.capabilities_for_lemmas(&lemmas, CAPABILITY_CANDIDATE_LIMIT) {
            ids.extend(lemma_ids);
        }
    }
    if ids.is_empty() {
        return Vec::new();
    }
    let mut ordered: Vec<String> = ids.into_iter().collect();
    ordered.sort();
    let rows = db.capabilities_for_ids(&ordered).unwrap_or_default();
    let candidates: Vec<BriefCandidate> = rows
        .into_iter()
        .map(|row| {
            let roles: Vec<String> = serde_json::from_str(&row.roles_json).unwrap_or_default();
            BriefCandidate {
                id: row.id.clone(),
                purpose: row.purpose,
                predicate: row.predicate,
                roles,
                crate_name: row.crate_name,
                domain: row.domain,
                source_path: row.source_path,
                anchor: row.anchor,
                fts_score: fts_scores.remove(&row.id),
            }
        })
        .collect();
    rank_briefs(&signals, &candidates)
}

/// The capability branch beside fused recall: code-thin relevant hits
/// open SVO-ranked briefs (branch 1); the rank-1 brief opens one
/// scoped code pull through the relaxed gate (branch 2, depth ≤ 2 by
/// construction — pulls never recurse). Untriggered queries cost
/// nothing: no DB open, no pipeline build.
#[must_use]
/// The capability branch beside fused recall: code-thin relevant hits
/// open SVO-ranked briefs (branch 1); the rank-1 brief opens one
/// scoped code pull through the relaxed gate (branch 2, depth ≤ 2 by
/// construction — pulls never recurse). Untriggered queries cost
/// nothing: no DB open, no pipeline build.
pub fn capability_branch(
    query: &str,
    workspace: &str,
    db_path: &str,
    hits: &[SearchHit],
) -> CapabilityBranch {
    if !capability_trigger(hits) {
        return CapabilityBranch::default();
    }
    let db_file = PathBuf::from(db_path);
    if !db_file.exists() {
        return CapabilityBranch::default();
    }
    let db = match GuidanceDb::open(&db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!("Warning: capability branch cannot open index at {db_path}: {error}");
            return CapabilityBranch::default();
        }
    };
    let nlp_holder = LazyNlp::new();
    let nlp = nlp_holder.get();
    let briefs = recall_capability_briefs(query, &db, nlp);
    let tokens = significant_query_tokens(query);
    let mut pulls = Vec::new();
    for brief in briefs
        .iter()
        .filter(|brief| brief_opens_traversal(brief.rank))
    {
        let Some(scope) = branch_scope_glob(&brief.crate_name) else {
            continue;
        };
        let plan = build_search_plan(
            &brief.purpose,
            SearchMode::Fuse,
            CAPABILITY_BRANCH2_LIMIT,
            false,
            true,
            &[scope],
            &[],
        );
        let mut branch_hits = match recall_on_index(workspace, db_path, &plan) {
            Ok(branch_hits) => branch_hits,
            Err(error) => {
                eprintln!("Warning: capability branch-2 recall failed ({error})");
                Vec::new()
            }
        };
        branch_hits.retain(|hit| branch_hit_passes_gate(hit, &tokens));
        pulls.push(BranchPull {
            crate_name: brief.crate_name.clone(),
            hits: branch_hits,
        });
    }
    CapabilityBranch { briefs, pulls }
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
    capabilities: &[(String, String)],
    snippet_tags: &[String],
) -> String {
    let mut out = format!("## Explain: {query}\n\n");
    if rows.is_empty() && capabilities.is_empty() {
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
    if !capabilities.is_empty() {
        out.push_str("\n### From the capabilities\n");
        for (label, purpose) in capabilities {
            out.push_str(&format!("- {label}: {purpose}\n"));
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
            let tag = snippet_tags.get(index).map(String::as_str).unwrap_or("");
            let tagged = if tag.is_empty() {
                String::new()
            } else {
                format!(" ({tag})")
            };
            out.push_str(&format!(
                "#### {}. {name} — {file}:{start}-{end}{tagged}\n",
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
