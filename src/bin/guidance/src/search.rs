//! `search` command (P5): thin shell over the P1 recall core.
//!
//! One core (`guidance-core` index + query), thin execution shells — this
//! module owns CLI parsing and rendering only. Dispatch builds a
//! [`SearchPlan`] and runs [`run_recall`] over [`GuidanceDbStorage`]; the
//! `--rg` route runs the standalone L0 [`RgBackend`]. L0 is never fused.

use std::path::PathBuf;

use guidance_core::query::db_storage::GuidanceDbStorage;
use guidance_core::query::hybrid::{plan_from_query, stages_from_hits};
use guidance_core::query::ingest::default_en_pipeline;
use guidance_core::query::recall::run_recall;
use guidance_core::query::rg_backend::{RgBackend, RgOptions};
use guidance_core::query::strategy::FsmEngine;
use guidance_core::query::structure_enrich::{enrich_hits, EnrichedRgHit};
use guidance_core::query_engine::{OutputFormat, QueryEngine};
use guidance_core::zg_types::{CodeSymbolType, SearchPlan, SearchPlanRoute, SearchPlanRouteMode};
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
    let nlp = default_en_pipeline();
    match run_recall(&plan, &storage, None, nlp.as_ref()) {
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

/// Standalone L0 search: managed rg under an `FsCapability` scope, rendered
/// `path:line:col: text` with optional context and symbol enrichment.
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
