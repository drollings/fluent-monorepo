//! `search` command (P5): thin shell over the P1 recall core.
//!
//! One core (`guidance-core` index + query), thin execution shells — this
//! module owns CLI parsing and rendering only. Dispatch builds a
//! [`SearchPlan`] and runs [`run_recall`] over [`GuidanceDbStorage`]; the
//! `--rg` route runs the standalone L0 [`RgBackend`]. L0 is never fused.

use std::path::{Path, PathBuf};

use guidance_core::graph_index::{ContextDirection, ContextEdge, ContextFamily, ResolutionMethod};
use guidance_core::query::db_storage::GuidanceDbStorage;
use guidance_core::query::hybrid::{plan_from_query, stages_from_hits};
use guidance_core::query::ingest::LazyNlp;
use guidance_core::query::recall::{run_recall, run_recall_with_policy};
use guidance_core::query::rg_backend::{RgBackend, RgHit, RgOptions};
use guidance_core::query::strategy::FsmEngine;
use guidance_core::query::strategy::QueryIntent;
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

/// Normalize user-supplied selection globs to the indexed root-relative
/// normal form for one workspace: a leading segment naming an indexing
/// root (`src_dirs` entry) is stripped once via
/// `query::glob::normalize_selection_glob`, so workspace-relative
/// `src/<crate>/**` and index-relative `<crate>/**` select the same
/// files. Matching itself (`matches_file_selection`) runs only against
/// `file.relative_path`; this is the single boundary where the
/// workspace-relative spelling is converted, never a second matcher.
#[must_use]
/// Normalize user-supplied selection globs to the indexed root-relative
/// normal form for one workspace: a leading segment naming an indexing
/// root (`src_dirs` entry) is stripped once via
/// `query::glob::normalize_selection_glob`, so workspace-relative
/// `src/<crate>/**` and index-relative `<crate>/**` select the same
/// files. Matching itself (`matches_file_selection`) runs only against
/// `file.relative_path`; this is the single boundary where the
/// workspace-relative spelling is converted, never a second matcher.
pub fn normalize_globs_for_workspace(globs: &[String], workspace: &str) -> Vec<String> {
    let cfg =
        guidance_core::config::load_config(std::path::Path::new(workspace)).unwrap_or_default();
    if cfg.src_dirs.is_empty() {
        return globs.to_vec();
    }
    let names: Vec<&str> = cfg
        .src_dirs
        .iter()
        .filter_map(|dir| dir.file_name().and_then(|name| name.to_str()))
        .collect();
    globs
        .iter()
        .map(|glob| guidance_core::query::glob::normalize_selection_glob(glob, &names))
        .collect()
}

