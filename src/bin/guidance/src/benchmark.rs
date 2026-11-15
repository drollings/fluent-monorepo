//! `benchmark` command: orchestrator-cost comparison for the initial
//! exploration step — shipping `ReadEngine` vs targeted literal grep
//! vs wholesale file loading. Per query it records wall time and
//! estimated tokens for each arm plus file-level recall/precision
//! against a golden relevant set; the aggregate reports totals and
//! the token ratios that tell an orchestrator what exploration costs.
//!
//! Deterministic and model-free: the shipping arm renders the same
//! structural output as `explain --no-llm` (no model call on any
//! path), so hermetic runs need no endpoint. Model synthesis cost is
//! out of scope by design — identical across arms once the context is
//! chosen. Tasks come from a task file (the checked-in orientation
//! battery); nothing is synthesized from the index under test.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use guidance_core::eval::parse_eval_tasks;

/// Token accounting for every arm: the shared
/// `fluent_llm::tokens::estimate_tokens` (no second estimator).
#[must_use]
/// Token accounting for every arm: the shared
/// `fluent_llm::tokens::estimate_tokens` (no second estimator).
pub fn tokens_for_text(text: &str) -> usize {
    fluent_llm::tokens::estimate_tokens(text) as usize
}

/// Grep exploration budget: at most this many files surface per query.
/// A real orchestrator stops reading into an unbounded match list too;
/// the cap is reported, never silent.
pub const GREP_FILE_BUDGET: usize = 50;

/// Per-file read cap for the grep/wholesale arms (bytes).
pub const MAX_READ_BYTES: u64 = 1_000_000;

/// Excerpt width for one grep match line.
pub const GREP_EXCERPT_CHARS: usize = 120;

/// One benchmark task: the shared eval shape plus the original golden
/// paths (the basename list remains for continuity with older reports).
#[derive(Debug, Clone)]
/// One benchmark task: the shared eval shape plus the original golden
/// paths (the basename list remains for continuity with older reports).
pub struct BenchTask {
    /// Stable task identity.
    pub id: String,
    /// Query text.
    pub query: String,
    /// Golden relevant paths exactly as written in the task file.
    pub relevant_paths: Vec<String>,
    /// Golden relevant basenames (legacy scoring field).
    pub relevant: Vec<String>,
    /// Sections the answer must render.
    pub required_sections: Vec<String>,
    /// Tune or heldout split, carried for eval reporting.
    pub split: String,
    /// A control case must stay silent.
    pub control: bool,
}

/// One arm's cost and value on one query.
#[derive(Debug, Clone, Serialize)]
/// One arm's cost and value on one query.
pub struct ArmResult {
    /// Wall time for the arm (milliseconds).
    pub wall_ms: u128,
    /// Rendered/read characters counted toward context.
    pub chars: usize,
    /// Estimated tokens via the shared token accounting.
    pub tokens: usize,
    /// Surfaced files (basenames, sorted, deduped).
    pub files: Vec<String>,
    /// Surfaced files in rank order (basenames, deduped, first wins).
    /// Scoring reads `files`; this preserves the ordinal the reorder
    /// work moves, so rank effects stay measurable after future
    /// ranking changes. Grep/wholesale arms mirror `files` (no order).
    pub order: Vec<String>,
    /// Fraction of the relevant set surfaced (controls: 1.0 when empty).
    pub recall: f64,
    /// Fraction of surfaced files that are relevant (controls: 1.0/0.0).
    pub precision: f64,
    /// Fraction of the exact golden paths surfaced (controls: 1.0 when
    /// the surfaced set is empty).
    pub path_recall: f64,
    /// Fraction of surfaced paths that are exact golden paths.
    pub path_precision: f64,
    /// Required sections present in the rendered answer.
    pub required_sections_hit: usize,
    /// Citations that were not marked stale or unverified.
    pub citations_verified: usize,
    /// Surfaced paths by source scope (product/doc/test/fixture/eval/data).
    pub scope_mix: BTreeMap<String, usize>,
    /// A control task rendered no citations and no answer.
    pub control_abstained: bool,
}

