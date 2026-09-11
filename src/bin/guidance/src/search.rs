//! `search` command (P5): thin shell over the P1 recall core.
//!
//! One core (`guidance-core` index + query), thin execution shells — this
//! module owns CLI parsing and rendering only. Dispatch builds a
//! [`SearchPlan`] and runs [`run_recall`] over [`GuidanceDbStorage`]; the
//! `--rg` route runs the standalone L0 [`RgBackend`]. L0 is never fused.

use std::path::PathBuf;

use guidance_core::query::db_storage::GuidanceDbStorage;
use guidance_core::query::hybrid::{plan_from_query, stages_from_hits};
use guidance_core::query::ingest::LazyNlp;
use guidance_core::query::recall::run_recall;
use guidance_core::query::rg_backend::{RgBackend, RgOptions};
use guidance_core::query::strategy::FsmEngine;
use guidance_core::query::structure_enrich::{enrich_hits, EnrichedRgHit};
use guidance_core::graph_index::{ContextDirection, ContextEdge, ContextFamily};
use guidance_core::query_engine::{OutputFormat, QueryEngine};
use guidance_core::search_types::{
    CodeSymbolType, SearchHit, SearchPlan, SearchPlanRoute, SearchPlanRouteMode,
};
use search_vector::GuidanceDb;

/// Recall route selection. At most one non-default mode flag may be set
/// (one-probe discipline: combined mode flags are rejected, never merged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
pub fn clamp_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_LIMIT)
}

/// L0 display options: context capture, mtime post-filter, enrichment.
#[derive(Debug, Clone, Default)]
pub struct RgDisplayOptions {
    /// `-B` lines of leading context.
    pub before: usize,
    /// `-A` lines of trailing context.
    pub after: usize,
    /// Modification-time floor (ms epoch).
    pub mtime_after_ms: Option<i64>,
    /// Modification-time ceiling (ms epoch).
    pub mtime_before_ms: Option<i64>,
    /// Attach enclosing symbols (best-effort).
    pub enrich: bool,
}