/// Build a caller plan for one recall mode. `globs` are selection globs in
/// either workspace-relative (`src/<crate>/**`) or index-relative
/// (`<crate>/**`) spelling; both recall entries normalize them to the
/// indexed root-relative normal form before matching, so the two spellings
/// select the same files.
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
            // Symbol preference is intent-scoped, never forced: only
            // Symbol/Graph queries earn symbol-filtered routes. An
            // explicit flag for any other intent is ignored — light
            // verbs must not hijack mechanism queries.
            let scoped = matches!(
                guidance_core::read::intent::classify(query),
                QueryIntent::SingleIdentifier | QueryIntent::Graph
            );
            plan.prefer_symbol = Some(prefer_symbol && scoped);
            plan.globs = globs_opt;
            plan.symbol_types = symbol_types_opt;
            plan
        }
        SearchMode::Fts | SearchMode::Vector => {
            let route_mode = match mode {
                SearchMode::Fts => SearchPlanRouteMode::Fts,
                _ => SearchPlanRouteMode::Vector,
            };
            // Single-route modes honor the flag only for symbol-scoped
            // intents, same rule as the fused path.
            let scoped = matches!(
                guidance_core::read::intent::classify(query),
                QueryIntent::SingleIdentifier | QueryIntent::Graph
            );
            SearchPlan {
                routes: vec![SearchPlanRoute {
                    mode: route_mode,
                    query: query.to_string(),
                }],
                limit: Some(limit),
                trace,
                prefer_symbol: Some(prefer_symbol && scoped),
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
    include_fixtures: bool,
    freshen: bool,
) {
    let result = if freshen {
        run_search_impl(
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
            include_fixtures,
            true,
            3,
        )
    } else {
        run_search(
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
            include_fixtures,
        )
    };
    match result {
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
    include_fixtures: bool,
) -> Result<String, String> {
    run_search_impl(
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
        include_fixtures,
        false,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_search_impl(
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
    include_fixtures: bool,
    freshen: bool,
    rounds: u8,
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
    // Explicit `--vector` with no stored vectors is a named decline, never
    // a silent lemma fallback: the route is populated only by `sync` with
    // a configured backend.
    if mode == SearchMode::Vector {
        let empty = search_vector::GuidanceDb::open(Path::new(db_path))
            .map(|db| db.embedded_fragment_count().unwrap_or(0) == 0)
            .unwrap_or(true);
        if empty {
            return Err(
                "search --vector has no stored vectors: run `guidance sync` \
                 with an embedding backend configured (models.embed), then retry"
                    .to_string(),
            );
        }
    }
    // Per-mode limit default: L0 drains the sweep unless the caller
    // bounds it explicitly (line-budgeted early-kill dropped files
    // nondeterministically under parallel rg); recall modes keep the
    // CLI default. `usize::MAX` never trips the backend's
    // `hits.len() >= limit` kill, and renders as `rg_exhaustive`.
    let limit_option = limit;
    let limit = match (mode, limit) {
        (SearchMode::Rg, None) => usize::MAX,
        (_, None) => CLI_DEFAULT_LIMIT,
        (_, Some(requested)) => clamp_limit(requested),
    };

    if mode == SearchMode::Rg {
        return run_rg_search(query, workspace, limit, rg_options, globs);
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
    // One holder per command: Fuse, single-route, and capability-branch
    // recall share a single pipeline build, on first lemma need.
    let nlp_holder = LazyNlp::new();
    if mode == SearchMode::Fuse {
        return match recall_on_index(
            workspace,
            db_path,
            &plan,
            &nlp_holder,
            guidance_core::query::recall::RecallPolicy::default(),
        ) {
            Ok(hits) => {
                if hits.is_empty() {
                    return Ok("No results found.\n".to_string());
                }
                if freshen {
                    match freshen_hit_files(workspace, db_path, &hits) {
                        Ok(true) if rounds > 0 => {
                            return run_search_impl(
                                query,
                                workspace,
                                db_path,
                                limit_option,
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
                                include_fixtures,
                                true,
                                rounds - 1,
                            );
                        }
                        Ok(_) => {}
                        Err(error) => {
                            eprintln!("warning: freshen failed ({error}); serving indexed hits");
                        }
                    }
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
    let normalized_globs = normalize_globs_for_workspace(globs, workspace);
    let plan = build_search_plan(
        query,
        mode,
        limit,
        trace,
        prefer_symbol,
        &normalized_globs,
        &symbol_types,
    );
    // Rule-lemmatizer pipeline (no model): the L2 route collapses
    // inflections at zero embedding cost; `None` degrades to L1-only.
    // Built lazily on lemma need — recall always needs it here.
    // Shares the command holder created above.
    let nlp = nlp_holder.get();
    match run_recall(&plan, &storage, embedder.as_deref(), nlp) {
        Ok(output) => {
            let mut hits = output.hits;
            // Eval corpus never enters any arm, even opt-in.
            hits.retain(|hit| {
                !matches!(
                    guidance_core::source_scope::of_file(&hit.file),
                    guidance_core::source_scope::SourceScope::Eval
                )
            });
            if !include_fixtures {
                hits.retain(|hit| {
                    !matches!(
                        guidance_core::source_scope::of_file(&hit.file),
                        guidance_core::source_scope::SourceScope::Fixture
                    )
                });
            }
            if hits.is_empty() {
                return Ok("No results found.\n".to_string());
            }
            if freshen {
                match freshen_hit_files(workspace, db_path, &hits) {
                    Ok(true) if rounds > 0 => {
                        return run_search_impl(
                            query,
                            workspace,
                            db_path,
                            limit_option,
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
                            include_fixtures,
                            true,
                            rounds - 1,
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        eprintln!("warning: freshen failed ({error}); serving indexed hits");
                    }
                }
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

fn recall_on_index(
    workspace: &str,
    db_path: &str,
    plan: &SearchPlan,
    nlp: &LazyNlp,
    policy: guidance_core::query::recall::RecallPolicy,
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
    // Selection globs arrive in workspace-relative spelling; the fragment
    // index matches root-relative paths, so normalize once here — the
    // single workspace-aware boundary, never a second matcher.
    let mut plan = plan.clone();
    plan.globs = plan
        .globs
        .map(|globs| normalize_globs_for_workspace(&globs, workspace));
    plan.insensitive_globs = plan
        .insensitive_globs
        .map(|globs| normalize_globs_for_workspace(&globs, workspace));
    // Rule-lemmatizer pipeline (no model): the L2 route collapses
    // inflections at zero embedding cost; `None` degrades to L1-only.
    // Built lazily on lemma need by the caller's holder — recall always
    // needs it here. The holder is owned by the command entry so fused
    // recall, single-route recall, and the capability branch share one
    // pipeline build per command.
    let nlp_phase = crate::profile::Phase::new("recall.nlp_build");
    let nlp_ref = nlp.get();
    drop(nlp_phase);
    let _recall_phase = crate::profile::Phase::new("recall.run");
    match run_recall_with_policy(&plan, &storage, embedder.as_deref(), nlp_ref, policy) {
        Ok(output) => Ok(dedupe_dual_hits(
            output.hits,
            std::path::Path::new(workspace),
        )),
        Err(error) => Err(format!("search failed: {error}")),
    }
}

/// Machine-readable envelope v2 for orchestrators: every v1 key
/// plus the read-plane additions (coverage, claims, tokens,
/// latency, degraded). v1 keys keep their names and meaning;
/// `schema_version` 2 marks the additive set. Uncalibrated runs
/// report zero task-value confidence with an explicit shadow status —
/// the axis names the measurement, never a settled verdict.
#[must_use]
/// Machine-readable envelope v2 for orchestrators: every v1 key
/// plus the read-plane additions (coverage, claims, tokens,
/// latency, degraded). v1 keys keep their names and meaning;
/// `schema_version` 2 marks the additive set. Uncalibrated runs
/// report zero task-value confidence with an explicit shadow status —
/// the axis names the measurement, never a settled verdict.
pub fn envelope_json_v2(
    intent: &str,
    route: &str,
    answer: &guidance_core::read::engine::ReadAnswer,
    latency_ms: u64,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 2,
        "intent": intent,
        "route": route,
        "resolution": answer.route,
        "confidence": {"value": 0.0, "axis": "task-value (shadow)"},
        "gaps": answer.gaps,
        "citations": answer.citations.iter().map(|cite| serde_json::json!({
            "citation": cite.to_string(),
            "digest": cite.digest,
            "stale": cite.stale,
            "unverified": cite.unverified,
        })).collect::<Vec<_>>(),
        "coverage": answer.coverage,
        "claims": answer.claims.iter().map(|claim| serde_json::json!({
            "text": claim.text,
            "citation": claim.citation,
            "support": format!("{:?}", claim.support).to_lowercase(),
        })).collect::<Vec<_>>(),
        "tokens": answer.tokens_used,
        "latency_ms": latency_ms,
        "degraded": answer.degraded,
    })
}

/// Machine-readable envelope handoff for orchestrators: schema version,
/// intent, route, resolution, confidence with its axis, gaps, and
/// citations. Additive — the human sections render unchanged beside it.
#[must_use]
/// Machine-readable envelope handoff for orchestrators: schema version,
/// intent, route, resolution, confidence with its axis, gaps, and
/// citations. Additive — the human sections render unchanged beside it.
pub fn envelope_json(
    envelope: &guidance_core::query::envelope::AnswerEnvelope,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": envelope.schema_version,
        "intent": format!("{:?}", envelope.intent),
        "route": envelope.route,
        "resolution": envelope.resolution,
        "confidence": {
            "value": envelope.confidence.value,
            "axis": format!("{:?}", envelope.confidence.axis),
        },
        "gaps": envelope.gaps,
        "citations": envelope.citations,
    })
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

/// Converge the indexed files behind recall hits, then report
/// whether anything moved — a fresh tree pays stats only.
fn freshen_hit_files(workspace: &str, db_path: &str, hits: &[SearchHit]) -> Result<bool, String> {
    use guidance_core::sync_engine::absolute_or;
    let root = absolute_or(PathBuf::from(workspace));
    let db = GuidanceDb::open(Path::new(db_path))
        .map_err(|error| format!("cannot open index at {db_path}: {error}"))?;
    let mut files = Vec::new();
    for hit in hits {
        let absolute = absolute_or(PathBuf::from(&hit.file.absolute_path));
        if absolute.is_dir() || !absolute.starts_with(&root) || files.contains(&absolute) {
            continue;
        }
        files.push(absolute);
    }
    if files.is_empty() {
        return Ok(false);
    }
    crate::freshen::freshen_search_hits(&root, &db, Path::new(db_path), &files)
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

/// Standalone L0 search: managed rg under an `FsCapability` scope, rendered
/// Operator CLI enters the scope explicitly — backends stay fail-closed.
fn run_rg_search(
    query: &str,
    workspace: &str,
    limit: usize,
    rg_options: &RgDisplayOptions,
    globs: &[String],
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
    let rg_globs = globs.to_vec();
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
                    include_globs: rg_globs,
                    exclude_globs: Vec::new(),
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

#[cfg(test)]
#[path = "../tests/roadmap_pins.rs"]
mod roadmap_pins_tests;

#[cfg(test)]
#[path = "../tests/audit_baseline.rs"]
mod audit_baseline_tests;
