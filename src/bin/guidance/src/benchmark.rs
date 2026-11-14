//! `benchmark` command: orchestrator-cost comparison for the initial
//! exploration step — indexed `explain` vs unindexed literal grep vs
//! wholesale file loading. Per query it records wall time and estimated
//! tokens for each arm plus file-level recall/precision against a golden
//! relevant set; the aggregate reports totals and the token ratios that
//! tell an orchestrator what exploration costs.
//!
//! Deterministic and model-free: the `explain` arm renders the same
//! structural output as `explain --no-llm` (no model call on any path),
//! so hermetic runs need no endpoint. Model synthesis cost is out of
//! scope by design — identical across arms once the context is chosen.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::search;

/// Estimated tokens per character for output sizing (documented
/// approximation, not a tokenizer measurement).
pub const EST_CHARS_PER_TOKEN: usize = 4;

/// Grep exploration budget: at most this many files surface per query.
/// A real orchestrator stops reading into an unbounded match list too;
/// the cap is reported, never silent.
pub const GREP_FILE_BUDGET: usize = 50;

/// Per-file read cap for the grep/wholesale arms (bytes).
pub const MAX_READ_BYTES: u64 = 1_000_000;

/// Excerpt width for one grep match line.
pub const GREP_EXCERPT_CHARS: usize = 120;

/// One benchmark task: an exploration query plus the file set an
/// orchestrator should end up reading (basenames; see [`matches`]).
#[derive(Debug, Clone)]
/// One benchmark task: an exploration query plus the file set an
/// orchestrator should end up reading (basenames; see [`matches`]).
pub struct BenchTask {
    /// Stable task identity (reported, never scored).
    pub id: String,
    /// Exploration query, run verbatim through every arm.
    pub query: String,
    /// Golden relevant files (basenames; empty = control, must not fire).
    /// Controls must use codebase-absent tokens (nonce words verified
    /// against the corpus): the grep arm matches ANY identifier token
    /// and the FTS arm ORs tokens, so a real word fires almost
    /// everywhere and the control measures nothing.
    pub relevant: Vec<String>,
}