/// One query measured three ways.
#[derive(Debug, Clone, Serialize)]
/// One query measured three ways.
pub struct QueryReport {
    /// Task identity.
    pub id: String,
    /// Query text.
    pub query: String,
    /// Golden relevant basenames.
    pub relevant: Vec<String>,
    /// Golden relevant paths exactly as written.
    pub relevant_paths: Vec<String>,
    /// Sections the answer must render.
    pub required_sections: Vec<String>,
    /// Tune or heldout split, carried for eval reporting.
    pub split: String,
    /// Indexed arm (structural `explain`, no model).
    pub explain: ArmResult,
    /// Unindexed literal-grep arm.
    pub grep: ArmResult,
    /// Wholesale arm (relevant files covered by the corpus).
    pub wholesale: ArmResult,
}

/// Whole-run report: per-query rows plus session aggregates.
#[derive(Debug, Clone, Serialize)]
/// Whole-run report: per-query rows plus session aggregates.
pub struct BenchReport {
    /// Workspace as given.
    pub workspace: String,
    /// Corpus behind the wholesale arm.
    pub corpus: CorpusStats,
    /// Per-query rows, input order.
    pub queries: Vec<QueryReport>,
    /// Per-arm totals and means.
    pub totals: BTreeMap<String, ArmTotals>,
    /// Token ratios over the whole session (query count included).
    pub ratios: BTreeMap<String, serde_json::Value>,
}

/// Session-level corpus behind the wholesale arm (measured once).
#[derive(Debug, Clone, Serialize)]
/// Session-level corpus behind the wholesale arm (measured once).
pub struct CorpusStats {
    /// Selected files walked.
    pub files: usize,
    /// Total bytes read.
    pub chars: usize,
    /// Estimated tokens for a full load.
    pub tokens: usize,
    /// Walk + read wall time (milliseconds).
    pub wall_ms: u128,
}

/// Per-arm session aggregate.
#[derive(Debug, Clone, Default, Serialize)]
/// Per-arm session aggregate.
pub struct ArmTotals {
    /// Summed wall time (milliseconds).
    pub wall_ms: u128,
    /// Summed estimated tokens.
    pub tokens: usize,
    /// Mean recall over queries.
    pub mean_recall: f64,
    /// Mean precision over queries.
    pub mean_precision: f64,
}

/// Basename-folded matching: a surfaced file counts when its basename is
/// in the relevant set. Prefixes (workspace vs src-root-relative) must
/// never decide relevance — only the file identity does.
#[must_use]
/// Basename-folded matching: a surfaced file counts when its basename is
/// in the relevant set. Prefixes (workspace vs src-root-relative) must
/// never decide relevance — only the file identity does.
pub fn matches(relevant: &BTreeSet<String>, surfaced: &str) -> bool {
    let base = Path::new(surfaced)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| surfaced.to_string());
    relevant.contains(&base)
}

/// Score one surfaced file list against the relevant basename set.
/// Empty relevant sets are controls: silence scores 1.0/1.0, any firing
/// scores 0.0/0.0.
#[must_use]
/// Score one surfaced file list against the relevant basename set.
/// Empty relevant sets are controls: silence scores 1.0/1.0, any firing
/// scores 0.0/0.0.
pub fn score_arm(surfaced: &[String], relevant: &BTreeSet<String>) -> (f64, f64) {
    if relevant.is_empty() {
        return if surfaced.is_empty() {
            (1.0, 1.0)
        } else {
            (0.0, 0.0)
        };
    }
    if surfaced.is_empty() {
        return (0.0, 1.0);
    }
    let hits = surfaced
        .iter()
        .filter(|file| matches(relevant, file))
        .count();
    (
        hits as f64 / relevant.len() as f64,
        hits as f64 / surfaced.len() as f64,
    )
}