/// Render standalone L0 hits in the shared `path:line:col` shape, headed by
/// the coverage vocabulary (`rg_exhaustive` when the sweep completed,
/// `rg_truncated` when early-kill cut it at the limit). Enriched hits carry
/// their symbol (`path:line:col [symbol]:`); context lines follow indented.
#[must_use]
pub fn render_rg_hits(hits: &[EnrichedRgHit], limit: usize) -> String {
    let coverage = if hits.len() < limit {
        crate::mcp::Coverage::RgExhaustive
    } else {
        crate::mcp::Coverage::RgTruncated
    };
    let mut out = format!("coverage: {}\n", coverage.as_str());
    for item in hits {
        let hit = &item.hit;
        match &item.symbol {
            Some(symbol) => out.push_str(&format!(
                "{}:{}:{} [{}]: {}\n",
                hit.path,
                hit.line,
                hit.column,
                symbol,
                hit.text.trim_end()
            )),
            None => out.push_str(&format!(
                "{}:{}:{}: {}\n",
                hit.path,
                hit.line,
                hit.column,
                hit.text.trim_end()
            )),
        }
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
                Ok(QueryEngine::format_stages(&stages, format))
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
    let db = match GuidanceDb::open(&db_file) {
        Ok(db) => db,
        Err(error) => return Err(format!("cannot open index at {db_path}: {error}")),
    };
    // L3 embedder: resolved from the workspace project config
    // (`models.embed`, provider table, dims). Construction is offline;
    // absence degrades through the existing embedder-absent paths.
    let cfg =
        guidance_core::config::load_config(std::path::Path::new(workspace)).unwrap_or_default();
    let embedder = crate::embed::embedder_from_config(&cfg);
    let storage = GuidanceDbStorage::new(&db);
    // Rule-lemmatizer pipeline (no model): the L2 route collapses
    // inflections at zero embedding cost; `None` degrades to L1-only.
    // Built lazily on lemma need — recall always needs it here.
    let nlp = LazyNlp::new();
    match run_recall(plan, &storage, embedder.as_deref(), nlp.get()) {
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
    Ok(build_context_lines(&edges, EXPLAIN_CONTEXT_BUDGET, workspace))
}

/// Top-N distinct fused files (hit order) seeding the closure.
/// Pure; the cap is [`CLOSURE_ANCHOR_FILES`](guidance_core::graph_index::CLOSURE_ANCHOR_FILES).
#[must_use]
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

/// Build explain rows: one per fused hit, rank order. The table carries
/// hits only — expansion never perturbs it (additive default); context
/// renders separately below. Pure.
#[must_use]
pub fn build_explain_rows(hits: &[SearchHit]) -> Vec<ExplainRow> {
    hits
        .iter()
        .map(|hit| {
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
            }
        })
        .collect()
}

/// Grouped context provenance: one line per (file, anchor, direction),
/// families merged. Keeps the builder below within type-complexity bounds.
type ContextGroup<'a> = (
    (&'a str, &'a str, ContextDirection),
    Vec<(ContextFamily, &'a str)>,
);

/// Build ranked context lines from depth-1 expansion edges: one line
/// per (file, anchor, direction), families merged (`import "s", call
/// "c"`) so provenance stays complete without doubling lines. Sorted
/// edge order, capped at `cap` lines. Pure.
#[must_use]
pub fn build_context_lines(
    edges: &[ContextEdge],
    cap: usize,
    workspace: &str,
) -> Vec<String> {
    // Group by (file, anchor, direction), merging families in sort order.
    let mut groups: Vec<ContextGroup<'_>> = Vec::new();
    for edge in edges {
        let key = (edge.file.as_str(), edge.anchor.as_str(), edge.direction);
        match groups.iter_mut().find(|(group, _)| *group == key) {
            Some((_, members)) => members.push((edge.family, edge.via.as_str())),
            None => groups.push((key, vec![(edge.family, edge.via.as_str())])),
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
                .map(|(family, via)| {
                    let family = match family {
                        ContextFamily::Import => "import",
                        ContextFamily::Call => "call",
                    };
                    format!("{family} \"{via}\"")
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

/// One rendered explain row: member/symbol title, file path, score.
/// Score is the fused RRF ordinal (positions, not a magnitude — see
/// `RrfScore`) for recall rows, `0.0` for graph-closure rows (graph
/// signal, not a recall score — see M5). Table rendering rounds to two
/// decimals, which is lossy by design: never feed back into ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplainRow {
    /// Symbol/member title (entity id fallback).
    pub name: String,
    /// Workspace-relative file path (absolute fallback).
    pub source: String,
    /// Row score (lossy display value — see above).
    pub score: f64,
}

/// Render explain output: stable header + hits table, then the additive
/// context section (present only when non-empty). The table section is
/// byte-identical with and without expansion — pinned by test. Pure.
#[must_use]
pub fn render_explain_table(
    query: &str,
    rows: &[ExplainRow],
    context: &[String],
) -> String {
    let mut out = format!("## Explain: {query}\n\n");
    if rows.is_empty() {
        out.push_str("No results found.\n");
        return out;
    }
    out.push_str("| Name | Source | Score |\n");
    out.push_str("|------|--------|-------|\n");
    for row in rows {
        out.push_str(&format!("| {} | {} | {:.2} |\n", row.name, row.source, row.score));
    }
    if !context.is_empty() {
        out.push_str("\n### Context\n");
        for line in context {
            out.push_str(line);
            out.push('\n');
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
    let enrich = rg_options.enrich;
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
                if enrich {
                    let (enriched, _) = enrich_hits(&root, &hits);
                    Ok::<_, guidance_core::query::rg_backend::RgError>(enriched)
                } else {
                    Ok(hits
                        .into_iter()
                        .map(|hit| EnrichedRgHit { hit, symbol: None })
                        .collect())
                }
            },
        ))
    });
    match result {
        Ok(enriched) => {
            if enriched.is_empty() {
                Ok("No results found.\n".to_string())
            } else {
                Ok(render_rg_hits(&enriched, limit))
            }
        }
        Err(error) => Err(format!("rg search failed: {error}")),
    }
}

#[cfg(test)]
#[path = "../tests/search_cmd.rs"]
mod tests;