/// One arm's cost and value on one query.
#[derive(Debug, Clone, Serialize)]
/// One arm's cost and value on one query.
pub struct ArmResult {
    /// Wall time for the arm (milliseconds).
    pub wall_ms: u128,
    /// Rendered/read characters counted toward context.
    pub chars: usize,
    /// Estimated tokens (`chars` / [`EST_CHARS_PER_TOKEN`], rounded up).
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

/// Estimate tokens for `chars` characters (ceiling division).
#[must_use]
/// Estimate tokens for `chars` characters (ceiling division).
pub fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(EST_CHARS_PER_TOKEN)
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

/// Parse a tasks file: JSONL, one object per line with `id`, `query`,
/// and optional `relevant` (array of paths; basenames are compared).
/// Unknown fields are ignored; blank lines are skipped.
pub fn parse_tasks_file(text: &str) -> Result<Vec<BenchTask>, String> {
    let mut tasks = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        let id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("task")
            .to_string();
        let query = value
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("line {}: missing `query`", index + 1))?;
        let relevant = value
            .get("relevant")
            .and_then(|relevant| relevant.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .map(|path| {
                        Path::new(path)
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        tasks.push(BenchTask {
            id: format!("{id}-{}", tasks.len() + 1),
            query: query.to_string(),
            relevant,
        });
    }
    Ok(tasks)
}

/// Synthesize narrow tasks from the index itself: one
/// `where is <symbol> defined` task per symbol (alphabetical, first
/// `num`), relevant = every defining file. Ground truth comes from the
/// persisted graph rows, so the set is exact by construction. Module
/// pseudo-symbols (`$file`) and unnamed scopes never become tasks.
#[must_use]
/// Synthesize narrow tasks from the index itself: one
/// `where is <symbol> defined` task per symbol (alphabetical, first
/// `num`), relevant = every defining file. Ground truth comes from the
/// persisted graph rows, so the set is exact by construction. Module
/// pseudo-symbols (`$file`) and unnamed scopes never become tasks.
pub fn synthesize_tasks(db: &search_vector::GuidanceDb, num: usize) -> Vec<BenchTask> {
    let mut by_symbol: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if let Ok(rows) = db.graph_symbol_rows() {
        for row in rows {
            if row.name.is_empty() || row.name == "$file" {
                continue;
            }
            by_symbol
                .entry(row.name)
                .or_default()
                .insert(row.file.clone());
        }
    }
    by_symbol
        .into_iter()
        .take(num.max(1))
        .enumerate()
        .map(|(index, (name, files))| {
            let relevant: Vec<String> = files
                .into_iter()
                .map(|file| {
                    Path::new(&file)
                        .file_name()
                        .map(|base| base.to_string_lossy().into_owned())
                        .unwrap_or(file)
                })
                .collect();
            BenchTask {
                id: format!("synth-{}", index + 1),
                query: format!("where is {name} defined"),
                relevant,
            }
        })
        .collect()
}

/// Identifier-like query tokens (`[A-Za-z_][A-Za-z0-9_]{2,}`): the words
/// an orchestrator would lift from the question into a literal grep.
/// Short tokens (noise: `is`, `of`, `to`) never become patterns.
#[must_use]
/// Identifier-like query tokens (`[A-Za-z_][A-Za-z0-9_]{2,}`): the words
/// an orchestrator would lift from the question into a literal grep.
/// Short tokens (noise: `is`, `of`, `to`) never become patterns.
pub fn identifier_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, tokens: &mut Vec<String>| {
        if current.len() >= 3
            && current
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && !tokens.contains(current)
        {
            tokens.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    for ch in query.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            flush(&mut current, &mut tokens);
        }
    }
    flush(&mut current, &mut tokens);
    tokens
}

/// Walk `roots` once (same scope the index ingests: hidden dirs, `.git`,
/// and `.guidance` skipped, gitignore honored): every readable text file
/// under the byte cap, in sorted order. The single corpus behind both
/// the grep and wholesale arms.
pub fn walk_corpus(roots: &[PathBuf]) -> Vec<PathBuf> {
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
    files.sort();
    files.dedup();
    files
}

/// Literal case-insensitive grep over the corpus for ANY identifier
/// token: the broad-net first pass an orchestrator runs before reading
/// files. Files cap at [`GREP_FILE_BUDGET`] (reported); matches render
/// as `relpath:line:excerpt` (excerpt capped at [`GREP_EXCERPT_CHARS`]).
pub fn grep_arm(
    corpus: &[PathBuf],
    workspace: &str,
    tokens: &[String],
    budget: usize,
) -> (String, Vec<String>) {
    let lowered: Vec<String> = tokens.iter().map(|token| token.to_lowercase()).collect();
    let mut out = String::new();
    let mut files: Vec<String> = Vec::new();
    if lowered.is_empty() {
        return (out, files);
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
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
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
            files.push(
                path.file_name()
                    .map(|base| base.to_string_lossy().into_owned())
                    .unwrap_or(rel),
            );
        }
    }
    files.sort();
    files.dedup();
    (out, files)
}