/// Normalize a path for exact comparison; workspace-relative spellings
/// are the contract, but a leading `./` never decides a match.
#[must_use]
/// Normalize a path for exact comparison; workspace-relative spellings
/// are the contract, but a leading `./` never decides a match.
pub fn normalize_bench_path(path: &str) -> String {
    common_core::path::normalize_lexical(path)
        .trim_start_matches("./")
        .to_string()
}

/// Exact path-level scoring over workspace-relative paths. Controls
/// score 1.0 only when the surfaced set is silent.
#[must_use]
/// Exact path-level scoring over workspace-relative paths. Controls
/// score 1.0 only when the surfaced set is silent.
pub fn score_paths(surfaced: &[String], relevant: &BTreeSet<String>) -> (f64, f64) {
    if relevant.is_empty() {
        return if surfaced.is_empty() {
            (1.0, 1.0)
        } else {
            (0.0, 0.0)
        };
    }
    if surfaced.is_empty() {
        return (0.0, 1.0);
    }
    let relevant: BTreeSet<String> = relevant
        .iter()
        .map(|path| normalize_bench_path(path))
        .collect();
    let hits = surfaced
        .iter()
        .map(|path| normalize_bench_path(path))
        .filter(|path| relevant.contains(path))
        .count();
    (
        hits as f64 / relevant.len() as f64,
        hits as f64 / surfaced.len() as f64,
    )
}

/// Path scope for the acceptance report; the single classifier lives in
/// `guidance_core::source_scope`.
#[must_use]
/// Path scope for the acceptance report; the single classifier lives in
/// `guidance_core::source_scope`.
pub fn scope_mix(paths: &[String]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for path in paths {
        let scope = guidance_core::source_scope::classify(path, None).as_str();
        *out.entry(scope.to_string()).or_insert(0) += 1;
    }
    out
}

#[must_use]
pub fn required_sections_hit(text: &str, required: &[String]) -> usize {
    required
        .iter()
        .filter(|section| text.contains(&format!("### {section}")))
        .count()
}

#[must_use]
pub fn citations_verified_count(citations: &[String]) -> usize {
    citations
        .iter()
        .filter(|cite| !cite.contains("(stale)") && !cite.contains("(unverified)"))
        .count()
}

#[must_use]
pub fn control_abstained(text: &str, citations: &[String]) -> bool {
    citations.is_empty() && !text.contains("### Answer") && !text.contains("## Code")
}
pub fn parse_tasks_file(text: &str) -> Result<Vec<BenchTask>, String> {
    let tasks = parse_eval_tasks(text)?;
    Ok(tasks
        .into_iter()
        .enumerate()
        .map(|(index, task)| BenchTask {
            id: format!("{}-{}", task.id, index + 1),
            query: task.query,
            relevant_paths: task.relevant.clone(),
            relevant: task
                .relevant
                .iter()
                .map(|path| {
                    Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string())
                })
                .collect(),
            required_sections: task.required_sections,
            split: task.split,
            control: task.control,
        })
        .collect())
}

/// Walk `roots` once (same scope the index ingests: hidden dirs, `.git`,
/// and `.guidance` skipped, gitignore honored): every readable text file
/// under the byte cap, in sorted order, plus summed token estimates.
/// The single corpus behind both unindexed arms. Measurement fixtures
/// never enter: benchmark corpora and eval goldens are retained out.
pub fn walk_corpus(roots: &[PathBuf]) -> (Vec<PathBuf>, usize) {
    let mut files = Vec::new();
    for root in roots {
        if root.is_file() {
            files.push(root.clone());
            continue;
        }
        if !root.is_dir() {
            continue;
        }
        let filter = common_core::walk::WalkFilter {
            extensions: std::collections::HashSet::new(),
            max_bytes: Some(MAX_READ_BYTES),
            skip_binary: true,
            extra_skip_dirs: [".guidance".to_string(), ".git".to_string()]
                .into_iter()
                .collect(),
            honor_gitignore: true,
        };
        let mut skipped = Vec::new();
        common_core::walk::walk_files_filtered(
            root,
            &filter,
            &mut |entry: common_core::walk::WalkEntry| files.push(entry.path),
            &mut |path: &Path, _| skipped.push(path.to_path_buf()),
        );
        let _ = skipped;
    }
    // Measurement fixtures are not retrieval inputs: benchmark task
    // corpora and eval goldens never enter any arm, however the roots
    // are spelled. Component-wise match (not substring) so similarly
    // named user files never trip it.
    files.retain(|path| !is_measurement_path(path));
    files.sort();
    files.dedup();
    let tokens = files
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .map(|text| tokens_for_text(&text))
        .sum();
    (files, tokens)
}

/// Whether one walked path is a measurement fixture: the benchmark
/// task corpus or the eval goldens. Component-wise, separator-aware,
/// root-independent.
fn is_measurement_path(path: &Path) -> bool {
    let parts: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().replace('\\', "/")),
            _ => None,
        })
        .collect();
    parts.windows(3).any(|window| {
        (window[0] == "doc" && window[1] == "guidance" && window[2] == "eval")
            || (window[0] == "src" && window[1] == "guidance" && window[2] == "corpora")
    })
}

/// Targeted literal grep: files matching ANY significant term, with
/// `sed`-style `relpath:line:excerpt` matches. The targeting lives in
/// the vocabulary — callers pass the shared read-vocabulary content
/// terms (scaffolding and light verbs stripped), so a file of pure
/// scaffolding never surfaces. The match itself stays broad-net: one
/// distinctive term suffices, the orchestrator reads the excerpts.
/// Files cap at [`GREP_FILE_BUDGET`] (reported); matches render per
/// line (excerpt capped at [`GREP_EXCERPT_CHARS`]).
#[cfg(test)]
/// Targeted literal grep: files matching ANY significant term, with
/// `sed`-style `relpath:line:excerpt` matches. The targeting lives in
/// the vocabulary — callers pass the shared read-vocabulary content
/// terms (scaffolding and light verbs stripped), so a file of pure
/// scaffolding never surfaces. The match itself stays broad-net: one
/// distinctive term suffices, the orchestrator reads the excerpts.
/// Files cap at [`GREP_FILE_BUDGET`] (reported); matches render per
/// line (excerpt capped at [`GREP_EXCERPT_CHARS`]).
pub fn grep_arm(
    corpus: &[PathBuf],
    workspace: &str,
    tokens: &[String],
    budget: usize,
) -> (String, Vec<String>) {
    let (text, files, _paths) = grep_arm_with_paths(corpus, workspace, tokens, budget);
    (text, files)
}