/// Indexed arm: the structural `explain` output (no model call) over the
/// fused recall path — the same assembly `explain --no-llm` renders.
/// Recall failure degrades to empty (never a hard error), timed like
/// every other arm.
pub fn explain_arm(
    query: &str,
    workspace: &str,
    db_path: &str,
    limit: usize,
    jumps: Option<&guidance_core::jumps::JumpIndex>,
) -> (String, Vec<String>, Vec<String>) {
    // One holder per benchmarked query, shared across the arm's recall.
    let nlp = guidance_core::query::ingest::LazyNlp::new();
    let hits = search::fused_hits(query, workspace, db_path, limit, &nlp, guidance_core::query::recall::RecallPolicy::default(), jumps).unwrap_or_default();
    let cites = search::cite_hits(&hits, workspace);
    let rows = search::build_explain_rows(&hits, &cites);
    let workspace_root = PathBuf::from(workspace);
    let snippets = search::top_snippets(&rows, &hits, &workspace_root);
    let citations = search::cite_hits_live(&hits, &workspace_root);
    let suggestions = search::suggest_drill_downs(&snippets, 3);
    let mut files: Vec<String> = rows
        .iter()
        .map(|row| {
            Path::new(&row.source)
                .file_name()
                .map(|base| base.to_string_lossy().into_owned())
                .unwrap_or_else(|| row.source.clone())
        })
        .collect();
    // Rank order before the sorted/deduped scoring set below: rows
    // preserve fused rank, so first occurrence wins here.
    let mut seen_order = std::collections::HashSet::new();
    let mut order = Vec::new();
    for file in &files {
        if seen_order.insert(file.clone()) {
            order.push(file.clone());
        }
    }
    files.sort();
    files.dedup();
    let rendered = search::render_explain_full(
        query,
        &rows,
        &snippets,
        &citations,
        &[],
        None,
        &suggestions,
        &[],
        &[],
    );
    (rendered, files, order)
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
        vec![PathBuf::from(workspace)]
    } else {
        cfg.src_dirs
            .iter()
            .map(|dir| PathBuf::from(workspace).join(dir))
            .collect()
    };
    let corpus_started = Instant::now();
    let corpus_files = walk_corpus(&roots);
    let mut corpus_chars = 0usize;
    let mut corpus_basenames = BTreeSet::new();
    for path in &corpus_files {
        if let Ok(text) = std::fs::read_to_string(path) {
            corpus_chars += text.len();
        }
        if let Some(base) = path.file_name().and_then(|base| base.to_str()) {
            corpus_basenames.insert(base.to_string());
        }
    }
    let corpus = CorpusStats {
        files: corpus_files.len(),
        chars: corpus_chars,
        tokens: estimate_tokens(corpus_chars),
        wall_ms: corpus_started.elapsed().as_millis(),
    };
    let mut queries = Vec::with_capacity(tasks.len());
    // Jump terms load once per session beside the corpus walk: the
    // manifest is derivation-gated upstream, so a session sees one
    // parsed index, never per-query I/O.
    let jumps_holder = search::LazyJumps::new();
    let jumps = jumps_holder.get(
        std::path::Path::new(workspace),
        &std::path::Path::new(workspace).join(".guidance"),
    );
    for task in tasks {
        let relevant: BTreeSet<String> = task.relevant.iter().cloned().collect();
        let explain_started = Instant::now();
        let (explain_text, explain_files, explain_order) =
            explain_arm(&task.query, workspace, db_path, limit, jumps.as_ref());
        let explain_wall = explain_started.elapsed().as_millis();
        let (explain_recall, explain_precision) = score_arm(&explain_files, &relevant);
        let grep_started = Instant::now();
        let tokens = identifier_tokens(&task.query);
        let (grep_text, grep_files) = grep_arm(&corpus_files, workspace, &tokens, GREP_FILE_BUDGET);
        let grep_wall = grep_started.elapsed().as_millis();
        let (grep_recall, grep_precision) = score_arm(&grep_files, &relevant);
        let covered: Vec<String> = relevant
            .iter()
            .filter(|base| corpus_basenames.contains(base.as_str()))
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
        queries.push(QueryReport {
            id: task.id.clone(),
            query: task.query.clone(),
            relevant: task.relevant.clone(),
            explain: ArmResult {
                wall_ms: explain_wall,
                chars: explain_text.len(),
                tokens: estimate_tokens(explain_text.len()),
                files: explain_files,
                order: explain_order,
                recall: explain_recall,
                precision: explain_precision,
            },
            grep: ArmResult {
                wall_ms: grep_wall,
                chars: grep_text.len(),
                tokens: estimate_tokens(grep_text.len()),
                files: grep_files.clone(),
                order: grep_files,
                recall: grep_recall,
                precision: grep_precision,
            },
            wholesale: ArmResult {
                wall_ms: corpus.wall_ms,
                chars: corpus.chars,
                tokens: corpus.tokens,
                files: covered.clone(),
                order: covered,
                recall: wholesale_recall,
                precision: wholesale_precision,
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
pub fn render_human(report: &BenchReport) -> String {
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
            "\n### {}: {}\n  relevant: {relevant}\n",
            query.id, query.query
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

/// Entry point shared by the CLI dispatch: load tasks (file or
/// synthesized from the index), measure, render. `num` caps both
/// sources; `limit` sizes the explain arm like `explain --limit`.
pub fn cmd_benchmark(
    workspace: &str,
    db_path: &str,
    tasks_path: Option<&str>,
    num: usize,
    limit: usize,
    format: &str,
) {
    let tasks = match tasks_path {
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
            let db_file = PathBuf::from(db_path);
            if !db_file.is_file() {
                eprintln!("error: index not found at {db_path} (run `guidance sync` first)");
                std::process::exit(1);
            }
            match search_vector::GuidanceDb::open(&db_file) {
                Ok(db) => synthesize_tasks(&db, num.max(1)),
                Err(error) => {
                    eprintln!("error: cannot open index at {db_path}: {error}");
                    std::process::exit(1);
                }
            }
        }
    };
    if tasks.is_empty() {
        eprintln!("error: no benchmark tasks (empty tasks file or no symbols indexed)");
        std::process::exit(1);
    }
    let report = run_benchmark(workspace, db_path, &tasks, limit);
    match format {
        "json" => println!("{}", render_json(&report)),
        "human" => print!("{}", render_human(&report)),
        other => {
            eprintln!("error: unknown format '{other}' (human|json)");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
#[path = "../tests/benchmark_unit.rs"]
mod tests;