/// Grep arm with the workspace-relative paths behind each basename.
#[must_use]
/// Grep arm with the workspace-relative paths behind each basename.
pub fn grep_arm_with_paths(
    corpus: &[PathBuf],
    workspace: &str,
    tokens: &[String],
    budget: usize,
) -> (String, Vec<String>, Vec<String>) {
    let lowered: Vec<String> = tokens.iter().map(|token| token.to_lowercase()).collect();
    let mut out = String::new();
    let mut files: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    if lowered.is_empty() {
        return (out, files, paths);
    }
    for path in corpus {
        if files.len() >= budget {
            break;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(workspace)
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
        let mut matched = false;
        for (index, line) in text.lines().enumerate() {
            let fold = line.to_lowercase();
            if !lowered.iter().any(|token| fold.contains(token)) {
                continue;
            }
            matched = true;
            let excerpt = line.trim().to_string();
            let excerpt = if excerpt.len() > GREP_EXCERPT_CHARS {
                common_core::string::truncate_utf8(&excerpt, GREP_EXCERPT_CHARS)
            } else {
                excerpt
            };
            out.push_str(&format!("{}:{}:{excerpt}\n", rel, index + 1));
        }
        if matched {
            paths.push(rel);
            files.push(
                path.file_name()
                    .map(|base| base.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            );
        }
    }
    files.sort();
    files.dedup();
    paths.sort();
    paths.dedup();
    (out, files, paths)
}

/// Indexed arm: the structural `explain` output (no model call) over the
/// fused recall path — the same assembly `explain --no-llm` renders.
/// Recall failure degrades to empty (never a hard error), timed like
/// every other arm.
/// Shipping arm: the `ReadEngine` answer over the committed index
/// (evidence-only, no model call), rendered as `explain --no-llm`
/// renders it. Files are citation basenames: sorted/deduped for
/// scoring, rank-ordered for the `order` set. An unreadable index
/// degrades to empty, never a hard error — the arm measures the
/// shipping path, including its failure modes.
#[must_use]
/// Shipping arm: the `ReadEngine` answer over the committed index
/// (evidence-only, no model call), rendered as `explain --no-llm`
/// renders it. Files are citation basenames: sorted/deduped for
/// scoring, rank-ordered for the `order` set. An unreadable index
/// degrades to empty, never a hard error — the arm measures the
/// shipping path, including its failure modes.
#[cfg(test)]
/// Indexed arm: the structural `explain` output (no model call) over the
/// fused recall path — the same assembly `explain --no-llm` renders.
/// Recall failure degrades to empty (never a hard error), timed like
/// every other arm.
/// Shipping arm: the `ReadEngine` answer over the committed index
/// (evidence-only, no model call), rendered as `explain --no-llm`
/// renders it. Files are citation basenames: sorted/deduped for
/// scoring, rank-ordered for the `order` set. An unreadable index
/// degrades to empty, never a hard error — the arm measures the
/// shipping path, including its failure modes.
/// Shipping arm: the `ReadEngine` answer over the committed index
/// (evidence-only, no model call), rendered as `explain --no-llm`
/// renders it. Files are citation basenames: sorted/deduped for
/// scoring, rank-ordered for the `order` set. An unreadable index
/// degrades to empty, never a hard error — the arm measures the
/// shipping path, including its failure modes.
pub fn explain_arm(
    query: &str,
    workspace: &str,
    db_path: &str,
    limit: usize,
) -> (String, Vec<String>, Vec<String>) {
    let (text, files, order, _paths, _citations) =
        explain_arm_detailed(query, workspace, db_path, limit);
    (text, files, order)
}

/// Detailed shipping arm: rendered text, basenames, rank order,
/// workspace-relative citation paths, and raw citations.
#[must_use]
/// Detailed shipping arm: rendered text, basenames, rank order,
/// workspace-relative citation paths, and raw citations.
pub fn explain_arm_detailed(
    query: &str,
    workspace: &str,
    db_path: &str,
    limit: usize,
) -> (String, Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    use guidance_core::read::engine::{ExplainContext, ExplainRequest, ReadEngine};
    let workspace_root = PathBuf::from(workspace);
    let Ok(db) = search_vector::GuidanceDb::open(Path::new(db_path)) else {
        return (
            String::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
    };
    let context = ExplainContext {
        workspace: &workspace_root,
        db: &db,
        nlp: None,
        coverage: None,
    };
    let request = ExplainRequest {
        query: query.to_string(),
        limit,
        budget_override: None,
        no_llm: true,
    };
    let answer = ReadEngine::new().explain(&request, &context, None);
    let rendered = guidance_core::read::render::render_answer(&answer);
    // Citation sources in rank order (first wins); basenames fold
    // workspace prefixes so scoring stays identity-based.
    let mut order = Vec::new();
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for citation in &answer.citations {
        let source = normalize_bench_path(citation.source.display());
        paths.push(source.clone());
        let base = Path::new(&source)
            .file_name()
            .map(|base| base.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.clone());
        if seen.insert(base.clone()) {
            order.push(base);
        }
    }
    let mut files = order.clone();
    files.sort();
    let raw_citations: Vec<String> = answer.citations.iter().map(ToString::to_string).collect();
    (rendered, files, order, paths, raw_citations)
}

/// Run every task through all three arms and aggregate. The wholesale
/// corpus is walked and read once per session (it is a one-time load,
/// not a per-query cost); per-query wholesale rows carry the shared
/// wall time and the covered relevant files.
pub fn run_benchmark(
    workspace: &str,
    db_path: &str,
    tasks: &[BenchTask],
    limit: usize,
) -> BenchReport {
    let cfg = guidance_core::config::load_config(Path::new(workspace)).unwrap_or_default();
    let roots: Vec<PathBuf> = if cfg.src_dirs.is_empty() {
        guidance_core::read::project_map::default_source_roots(Path::new(workspace))
    } else {
        cfg.src_dirs
            .iter()
            .map(|dir| PathBuf::from(workspace).join(dir))
            .collect()
    };
    let corpus_started = Instant::now();
    let (corpus_files, corpus_tokens) = walk_corpus(&roots);
    let mut corpus_chars = 0usize;
    let mut corpus_basenames = BTreeSet::new();
    let mut corpus_paths = Vec::new();
    for path in &corpus_files {
        if let Ok(text) = std::fs::read_to_string(path) {
            corpus_chars += text.len();
        }
        if let Some(base) = path.file_name().and_then(|base| base.to_str()) {
            corpus_basenames.insert(base.to_string());
        }
        if let Ok(rel) = path.strip_prefix(workspace) {
            corpus_paths.push(normalize_bench_path(&rel.to_string_lossy()));
        }
    }
    corpus_paths.sort();
    corpus_paths.dedup();
    let corpus = CorpusStats {
        files: corpus_files.len(),
        chars: corpus_chars,
        tokens: corpus_tokens,
        wall_ms: corpus_started.elapsed().as_millis(),
    };
    let mut queries = Vec::with_capacity(tasks.len());
    for task in tasks {
        let relevant: BTreeSet<String> = task.relevant.iter().cloned().collect();
        let relevant_paths: BTreeSet<String> = task.relevant_paths.iter().cloned().collect();
        let explain_started = Instant::now();
        let (explain_text, explain_files, explain_order, explain_paths, explain_citations) =
            explain_arm_detailed(&task.query, workspace, db_path, limit);
        let explain_wall = explain_started.elapsed().as_millis();
        let (explain_recall, explain_precision) = score_arm(&explain_files, &relevant);
        let (explain_path_recall, explain_path_precision) =
            score_paths(&explain_paths, &relevant_paths);
        let explain_sections = required_sections_hit(&explain_text, &task.required_sections);
        let explain_verified = citations_verified_count(&explain_citations);
        let explain_scope = scope_mix(&explain_paths);
        let explain_control = control_abstained(&explain_text, &explain_citations);
        let grep_started = Instant::now();
        let tokens = guidance_core::read::vocabulary::content_terms(&task.query);
        let (grep_text, grep_files, grep_paths) =
            grep_arm_with_paths(&corpus_files, workspace, &tokens, GREP_FILE_BUDGET);
        let grep_wall = grep_started.elapsed().as_millis();
        let (grep_recall, grep_precision) = score_arm(&grep_files, &relevant);
        let (grep_path_recall, grep_path_precision) = score_paths(&grep_paths, &relevant_paths);
        let covered: Vec<String> = relevant
            .iter()
            .filter(|base| corpus_basenames.contains(base.as_str()))
            .cloned()
            .collect();
        let covered_paths: Vec<String> = relevant_paths
            .iter()
            .filter(|path| corpus_paths.contains(&normalize_bench_path(path)))
            .cloned()
            .collect();
        // Wholesale covers whatever relevant files exist in the
        // corpus; precision prices the load (relevant over the corpus).
        let wholesale_recall = if relevant.is_empty() {
            if covered.is_empty() {
                1.0
            } else {
                0.0
            }
        } else {
            covered.len() as f64 / relevant.len() as f64
        };
        let wholesale_precision = if corpus_files.is_empty() {
            1.0
        } else {
            covered.len() as f64 / corpus_files.len() as f64
        };
        let (wholesale_path_recall, wholesale_path_precision) =
            score_paths(&covered_paths, &relevant_paths);
        queries.push(QueryReport {
            id: task.id.clone(),
            query: task.query.clone(),
            relevant: task.relevant.clone(),
            relevant_paths: task.relevant_paths.clone(),
            required_sections: task.required_sections.clone(),
            split: task.split.clone(),
            explain: ArmResult {
                wall_ms: explain_wall,
                chars: explain_text.len(),
                tokens: tokens_for_text(&explain_text),
                files: explain_files,
                order: explain_order,
                recall: explain_recall,
                precision: explain_precision,
                path_recall: explain_path_recall,
                path_precision: explain_path_precision,
                required_sections_hit: explain_sections,
                citations_verified: explain_verified,
                scope_mix: explain_scope,
                control_abstained: explain_control,
            },
            grep: ArmResult {
                wall_ms: grep_wall,
                chars: grep_text.len(),
                tokens: tokens_for_text(&grep_text),
                files: grep_files.clone(),
                order: grep_files,
                recall: grep_recall,
                precision: grep_precision,
                path_recall: grep_path_recall,
                path_precision: grep_path_precision,
                required_sections_hit: 0,
                citations_verified: 0,
                scope_mix: scope_mix(&grep_paths),
                control_abstained: task.control && grep_text.trim().is_empty(),
            },
            wholesale: ArmResult {
                wall_ms: corpus.wall_ms,
                chars: corpus.chars,
                tokens: corpus.tokens,
                files: covered.clone(),
                order: covered,
                recall: wholesale_recall,
                precision: wholesale_precision,
                path_recall: wholesale_path_recall,
                path_precision: wholesale_path_precision,
                required_sections_hit: 0,
                citations_verified: 0,
                scope_mix: scope_mix(&covered_paths),
                control_abstained: task.control && covered_paths.is_empty(),
            },
        });
    }
    let mut totals = BTreeMap::new();
    for arm in ["explain", "grep", "wholesale"] {
        let rows: Vec<&ArmResult> = queries
            .iter()
            .map(|query| match arm {
                "grep" => &query.grep,
                "wholesale" => &query.wholesale,
                _ => &query.explain,
            })
            .collect();
        let wall_ms = rows.iter().map(|row| row.wall_ms).sum();
        let tokens = rows.iter().map(|row| row.tokens).sum();
        let mean_recall = mean(rows.iter().map(|row| row.recall));
        let mean_precision = mean(rows.iter().map(|row| row.precision));
        totals.insert(
            arm.to_string(),
            ArmTotals {
                wall_ms,
                tokens,
                mean_recall,
                mean_precision,
            },
        );
    }
    // Wholesale is a one-time session load: its token total is the
    // corpus cost once, not once per query.
    if let Some(wholesale) = totals.get_mut("wholesale") {
        wholesale.tokens = corpus.tokens;
        wholesale.wall_ms = corpus.wall_ms;
    }
    let mut ratios = BTreeMap::new();
    ratios.insert(
        "queries".to_string(),
        serde_json::Value::from(queries.len() as u64),
    );
    let wholesale_tokens = corpus.tokens.max(1) as f64;
    for arm in ["explain", "grep"] {
        let total = totals.get(arm).map(|row| row.tokens).unwrap_or(0) as f64;
        ratios.insert(
            format!("{arm}_tokens_per_wholesale"),
            serde_json::Value::from(total / wholesale_tokens),
        );
    }
    BenchReport {
        workspace: workspace.to_string(),
        corpus,
        queries,
        totals,
        ratios,
    }
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for value in values {
        sum += value;
        count += 1;
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// Human rendering: per-query arm rows plus session totals and the
/// token ratios that price exploration.
#[must_use]
/// Human rendering: per-query arm rows plus session totals and the
/// token ratios that price exploration.
pub fn render_human(report: &BenchReport, scope_report: bool) -> String {
    let mut out = format!(
        "## Benchmark: {} queries ({})\n\n",
        report.queries.len(),
        report.workspace
    );
    out.push_str(&format!(
        "Corpus (wholesale): {} files, {} chars, ~{} tokens, read in {}ms\n",
        report.corpus.files, report.corpus.chars, report.corpus.tokens, report.corpus.wall_ms
    ));
    for query in &report.queries {
        let relevant = if query.relevant.is_empty() {
            "(control: must stay empty)".to_string()
        } else {
            query.relevant.join(", ")
        };
        out.push_str(&format!(
            "\n### {}: {}\n  relevant: {relevant}\n  path recall: {:.2} path precision: {:.2} required sections: {}/{}\n",
            query.id,
            query.query,
            query.explain.path_recall,
            query.explain.path_precision,
            query.explain.required_sections_hit,
            query.required_sections.len()
        ));
        for (name, arm) in [
            ("explain", &query.explain),
            ("grep", &query.grep),
            ("wholesale", &query.wholesale),
        ] {
            let top: Vec<String> = arm.order.iter().take(5).cloned().collect();
            out.push_str(&format!(
                "  {name:9} {:>6}ms ~{:>7} tok files {:>3} recall {:.2} precision {:.2} top [{}]\n",
                arm.wall_ms,
                arm.tokens,
                arm.files.len(),
                arm.recall,
                arm.precision,
                top.join(", ")
            ));
            if scope_report {
                let scopes: Vec<String> = arm
                    .scope_mix
                    .iter()
                    .map(|(scope, count)| format!("{scope}:{count}"))
                    .collect();
                out.push_str(&format!("            scopes [{}]\n", scopes.join(", ")));
            }
        }
    }
    out.push_str("\n### Totals\n");
    for arm in ["explain", "grep", "wholesale"] {
        if let Some(total) = report.totals.get(arm) {
            out.push_str(&format!(
                "  {arm:9} {:>6}ms ~{:>7} tok mean recall {:.2} mean precision {:.2}\n",
                total.wall_ms, total.tokens, total.mean_recall, total.mean_precision
            ));
        }
    }
    out.push_str("### Ratios (whole session)\n");
    let mut ratios: Vec<(&String, &serde_json::Value)> = report.ratios.iter().collect();
    ratios.sort_by(|left, right| left.0.cmp(right.0));
    for (name, value) in ratios {
        match value.as_f64() {
            Some(number) => out.push_str(&format!("  {name}: {number:.3}\n")),
            None => out.push_str(&format!("  {name}: {value}\n")),
        }
    }
    out
}

/// JSON rendering for agents: the full report object, stable keys.
#[must_use]
/// JSON rendering for agents: the full report object, stable keys.
pub fn render_json(report: &BenchReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string())
}

/// Entry point shared by the CLI dispatch: load tasks from a task
/// file, measure, render. `num` caps the file; `limit` sizes the
/// shipping arm like `explain --limit`. Tasks are never synthesized
/// from the index under test (self-indexed tasks retrieve their own
/// answer key); the checked-in orientation battery lives at
/// `src/guidance/corpora/benchmark_tasks.jsonl`.
pub fn cmd_benchmark(
    workspace: &str,
    db_path: &str,
    tasks_path: Option<&str>,
    num: usize,
    limit: usize,
    format: &str,
    scope_report: bool,
) {
    let tasks: Vec<BenchTask> = match tasks_path {
        Some(path) => {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    eprintln!("error: cannot read tasks file {path}: {error}");
                    std::process::exit(1);
                }
            };
            match parse_tasks_file(&text) {
                Ok(tasks) => tasks.into_iter().take(num.max(1)).collect(),
                Err(error) => {
                    eprintln!("error: {error}");
                    std::process::exit(1);
                }
            }
        }
        None => {
            eprintln!(
                "error: no tasks file (pass --tasks; the checked-in battery is src/guidance/corpora/benchmark_tasks.jsonl)"
            );
            std::process::exit(1);
        }
    };
    if tasks.is_empty() {
        eprintln!("error: no benchmark tasks (empty tasks file)");
        std::process::exit(1);
    }
    let report = run_benchmark(workspace, db_path, &tasks, limit);
    match format {
        "json" => println!("{}", render_json(&report)),
        "human" => print!("{}", render_human(&report, scope_report)),
        other => {
            eprintln!("error: unknown format '{other}' (human|json)");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
#[path = "../tests/benchmark_unit.rs"]
mod tests;
