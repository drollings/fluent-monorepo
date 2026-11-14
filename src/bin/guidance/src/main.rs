#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use common_core::ensure_dir_or_panic;
use common_core::shell::run_command;
use fluent_concurrency::pool::ResultPoolError;
use guidance_core::config;
use guidance_core::query::ingest::LazyNlp;
use guidance_core::runtime;
use guidance_core::sync_engine::SyncEngine;
use guidance_core::walk;
use search_vector::GuidanceDb;

mod benchmark;
mod embed;
mod index_cmd;
mod mcp;
mod profile;
mod search;
mod structure;
mod summarize_cmd;

/// Concurrent sidecar regenerations per propagation wave. Bounded so a
/// hub change cannot overrun the AST pool's queue.
const PROPAGATION_CONCURRENCY: usize = 16;

/// In-flight cap for AST pool submissions: the pool queue is finite, so
/// unbounded concurrent submits fail instead of waiting. Every member-gen
/// submit below (walk and propagation regen) holds a permit, bounding
/// queue occupancy on thousand-file syncs.
const AST_SUBMIT_IN_FLIGHT: usize = 8;

/// Pool submit with backpressure: waits for a permit instead of failing
/// when the pool queue is full. A closed semaphore maps to the pool's
/// canceled verdict; callers report it like any other pool error.
async fn submit_ast(
    pool: &Arc<
        fluent_concurrency::pool::ResultPool<
            runtime::AstGenPayload,
            fluent_types::GuidanceDoc,
            guidance_core::sync_engine::SyncEngineError,
        >,
    >,
    semaphore: &Arc<tokio::sync::Semaphore>,
    payload: runtime::AstGenPayload,
) -> Result<
    fluent_types::GuidanceDoc,
    fluent_concurrency::pool::ResultPoolError<guidance_core::sync_engine::SyncEngineError>,
> {
    let _permit = semaphore
        .acquire()
        .await
        .map_err(|_| fluent_concurrency::pool::ResultPoolError::Canceled)?;
    pool.submit(payload).await
}

#[derive(Parser)]
#[command(
    name = "guidance",
    about = "AST-guided vector search & edge AI orchestrator"
)]
struct Cli {
    /// First word still searches: a bare `guidance <query>` runs `search`.
    #[arg(index = 1)]
    query: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(global = true, long)]
    debug: bool,

    /// Refuse anything but fresh: read paths exit 2 naming stale files
    /// instead of serving labelled results.
    #[arg(global = true, long)]
    strict: bool,

    #[arg(global = true, long)]
    show_prompts: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Answer a question from the index, with live snippets, citations,
    /// ranked doc paragraphs, and a local-model synthesis unless declined.
    /// Doc scope is the configured `docs_dir` plus the repo-root
    /// `README.md`/`AGENTS.md` (`root_docs` in the config, on by
    /// default; set it to false to restore docs-dir-only scope).
    Explain {
        query: String,

        #[arg(long, default_value = ".guidance")]
        guidance: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(short = 'l', long, default_value_t = 10)]
        limit: usize,

        #[arg(long)]
        no_llm: bool,

        #[arg(long, default_value = "auto")]
        filter: String,
    },
    Test,
    Init {
        #[arg(default_value = ".")]
        dir: String,

        #[arg(short = 'g', long, default_value = ".guidance")]
        guidance_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
    },
    Sync {
        /// Sync one file only: member gen plus the fragment-index half
        /// (ingest with sig-gated dependent propagation) scoped to it.
        /// Re-running on an unchanged file is a no-op in both halves.
        #[arg(short, long)]
        file: Option<String>,

        #[arg(long)]
        scan: Option<String>,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(long, default_value = ".guidance")]
        json_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        #[arg(long)]
        force: bool,

        #[arg(long)]
        no_db: bool,

        #[arg(long)]
        dry_run: bool,

        #[arg(long)]
        verbose: bool,

        #[arg(long)]
        watch: bool,

        #[arg(long, default_value_t = 500)]
        watch_debounce_ms: u64,

        /// Autonomy cap for the fix pre-pass (`L0` index only, `L1` and
        /// up run the deterministic fixers the mechanical list allows).
        #[arg(long, default_value = "L2")]
        autonomy: String,

        /// Also run the background comment-summary pass after the index
        /// rebuilds (opt-in; the query path never calls a model).
        #[arg(long)]
        summarize: bool,
    },
    Status {
        #[arg(short = 'g', long, default_value = ".guidance")]
        guidance_dir: String,
    },
    Clean {
        #[arg(long, default_value = ".guidance")]
        json_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
    },
    Commit {
        #[arg(long)]
        dry_run: bool,

        #[arg(long)]
        debug: bool,

        #[arg(long)]
        force: bool,
    },
    Check {
        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        /// Script contract: fast readiness probe only (no test/lint/fmt
        /// subprocesses), single-line READY / NOT READY output, exit code
        /// carries the verdict.
        #[arg(long)]
        check_ready: bool,

        /// Converge the mirror to a fixpoint before checking: regenerate
        /// stale sidecars in bounded passes and report what, if anything,
        /// remains. Baseline stages still verify only.
        #[arg(long)]
        fixpoint: bool,
    },
    Structure {
        #[arg(long, default_value = ".guidance")]
        json_dir: String,
    },
    Mcp {
        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(long, default_value = ".guidance")]
        json_dir: String,

        #[arg(long, default_value = "agent")]
        toolset: String,
    },
    Search {
        query: String,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        /// Result limit. Absent = mode default (L0 drains the sweep;
        /// recall modes use 10). Present values clamp to [1, 50].
        #[arg(short = 'l', long)]
        limit: Option<usize>,

        #[arg(long)]
        fts: bool,

        #[arg(long)]
        vector: bool,

        #[arg(long)]
        rg: bool,

        #[arg(long)]
        fuse: bool,

        #[arg(long)]
        trace: bool,

        #[arg(long)]
        compact: bool,

        #[arg(long)]
        prefer_symbol: bool,

        #[arg(long)]
        glob: Vec<String>,

        #[arg(long)]
        symbol_type: Vec<String>,

        #[arg(short = 'A', long, default_value_t = 0)]
        after: usize,

        #[arg(short = 'B', long, default_value_t = 0)]
        before: usize,

        #[arg(short = 'C', long, default_value_t = 0)]
        context: usize,

        #[arg(long)]
        mtime_after: Option<i64>,

        #[arg(long)]
        mtime_before: Option<i64>,
    },
    Index {
        #[arg(default_value = ".")]
        path: String,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(long, default_value = ".guidance")]
        json_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        #[arg(long)]
        force: bool,
    },
    /// Converge the workspace: sync the mirror, fix what deterministic
    /// rungs can fix (format, machine-applicable lint), reindex, and
    /// report. Exit 0 clean, 1 work remains, 2 stuck with escalations,
    /// 4 halted by the breaker.
    Run {
        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(long, default_value = ".guidance")]
        json_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        /// Wave budget for the fix loop.
        #[arg(long, default_value_t = 4)]
        max_waves: usize,

        /// Autonomy cap (`L0` index only, `L1` and up run the
        /// deterministic fixers the mechanical list allows).
        #[arg(long, default_value = "L2")]
        autonomy: String,
    },
    /// Knowledge-base summary for orchestrators: mirror freshness,
    /// member counts, index presence, and the commands that consume
    /// this workspace. JSON on stdout, stable keys.
    Kb {
        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(long, default_value = ".guidance")]
        json_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
    },
    ComponentGate {
        #[arg(default_value = ".")]
        path: String,
    },
    Scorecard {
        /// Output format: human or json.
        #[arg(long, default_value = "human")]
        format: String,

        /// Deep tier: per-query breakdown plus repeated timing.
        #[arg(long)]
        deep: bool,

        /// Live tier: print the exact model-gated commands (never runs them).
        #[arg(long)]
        live: bool,

        /// Checked-in baseline for ratchet comparison.
        #[arg(long, default_value = "src/guidance/scorecard_baseline.json")]
        baseline: String,

        /// Rewrite the baseline from a fresh tier run (explicit command only).
        #[arg(long)]
        update_baseline: bool,

        /// Human reason recorded with the baseline update (required).
        #[arg(long, default_value = "")]
        reason: String,

        /// Allow the update to record an adverse move (still recorded).
        #[arg(long)]
        allow_regression: bool,

        /// Tolerance override `name=value` (repeatable).
        #[arg(long)]
        tolerance: Vec<String>,

        /// Absolute bound override `name=value` (repeatable).
        #[arg(long)]
        bound: Vec<String>,
    },
    Seal {
        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,
    },
    /// Orchestrator-cost comparison for initial exploration: indexed
    /// `explain` vs unindexed literal grep vs wholesale file loading.
    /// Per query it measures wall time and estimated tokens per arm plus
    /// file-level recall/precision against a golden relevant set, then
    /// aggregates totals and token ratios. Deterministic, model-free.
    Benchmark {
        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        /// Tasks file (JSONL: `id`, `query`, `relevant` paths; extra
        /// fields ignored). Absent: synthesize `where is <symbol>
        /// defined` tasks from the index.
        #[arg(long)]
        tasks: Option<String>,

        /// Task cap (both file and synthesized sources).
        #[arg(long, default_value_t = 10)]
        num: usize,

        /// Fused-recall limit behind the explain arm (like `explain`).
        #[arg(short = 'l', long, default_value_t = 10)]
        limit: usize,

        /// Output format: human or json (json is the agent handoff).
        #[arg(long, default_value = "human")]
        format: String,
    },
    /// Summarize file and node comments in the background (opt-in;
    /// never on the query path). Writes sidecars plus the DB projection.
    Summarize {
        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(long, default_value = ".guidance")]
        json_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        /// Also brief the doc set into the capabilities table (opt-in;
        /// never on the query path). Runs instead of the summary pass.
        #[arg(long, default_value_t = false)]
        capabilities: bool,
    },
    Impact {
        /// Symbol name or file path whose impact to report.
        target: String,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,
    },
    /// Post one unit of work to the queue (validated; never touches
    /// running items). Prints the resulting queue snapshot as JSON.
    QueuePost {
        /// Stable id (also the graph key).
        #[arg(long)]
        id: String,

        /// Component scope the narrowest gate covers.
        #[arg(long, default_value = "")]
        scope: String,

        /// Fix class (closed set: rustfmt, clippy-fix, index,
        /// commentary-refresh, plan-item).
        #[arg(long)]
        kind: String,

        /// Dependency ids, comma-separated; every one must be queued.
        #[arg(long, default_value = "")]
        deps: String,

        /// Queue snapshot file to load and save (created when absent).
        #[arg(long, default_value = ".guidance-queue.json")]
        queue: String,
    },
    /// File one escalation as open in the ledger file (created when absent).
    EscalationFile {
        #[arg(long)]
        id: String,

        #[arg(long, default_value = ".guidance-escalations.json")]
        ledger: String,
    },
    /// Claim an open escalation for an owner.
    EscalationClaim {
        #[arg(long)]
        id: String,

        #[arg(long)]
        owner: String,

        #[arg(long, default_value = ".guidance-escalations.json")]
        ledger: String,
    },
    /// Resolve a claimed (or open, with owner) escalation.
    EscalationResolve {
        #[arg(long)]
        id: String,

        #[arg(long)]
        owner: String,

        #[arg(long, default_value = ".guidance-escalations.json")]
        ledger: String,
    },
    /// Import plan items from a JSON array file into a reviewable plan.
    PlanImport {
        /// Plan identity.
        #[arg(long)]
        id: String,

        /// JSON array of `{id, target, package, test?}` items.
        #[arg(long)]
        file: String,
    },
    /// Tail the journal ledger: last groups at or after a cursor.
    LedgerTail {
        #[arg(long, default_value = ".guidance/journal")]
        journal_dir: String,

        #[arg(long, default_value_t = 0)]
        since: usize,

        #[arg(long, default_value_t = 10)]
        count: usize,
    },
    /// Audit one source file against its sidecar: freshness, skew,
    /// and receipt verdicts. Exit 0 when fresh, else 1.
    Verify {
        /// Source file to audit (workspace-relative or absolute).
        path: String,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        /// Also print recorded-vs-live hashes and the clock reading.
        #[arg(long)]
        deep: bool,
    },
    /// Request a context pack from a JSON `PackInput` file.
    Pack {
        #[arg(long)]
        file: String,
    },
    /// Measure a decision point on a labelled corpus plus harvested
    /// journal negatives; store a calibration entry only when the
    /// precision/recall bars pass. Measurement is never an error.
    Calibrate {
        /// Decision point (`dp.triage_residual`, `dp.route_query`, `dp.capability`).
        #[arg(long)]
        point: String,
        /// Gold corpus JSON (see `src/guidance/corpora/`).
        #[arg(long)]
        dataset: String,
        /// Measured procedure identity (default: the static procedure).
        #[arg(long)]
        model_fingerprint: Option<String>,
        /// State dir holding `calibration.json`.
        #[arg(long, default_value = ".guidance/state")]
        state_dir: String,
        /// Journal dir to harvest reverted drafts from.
        #[arg(long, default_value = ".guidance/journal")]
        journal_dir: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let _ = cli.show_prompts;

    if cli.debug {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .init();
    }

    // Observes total command time. Per-unit observation is wired via
    // `Instrumented::with_metrics` in the browser-copilot handler. The
    // histogram here captures the outer command dispatch duration.
    let cmd_histogram = std::sync::Arc::new(common_core::LatencyHistogram::new());
    let cmd_start = std::time::Instant::now();

    match cli.command.as_ref() {
        Some(Commands::Explain {
            query,
            guidance,
            db,
            workspace,
            limit,
            no_llm,
            filter,
        }) => {
            if cli.strict && strict_refuse(workspace) {
                std::process::exit(2);
            }
            if index_mismatched(db) {
                eprintln!("refused: index generation mismatch (re-run sync to rebuild)");
                std::process::exit(2);
            }
            cmd_explain(
                query, guidance, db, workspace, *limit, *no_llm, filter, cli.strict,
            )
            .await;
        }
        Some(Commands::Test) => cmd_test(),
        Some(Commands::Init {
            dir,
            guidance_dir: _,
            db: _,
        }) => cmd_init(dir),
        Some(Commands::Sync {
            file,
            scan,
            workspace,
            json_dir,
            db,
            force,
            no_db,
            dry_run,
            verbose,
            watch,
            watch_debounce_ms,
            autonomy,
            summarize,
        }) => {
            cmd_sync(
                file.as_deref(),
                scan.as_deref(),
                workspace,
                json_dir,
                db,
                *force,
                *no_db,
                *dry_run,
                *verbose,
                *watch,
                *watch_debounce_ms,
                autonomy,
                *summarize,
            )
            .await;
        }
        Some(Commands::Status { guidance_dir }) => cmd_status(guidance_dir),
        Some(Commands::Verify {
            path,
            workspace,
            deep,
        }) => cmd_verify(path, workspace, *deep),
        Some(Commands::Clean { json_dir, db }) => cmd_clean(json_dir, db),
        Some(Commands::Commit { .. }) => {
            removed_command("commit", "guidance check or guidance status");
        }
        Some(Commands::Check {
            workspace,
            check_ready,
            fixpoint,
        }) => cmd_check(workspace, *check_ready, *fixpoint),
        Some(Commands::Structure { json_dir }) => cmd_structure(json_dir),
        Some(Commands::Mcp {
            db,
            workspace,
            json_dir,
            toolset,
        }) => {
            cmd_mcp(db, workspace, json_dir, toolset);
        }
        Some(Commands::Search {
            query,
            workspace,
            db,
            limit,
            fts,
            vector,
            rg,
            fuse,
            trace,
            compact,
            prefer_symbol,
            glob,
            symbol_type,
            after,
            before,
            context,
            mtime_after,
            mtime_before,
        }) => {
            if cli.strict && strict_refuse(workspace) {
                std::process::exit(2);
            }
            // rg reads live bytes; every other mode reads the index.
            if !rg && refuse_stale_index(db) {
                std::process::exit(2);
            }
            let rg_options = search::RgDisplayOptions {
                before: (*before).max(*context),
                after: (*after).max(*context),
                mtime_after_ms: *mtime_after,
                mtime_before_ms: *mtime_before,
            };
            search::cmd_search(
                query,
                workspace,
                db,
                *limit,
                *fts,
                *vector,
                *rg,
                *fuse,
                *trace,
                *compact,
                *prefer_symbol,
                glob,
                symbol_type,
                &rg_options,
            );
        }
        Some(Commands::Index {
            path,
            workspace,
            json_dir,
            db: _,
            force,
        }) => {
            // The fragment index (`GuidanceDb`) is populated by `sync --db`;
            // `index` owns member-doc generation for the path scope.
            index_cmd::cmd_index(workspace, json_dir, Some(path), *force);
        }
        Some(Commands::Run {
            workspace,
            json_dir,
            db,
            max_waves,
            autonomy,
        }) => cmd_run(workspace, json_dir, db, *max_waves, autonomy).await,
        Some(Commands::Kb {
            workspace,
            json_dir,
            db,
        }) => {
            cmd_kb(workspace, json_dir, db);
        }
        Some(Commands::ComponentGate { path }) => cmd_component_gate(path),
        Some(Commands::Seal { workspace }) => cmd_seal(workspace),
        Some(Commands::Benchmark {
            workspace,
            db,
            tasks,
            num,
            limit,
            format,
        }) => {
            benchmark::cmd_benchmark(workspace, db, tasks.as_deref(), *num, *limit, format);
        }
        Some(Commands::Summarize {
            workspace,
            json_dir,
            db,
            capabilities,
        }) => cmd_summarize(workspace, json_dir, db, *capabilities).await,
        Some(Commands::Impact { target, workspace }) => cmd_impact(target, workspace),
        Some(Commands::QueuePost {
            id,
            scope,
            kind,
            deps,
            queue,
        }) => cmd_queue_post(id, scope, kind, deps, queue),
        Some(Commands::EscalationFile { id, ledger }) => cmd_escalation_file(id, ledger),
        Some(Commands::EscalationClaim { id, owner, ledger }) => {
            cmd_escalation_claim(id, owner, ledger)
        }
        Some(Commands::EscalationResolve { id, owner, ledger }) => {
            cmd_escalation_resolve(id, owner, ledger)
        }
        Some(Commands::PlanImport { id, file }) => cmd_plan_import(id, file),
        Some(Commands::LedgerTail {
            journal_dir,
            since,
            count,
        }) => cmd_ledger_tail(journal_dir, *since, *count),
        Some(Commands::Pack { file }) => cmd_pack(file),
        Some(Commands::Calibrate {
            point,
            dataset,
            model_fingerprint,
            state_dir,
            journal_dir,
        }) => cmd_calibrate(
            point,
            dataset,
            model_fingerprint.clone(),
            state_dir,
            journal_dir,
        ),
        Some(Commands::Scorecard {
            format,
            deep,
            live,
            baseline,
            update_baseline,
            reason,
            allow_regression,
            tolerance,
            bound,
        }) => {
            cmd_scorecard(
                format,
                *deep,
                *live,
                baseline,
                *update_baseline,
                reason,
                *allow_regression,
                tolerance,
                bound,
            )
            .await;
        }
        None => {
            // First word still searches: `guidance <query>` is `search`.
            if let Some(query) = &cli.query {
                if cli.strict && strict_refuse(".") {
                    std::process::exit(2);
                }
                search::cmd_search(
                    query,
                    ".",
                    ".guidance.db",
                    None,
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    &[],
                    &[],
                    &search::RgDisplayOptions {
                        before: 0,
                        after: 0,
                        mtime_after_ms: None,
                        mtime_before_ms: None,
                    },
                );
            } else {
                eprintln!("No command given. Try `guidance --help` or `guidance <query>`.");
                std::process::exit(2);
            }
        }
    }

    cmd_histogram.observe_duration(cmd_start);
    if cli.debug && cmd_histogram.count() > 0 {
        eprintln!(
            "[telemetry] command latency: count={} sum_ms={} p50={}ms p99={}ms",
            cmd_histogram.count(),
            cmd_histogram.sum_ms(),
            cmd_histogram.estimate_percentile(50.0),
            cmd_histogram.estimate_percentile(99.0),
        );
    }
}

fn load_project_config(workspace: &Path) -> config::ProjectConfig {
    config::load_config(workspace).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
async fn cmd_explain(
    query: &str,
    guidance_dir: &str,
    db_path: &str,
    workspace: &str,
    limit: usize,
    no_llm: bool,
    _filter: &str,
    strict: bool,
) {
    // The guidance dir flag is workspace-relative by convention;
    // resolve once so sidecar mapping works from any cwd.
    let guidance_path = PathBuf::from(guidance_dir);
    let guidance_path = if guidance_path.is_absolute() {
        guidance_path
    } else {
        PathBuf::from(workspace).join(guidance_path)
    };
    let guidance_dir = guidance_path.to_string_lossy().into_owned();
    let _total = profile::Phase::new("explain.total");
    // One holder per command: fused recall, branch-1 recall, and
    // branch-2 pulls share a single pipeline build, on first lemma need.
    let nlp = LazyNlp::new();
    let jumps_holder = search::LazyJumps::new();
    let workspace_root = PathBuf::from(workspace);
    let cfg = load_project_config(&workspace_root);
    let calibration_registry = fluent_llm::decision::CalibrationRegistry::load(
        &PathBuf::from(&guidance_dir)
            .join("state")
            .join("calibration.json"),
    )
    .unwrap_or_else(|_| fluent_llm::decision::CalibrationRegistry::empty());
    // Recall policy: `Act` only with a passing summary-index entry for the
    // generating model, `Disabled` otherwise (byte-identical recall).
    // Every recall entry below inherits the same gate.
    let recall_policy = search::recall_policy_for(&calibration_registry, &cfg.generative.model);
    // `callers of <symbol>` resolves through persisted call edges
    // (hydrated graph, no re-parse), never keyword search: graph hits
    // bypass the relevance gate (its prose-token bar does not apply to
    // edge answers). Unknown symbols stay honestly empty.
    let callers_symbol = search::parse_callers_query(query);
    let recall_phase = profile::Phase::new("explain.recall");
    let (hits, briefs, class, route, capability_ready) = match callers_symbol {
        Some(symbol) => {
            let db_file = PathBuf::from(db_path);
            let graph_hits = if !db_file.exists() {
                Vec::new()
            } else {
                match search_vector::GuidanceDb::open(&db_file) {
                    Ok(db) => match search::graph_callers_hits(&db, workspace, &symbol, limit) {
                        Ok(hits) => hits,
                        Err(error) => {
                            eprintln!("Warning: explain callers failed ({error})");
                            Vec::new()
                        }
                    },
                    Err(error) => {
                        eprintln!("Warning: explain recall failed (cannot open index at {db_path}: {error})");
                        Vec::new()
                    }
                }
            };
            drop(recall_phase);
            if graph_hits.is_empty() {
                print!("{}", search::render_empty_explain(query));
                return;
            }
            let mut fsm = guidance_core::query::strategy::FsmEngine::new();
            let class = fsm.run(query);
            let route = fsm.route(&class).to_string();
            (
                graph_hits,
                Vec::new(),
                class,
                route,
                0,
            )
        }
        None => {
            // Intent-routed fused recall through the explain adapter: the
            // library classifies (intent + route) and envelops the fused
            // hits; the CLI keeps its relevance gate, capability branch,
            // and renderers below. Any recall failure degrades to empty,
            // never a hard error — the empty state below is the stable
            // contract. One holder serves fused recall and the capability
            // branch: the pipeline builds at most once per command, on
            // first lemma need.
            let adaptation = search::explain_adapter(
                query,
                workspace,
                db_path,
                limit,
                &nlp,
                recall_policy,
                jumps_holder
                    .get(
                        std::path::Path::new(workspace),
                        std::path::Path::new(guidance_dir.as_str()),
                    )
                    .as_ref(),
            );
            if adaptation.hits.is_empty()
                && !std::path::Path::new(db_path).is_file()
            {
                eprintln!("Warning: explain recall failed (index not found at {db_path})");
            }
            let search::ExplainAdaptation {
                hits,
                class,
                route,
                ..
            } = adaptation;
            drop(recall_phase);

            // Relevance gate: a hit survives on a structural match (query token
            // in the symbol name) or two distinct query tokens in its text;
            // prose-only single-token hits never render. File stems alone never
            // qualify. Single-token queries keep everything (narrow intent). Gated-empty (hits
            // existed, none relevant) is the honest empty state — no snippets,
            // no citations, no model calls — instead of rendering junk for the
            // answer to confabulate from. Deterministic: --no-llm behaves
            // identically, and hermetic tests pin the narrow path byte-for-byte.
            let gate_phase = profile::Phase::new("explain.relevance_gate");
            let tokens = search::significant_query_tokens(query);
            let hits = search::filter_relevant_hits(&hits, &tokens);
            drop(gate_phase);
            // Advisory finding filter (measure, never steer): per-finding
            // verdicts from the gate-plus-overlay signals above, applied
            // under the sufficiency point. Live authorization is
            // ShadowOnly, so the kept set is the input set in input
            // order — the seam is live and pinned, steering waits on
            // calibration.
            let finding_auth =
                guidance_core::query::decision_points::authorize_point_with(
                    &calibration_registry,
                    guidance_core::query::decision_points::SUFFICIENCY_POINT,
                    &cfg.decision.model,
                    guidance_core::query::decision_points::SUFFICIENCY_VERSION,
                );
            let findings = search::findings_for_hits(&hits);
            let verdicts = search::deterministic_finding_verdicts(&findings, &tokens);
            let outcome =
                search::advisory_filter_findings(findings, &verdicts, &finding_auth);
            debug_assert!(
                outcome.kept.len() == hits.len(),
                "shadow-regime filter must not drop findings"
            );
            // Capability branch phase 1 (no model): code-thin relevant
            // hits open SVO-ranked briefs. Pulls wait for judging below;
            // the merge after pulls keeps files rendering once.
            // Untriggered queries skip the branch entirely.
            let branch_phase = profile::Phase::new("explain.capability");
            let branch = search::recall_capability_briefs_for(query, db_path, &hits, &nlp);
            drop(branch_phase);
            let (hits, briefs, capability_ready) = (hits, branch.briefs, branch.ready);
            (hits, briefs, class, route, capability_ready)
        }
    };
    if hits.is_empty() && briefs.is_empty() {
        print!("{}", search::render_empty_explain(query));
        return;
    }

    // Capability judge (shadow): the uncalibrated decision model scores
    // each brief 0-100; scores tier the render but never filter or
    // reorder it. Unjudged briefs (no model, transport failure,
    // unparseable) render as `considered` in deterministic SVO order.
    // Every score logs a shadow record for the judge corpus.
    //
    // Fail-closed tiers: the scored tiers render at most `Considered`
    // until the capability point carries a passing calibration entry.
    // The judge is measured, never obeyed, on the live path — scores
    // still log shadow records, and `Strong` unlocks only when
    // `calibrate` authorizes the point (measure-only today, so the cap
    // holds until then).
    let capability_authorized = guidance_core::query::decision_points::authorize_point_with(
        &calibration_registry,
        guidance_core::capability::CAPABILITY_POINT,
        &cfg.decision.model,
        search::CAPABILITY_JUDGE_VERSION,
    )
    .is_calibrated();
    let mut scored_briefs: Vec<search::ScoredBrief> = briefs
        .iter()
        .map(|brief| search::ScoredBrief {
            brief: brief.clone(),
            score: None,
        })
        .collect();
    if !no_llm && cfg.decision.enabled && !briefs.is_empty() {
        let mut client =
            fluent_llm::client::LlmClient::new(&cfg.decision.base_url, &cfg.decision.model);
        client.config.timeout_ms = cfg.decision.timeout_ms;
        let judge_phase = profile::Phase::new("explain.capability_judge");
        scored_briefs = search::judge_capability_briefs(query, &briefs, &client);
        drop(judge_phase);
        for scored in &scored_briefs {
            if let Some(score) = scored.score {
                let tier = search::capability_tier_label(search::capability_tier(Some(score)));
                let record = guidance_core::query::shadow::capability_judge_shadow(
                    &scored.brief.id,
                    score,
                    tier,
                    scored.brief.rank,
                );
                tracing::info!(
                    target: "capability_shadow",
                    point = record.point.as_str(),
                    brief = scored.brief.id.as_str(),
                    score = score,
                    tier = tier,
                    rank = scored.brief.rank,
                    verdict = record.verdict.as_str(),
                    would_have_acted = record.would_have_acted,
                    brief_json = %search::scored_brief_json(scored),
                    "capability judge shadow",
                );
            }
        }
    }



    let display_tier = |scored: &search::ScoredBrief| -> search::CapabilityTier {
        let tier = search::capability_tier(scored.score);
        if capability_authorized || tier != search::CapabilityTier::Strong {
            tier
        } else {
            search::CapabilityTier::Considered
        }
    };
    // Phase 2 (no model): judged standings gate traversal, then merge.
    // Runs after judging (needs tiers) but before rows and citations
    // (pulls render). Note the ordering consequence: with briefs present
    // the judge model call precedes `--strict` refusal; without briefs
    // nothing model-touched runs before strict, as before. Judged
    // `Considered` never pulls; unjudged rank-1 keeps the labelled
    // pre-calibration exception.
    let judged = !no_llm && cfg.decision.enabled && !briefs.is_empty();
    let mut standings: std::collections::HashMap<String, search::BriefStanding> =
        std::collections::HashMap::new();
    for scored in &scored_briefs {
        standings.insert(
            scored.brief.id.clone(),
            if judged {
                search::BriefStanding::Judged(display_tier(scored))
            } else {
                search::BriefStanding::Unjudged
            },
        );
    }
    let pulls = search::capability_branch_pulls(
        &briefs,
        &standings,
        workspace,
        db_path,
        &search::significant_query_tokens(query),
        &nlp,
        recall_policy,
    );
    let (hits, hit_tags, dual_tags) =
        search::merge_branch_hits(hits, &pulls, Path::new(workspace));

    let cite_phase = profile::Phase::new("explain.citations");
    let cites = search::cite_hits(&hits, workspace);
    let mut rows = search::build_explain_rows(&hits, &cites);
    drop(cite_phase);
    // Freshness is labelled on the row, never hidden: files whose
    // sidecar is missing or older than the source read stale. Row
    // sources are src-root-relative, so matching resolves against the
    // workspace plus every src dir (a bare workspace join misses src/
    // layouts and --strict silently serves stale rows).
    let stale_phase = profile::Phase::new("explain.staleness");
    let stale_src_dirs = src_dirs_from_config(Path::new(workspace));
    let stale = search::stale_hit_sources(&hits, &guidance_dir, &stale_src_dirs);
    drop(stale_phase);
    for row in &mut rows {
        if search::explain_row_is_stale(&row.source, workspace, &stale_src_dirs, &stale) {
            row.cite.push_str(" (stale)");
        }
    }
    if strict {
        let bad: Vec<&str> = rows
            .iter()
            .filter(|row| row.cite == "unverified" || row.cite.ends_with("(stale)"))
            .map(|row| row.source.as_str())
            .collect();
        if !bad.is_empty() {
            eprintln!(
                "refused: {} stale or unverified citations (strict mode)",
                bad.len()
            );
            for source in &bad {
                eprintln!("  stale: {source}");
            }
            std::process::exit(2);
        }
    }

    // Top-match snippets with live bounds, byte-identical duplicates
    // removed. Tree-sitter positions resolve on the fly, so an edit
    // after indexing moves the lines instead of serving stale ones.
    let snippets = search::top_snippets(&rows, &hits, &workspace_root);
    let citations: Vec<String> = search::cite_hits_live(&hits, &workspace_root);

    // Doc paragraphs from the configured documentation tree plus the
    // repo-root orientation files (`README.md`, `AGENTS.md`, enabled
    // by the `root_docs` config, on by default), ranked by query-term
    // overlap at query time (no index writes), then gated by the local
    // decision model: only items at or above the floor are cited as
    // judged. Ranked-but-unjudged paragraphs (backend disabled,
    // unreachable, or unparseable) render labelled `[unjudged]` instead
    // of vanishing — producer overlap is shown, never trusted. Unjudged
    // prose never enters synthesis (the answer builder takes snippets
    // and Strong briefs only). No model call under --no-llm, so no
    // docs either.
    let mut docs = Vec::new();
    let mut docs_judged = false;
    if !no_llm {
        let docs_dir = if cfg.docs_dir.is_absolute() {
            cfg.docs_dir.clone()
        } else {
            workspace_root.join(&cfg.docs_dir)
        };
        let include_roots = cfg.root_docs;
        let roots_present = include_roots
            && (workspace_root.join("README.md").is_file()
                || workspace_root.join("AGENTS.md").is_file());
        if docs_dir.is_dir() || roots_present {
            let docs_opt = docs_dir.is_dir().then(|| docs_dir.clone());
            let rank_phase = profile::Phase::new("explain.docs_rank");
            // Ranked paragraphs are a pure function of (query tokens,
            // doc contents): memoize per tree fingerprint so repeats
            // skip the walk. Hits return byte-identical lists; a miss
            // re-ranks and records. Cache file lives beside the index
            // marker, never under an ingested root.
            let cache_path = PathBuf::from(&guidance_dir).join(search::DOC_RANK_CACHE_FILE);
            let mut rank_cache = search::DocRankCache::load(&cache_path);
            let (ranked, cache_changed) = search::rank_doc_paragraphs_cached(
                query,
                docs_opt.as_deref(),
                &workspace_root,
                search::EXPLAIN_DOC_JUDGE_K,
                include_roots,
                &mut rank_cache,
            );
            drop(rank_phase);
            if cache_changed {
                if let Err(error) = rank_cache.save(&cache_path) {
                    eprintln!("Warning: docs rank cache not written: {error}");
                }
            }
            if !ranked.is_empty() && cfg.decision.enabled {
                let mut client =
                    fluent_llm::client::LlmClient::new(&cfg.decision.base_url, &cfg.decision.model);
                client.config.timeout_ms = cfg.decision.timeout_ms;
                let judge_phase = profile::Phase::new("explain.docs_judge");
                let judged: Vec<(String, String)> = search::judge_doc_paragraphs(query, &ranked, &client)
                    .into_iter()
                    .take(search::EXPLAIN_DOC_TOP_K)
                    .collect();
                drop(judge_phase);
                if !judged.is_empty() {
                    docs = judged;
                    docs_judged = true;
                }
            }
            if !ranked.is_empty() && !docs_judged {
                docs = ranked
                    .into_iter()
                    .take(search::EXPLAIN_DOC_TOP_K)
                    .map(|(path, excerpt)| (format!("[unjudged] {path}"), excerpt))
                    .collect();
            }
        }
    }

    // Concise local-model answer over the snippets, unless declined.
    // No snippets means no evidence: skip the call instead of inviting
    // an invented answer. Transport failure degrades to a labelled
    // unavailable line, never a hard error. Strong briefs join the
    // synthesis context; considered briefs render without feeding it.
    let strong_briefs: Vec<String> = scored_briefs
        .iter()
        .filter(|scored| display_tier(scored) == search::CapabilityTier::Strong)
        .map(|scored| scored.brief.purpose.clone())
        .collect();
    let mut answer: Option<String> = None;
    if !no_llm && !snippets.is_empty() {
        let role = &cfg.generative;
        if role.enabled {
            let mut client = fluent_llm::client::LlmClient::new(&role.base_url, &role.model);
            client.config.timeout_ms = role.timeout_ms;
            match search::answer_with_backend(
                query,
                &snippets,
                &strong_briefs,
                &client,
                &role.model,
            ) {
                // Structural enforcement outside the model: every cited
                // path:line must resolve to shown evidence, else the
                // abstain-with-evidence template replaces the draft.
                Ok(Some(text)) => {
                    answer = Some(search::constrain_answer_to_evidence(&text, &snippets));
                }
                Ok(None) => answer = Some("(unavailable: empty answer)".to_string()),
                Err(error) => {
                    let detail = error.trim().to_string();
                    let detail = if detail.len() > 200 {
                        common_core::string::truncate_utf8(&detail, 200)
                    } else {
                        detail
                    };
                    answer = Some(format!("(unavailable: {detail})"));
                }
            }
        } else {
            answer = Some("(unavailable: model disabled)".to_string());
        }
    }

    let suggestions = search::suggest_drill_downs(&snippets, 3);
    // Capability section lines plus per-snippet branch tags: base
    // snippets read untagged, branch-2 files carry their brief's
    // crate (dual-provenance files tag the single render).
    let capabilities: Vec<(String, String)> = scored_briefs
        .iter()
        .map(|scored| {
            let tier = search::capability_tier_label(display_tier(scored));
            (
                format!(
                    "[{tier}] {}/{} ({}#{})",
                    scored.brief.crate_name,
                    scored.brief.domain,
                    scored.brief.source_path,
                    scored.brief.anchor
                ),
                scored.brief.purpose.clone(),
            )
        })
        .collect();
    let mut file_tags: Vec<(String, String)> = dual_tags;
    for (hit, tag) in hits.iter().zip(hit_tags.iter()) {
        if let Some(crate_name) = tag {
            file_tags.push((
                hit.file.relative_path.clone(),
                format!("via capability: {crate_name}"),
            ));
        }
    }
    let snippet_tags = search::tag_snippets_by_file(&snippets, &file_tags);
    // Tier honesty: the capabilities table holds zero ready briefs, so
    // the branch above could not fire for lack of data — say so in one
    // line rather than rendering a section that is always absent. A
    // populated table with no ranked match stays silent (tier available,
    // nothing relevant); only the empty tier is labelled. The count
    // travels with the branch result — no second index open.
    let capability_tier_note = match capability_ready {
        0 if briefs.is_empty() => {
            "\n> capability briefs: none indexed — run is unaffected\n"
        }
        _ => "",
    };
    // Machine handoff beside the human rendering: the library envelope
    // rebuilt over the final post-gate hits, so intent, route, gaps, and
    // the confidence axis describe what actually rendered. Additive —
    // parsers that stop at the suggestions block are unaffected.
    let envelope = search::envelope_for_hits(&class, &route, &hits);
    let envelope_block = match serde_json::to_string_pretty(&search::envelope_json(&envelope)) {
        Ok(json) => format!("\n### Envelope\n```json\n{json}\n```\n"),
        Err(_) => String::new(),
    };
    print!(
        "{}{}{}",
        search::render_explain_full(
            query,
            &rows,
            &snippets,
            &citations,
            &docs,
            answer.as_deref(),
            &suggestions,
            &capabilities,
            &snippet_tags,
        ),
        capability_tier_note,
        envelope_block
    );
}

fn cmd_test() {
    println!("Running tests...");
    let args: Vec<&str> = vec!["cargo", "test", "--workspace"];
    let ok = run_command(&args);
    if !ok {
        std::process::exit(1);
    }
}

fn cmd_init(dir: &str) {
    let d = Path::new(dir).join(".guidance");
    ensure_dir_or_panic(&d);
    let config_path = d.join("guidance-config.json");
    if !config_path.exists() {
        let default_config = serde_json::json!({
            "version": "1",
            "guidance_dir": ".guidance",
            "db_path": ".guidance.db",
            "skills_dir": "doc/skills",
            "capabilities_dir": "doc/capabilities",
            "src_dirs": ["src"],
            "providers": {
                "local": { "base_url": "http://localhost:11434", "chat_endpoint": "/v1/chat/completions" },
                "ollama": { "base_url": "http://localhost:11434", "chat_endpoint": "/api/chat" }
            },
            "models": {
                "default": "ollama:code:latest",
                "fast": "ollama:code:latest",
                "thinking": "ollama:code:latest",
                "batch": "ollama:code:latest",
                "embed": "ollama:embed:latest"
            },
            "embed": { "dims": 768, "cache_limit": 400 },
            "decision": { "base_url": "http://localhost:8080", "model": "liquidai/d1-2b", "timeout_ms": 30000, "enabled": true },
            "generative": { "base_url": "http://localhost:8080", "model": "code", "timeout_ms": 30000, "enabled": true },
            "decision_thresholds": {},
            "calibration_registry": ".guidance/calibration.json"
        });
        common_core::io::write_atomic(
            &config_path,
            serde_json::to_string_pretty(&default_config)
                .unwrap()
                .as_bytes(),
        )
        .expect("write config");
    }
    println!("Initialized guidance in {}", d.display());
}

#[allow(clippy::too_many_arguments)]
async fn cmd_summarize(workspace: &str, json_dir: &str, db_path: &str, capabilities: bool) {
    let workspace_path = PathBuf::from(workspace);
    let guidance_dir = PathBuf::from(json_dir);
    let cfg = load_project_config(&workspace_path);
    if capabilities {
        match summarize_cmd::run_capabilities(&guidance_dir, db_path, &workspace_path, &cfg, false)
            .await
        {
            Ok(report) => {
                println!(
                    "Briefed {} chunks ({} cached) across {} docs: {} failed, {} unavailable, {} gaps.",
                    report.briefed,
                    report.cached,
                    report.docs,
                    report.failed,
                    report.unavailable,
                    report.gaps,
                );
            }
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let src_dirs = src_dirs_from_config_with(&workspace_path, &cfg);
    match summarize_cmd::run_summarize(&guidance_dir, db_path, &src_dirs, &cfg, false).await {
        Ok(report) => {
            println!(
                "Summarized {} nodes ({} cached) across {} files: {} failed, {} unavailable, {} gaps.",
                report.summarized,
                report.cached,
                report.files,
                report.failed,
                report.unavailable,
                report.gaps,
            );
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

async fn cmd_sync(
    file: Option<&str>,
    scan: Option<&str>,
    workspace: &str,
    json_dir: &str,
    db_path: &str,
    force: bool,
    no_db: bool,
    dry_run: bool,
    verbose: bool,
    watch: bool,
    watch_debounce_ms: u64,
    autonomy: &str,
    summarize: bool,
) {
    let _total = profile::Phase::new("sync.total");
    let workspace_path = PathBuf::from(workspace);
    let guidance_dir = PathBuf::from(json_dir);
    let cfg = load_project_config(&workspace_path);

    if dry_run {
        println!("Dry run — no files will be written.");
    }

    if let Some(path) = file {
        let source_path = Path::new(path);
        if !source_path.exists() {
            eprintln!("error: file not found: {path}");
            std::process::exit(1);
        }
        // Sidecars live in a parallel tree under the guidance dir, never
        // beside their sources: resolve the file against the configured
        // source roots so the staleness read and member-gen write agree
        // on one `guidance_dir/src/<rel>.json` path.
        let src_dirs = src_dirs_from_config_with(&workspace_path, &cfg);
        let source_dir = find_source_dir(source_path, &src_dirs).unwrap_or_else(|| {
            if source_path.is_dir() {
                source_path.to_path_buf()
            } else {
                source_path.parent().unwrap_or(Path::new(".")).to_path_buf()
            }
        });
        ensure_dir_or_panic(&guidance_dir);

        if source_path.is_dir() {
            let engine = SyncEngine::new(guidance_dir.clone(), source_dir.clone());
            let status = engine.status().expect("status");
            if verbose {
                println!(
                    "Generated guidance for directory ({} stale)",
                    status.stale_files
                );
            }
        } else {
            // Single shared mapping: the engine resolves exactly what
            // the pipeline writes, so this read never disagrees with it.
            let engine = SyncEngine::new(guidance_dir.clone(), source_dir.clone());
            let json_path = engine.sidecar_path(source_path);
            // Stored source hashes for the member-JSON clock gate (empty on
            // any failure — the gate then keeps mtime behavior).
            let stored_hash: Option<String> = search_vector::GuidanceDb::open(Path::new(db_path))
                .ok()
                .and_then(|db| db.source_content_hashes().ok())
                .and_then(|map| {
                    map.get(
                        guidance_core::sync_engine::absolute_or(source_path.to_path_buf())
                            .to_string_lossy()
                            .as_ref(),
                    )
                    .cloned()
                });
            let fresh = match stored_hash.as_deref() {
                Some(stored) => !guidance_core::sync::staleness::should_generate_with_stored(
                    &json_path,
                    source_path,
                    Some(stored),
                    || guidance_core::diff::hash_file(source_path),
                ),
                None => !guidance_core::sync::staleness::should_generate(&json_path, source_path),
            };
            if !force && fresh {
                if verbose {
                    println!("  skip (up to date): {path}");
                }
                return;
            }
            // Fix pre-pass scoped to the file: lint, fix, and format
            // before member gen and indexing, so both converge on fixed
            // bytes. L0 and dry runs skip the rewrite half. Progress
            // stays quiet unless verbose; only escalations speak, with
            // the complete error as handoff.
            let mut fix_escalated: Vec<guidance_core::waves::Escalation> = Vec::new();
            let mut fix_drafts = guidance_core::waves::ModelDraftReport::default();
            if !dry_run && source_path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                let mut policy = cfg.policy.clone();
                policy.autonomy = autonomy.to_string();
                if guidance_core::policy::autonomy_allows_fix(&policy) {
                    let _phase = profile::Phase::new("sync.fix");
                    let scope = vec![source_path.to_path_buf()];
                    let fix = run_fix_scope(
                        &scope,
                        &guidance_dir,
                        &cfg,
                        &policy,
                        SYNC_FIX_MAX_WAVES,
                        verbose,
                    );
                    if verbose && (fix.waves.fixed > 0 || !fix.waves.escalated.is_empty()) {
                        println!(
                            "fix: {} fixed ({} formatted), {} escalated, drafts kept {}",
                            fix.waves.fixed,
                            fix.waves.formatted,
                            fix.waves.escalated.len(),
                            fix.drafts.kept
                        );
                    }
                    fix_escalated = fix.waves.escalated;
                    fix_drafts = fix.drafts;
                }
            }
            match runtime::ast_pool()
                .submit(runtime::AstGenPayload {
                    source_path: source_path.to_path_buf(),
                    source_dir,
                    guidance_dir: guidance_dir.clone(),
                    config: guidance_core::sync_engine::GenConfig::default(),
                })
                .await
            {
                Ok(doc) => {
                    if verbose {
                        println!("  gen: {path} ({} members)", doc.members.len());
                        println!("Generated guidance for {path}");
                        println!(
                            "  {} members, language: {}",
                            doc.members.len(),
                            doc.meta.language
                        );
                    }
                    // Scoped derived-index half: the same ingest plus
                    // sig-gated propagation the full sync runs, limited
                    // to this file (the purge stays root-scoped, so
                    // neighbors are never doomed). Skipped under
                    // `--no-db`/`--dry-run` like the full path.
                    if !no_db && !dry_run {
                        let scoped = vec![guidance_core::sync_engine::absolute_or(
                            source_path.to_path_buf(),
                        )];
                        refresh_search_index(
                            &workspace_path,
                            &guidance_dir,
                            db_path,
                            &scoped,
                            verbose,
                        )
                        .await;
                    }
                    print_fix_handoff(&fix_escalated, &fix_drafts);
                    if !fix_escalated.is_empty() {
                        let stuck: Vec<String> =
                            fix_escalated.iter().map(|esc| esc.file.clone()).collect();
                        backdate_stuck(&guidance_dir, &src_dirs, &stuck);
                    }
                }
                Err(ResultPoolError::Inner(e)) => eprintln!("error generating {path}: {e}"),
                Err(ResultPoolError::Canceled) => {
                    eprintln!("error: pool response canceled for {path}")
                }
                Err(ResultPoolError::Pool(e)) => eprintln!("error: pool queue {e} for {path}"),
            }
        }
    } else if let Some(scan_dir) = scan {
        let scan_path = PathBuf::from(scan_dir);
        ensure_dir_or_panic(&guidance_dir);
        // The walk stays scoped to the scan path, but sidecars mirror
        // their source root: a subdirectory scan still maps against the
        // containing `src` dir, never against the scan path itself.
        let src_dirs = src_dirs_from_config_with(&workspace_path, &cfg);
        let map_dir = find_source_dir(&scan_path, &src_dirs).unwrap_or_else(|| scan_path.clone());
        let generated = walk_and_gen_async(
            guidance_dir.clone(),
            scan_path,
            map_dir,
            force,
            verbose,
            &[],
            None,
        )
        .await;
        if verbose {
            println!("Scanned {scan_dir}: generated {generated} files");
        }
    } else {
        let src_dirs = src_dirs_from_config_with(&workspace_path, &cfg);

        let mut total_files = 0usize;
        let mut stale_files = 0usize;
        let mut generated = 0usize;

        // Rotation sweep and rename adoption run before derivation so
        // rotated receipts and moved files re-enter the stale set.
        {
            let _phase = profile::Phase::new("sync.sweep_rotation");
            sweep_for_rotation(guidance_dir.as_path(), &src_dirs, verbose);
        }
        {
            let _phase = profile::Phase::new("sync.adopt_renames");
            adopt_renames(guidance_dir.as_path(), &src_dirs, verbose);
        }

        // Fix pre-pass: lint, fix, and format stale Rust files before
        // member gen and indexing, so the mirror and the index converge
        // on fixed bytes. L0 and dry runs skip the rewrite half.
        // Progress stays quiet unless verbose; only escalations speak,
        // with the complete error as handoff.
        let mut fix_escalated: Vec<guidance_core::waves::Escalation> = Vec::new();
        let mut fix_drafts = guidance_core::waves::ModelDraftReport::default();
        {
            let mut policy = cfg.policy.clone();
            policy.autonomy = autonomy.to_string();
            if !dry_run && guidance_core::policy::autonomy_allows_fix(&policy) {
                let mut dirty: Vec<PathBuf> = Vec::new();
                for src_dir in &src_dirs {
                    if !src_dir.is_dir() {
                        continue;
                    }
                    let engine = SyncEngine::new(guidance_dir.clone(), src_dir.clone());
                    for path in engine.stale_files() {
                        if path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                            && path.is_file()
                        {
                            dirty.push(path);
                        }
                    }
                }
                dirty.sort();
                dirty.dedup();
                if dirty.len() > SYNC_FIX_MAX_FILES {
                    if verbose {
                        println!(
                            "fix: skipped ({} dirty files exceed the {}-file pre-pass budget; run 'guidance run' to converge)",
                            dirty.len(),
                            SYNC_FIX_MAX_FILES
                        );
                    }
                } else if !dirty.is_empty() {
                    let _phase = profile::Phase::new("sync.fix");
                    let fix = run_fix_scope(
                        &dirty,
                        &guidance_dir,
                        &cfg,
                        &policy,
                        SYNC_FIX_MAX_WAVES,
                        verbose,
                    );
                    if verbose {
                        println!(
                            "fix: {} fixed ({} formatted), {} escalated, drafts kept {}",
                            fix.waves.fixed,
                            fix.waves.formatted,
                            fix.waves.escalated.len(),
                            fix.drafts.kept
                        );
                    }
                    fix_escalated = fix.waves.escalated;
                    fix_drafts = fix.drafts;
                }
            }
        }

        for src_dir in &src_dirs {
            if !src_dir.is_dir() {
                continue;
            }
            ensure_dir_or_panic(&guidance_dir);
            let engine = SyncEngine::new(guidance_dir.clone(), src_dir.clone());
            let status = {
                let _phase = profile::Phase::new("sync.status");
                engine.status().expect("status")
            };
            total_files += status.total_files;
            stale_files += status.stale_files;

            if stale_files > 0 || force {
                let _phase = profile::Phase::new("sync.walk_and_gen");
                generated += walk_and_gen_async(
                    guidance_dir.clone(),
                    src_dir.clone(),
                    src_dir.clone(),
                    force,
                    verbose,
                    &[],
                    Some(db_path),
                )
                .await;
            }
        }

        if verbose {
            println!("Syncing {total_files} total files ({stale_files} stale)...");
        }

        if !no_db && !dry_run {
            generated +=
                refresh_search_index(&workspace_path, &guidance_dir, db_path, &src_dirs, verbose)
                    .await;
        }
        // Doc→code jump funnel: derived after the index rebuilds, gated
        // on manifest staleness so converged trees pay one stat sweep
        // and no rewrite. Respects dry runs (no writes, no report).
        if !dry_run {
            match guidance_core::jumps::sync_jumps(&workspace_path, &guidance_dir) {
                Ok(report) => {
                    if verbose || report.rederived {
                        println!(
                            "Jumps: {} chunks ({})",
                            report.chunks,
                            if report.rederived {
                                "rederived"
                            } else {
                                "fresh"
                            }
                        );
                    }
                }
                Err(error) => {
                    eprintln!("Warning: jumps not synced: {error}");
                }
            }
        }
        if summarize && !dry_run {
            // Opt-in summary pass after the index rebuilds: node sync is
            // done, so summary-body writes cannot move its fingerprint,
            // and the pass refreshes the generation marker itself.
            let src_roots: Vec<PathBuf> = src_dirs.clone();
            match summarize_cmd::run_summarize(&guidance_dir, db_path, &src_roots, &cfg, verbose)
                .await
            {
                Ok(report) => {
                    if verbose {
                        println!(
                            "Summarized {} nodes ({} cached) across {} files: {} failed, {} unavailable, {} gaps.",
                            report.summarized,
                            report.cached,
                            report.files,
                            report.failed,
                            report.unavailable,
                            report.gaps,
                        );
                    }
                    generated += report.summarized;
                }
                Err(error) => {
                    eprintln!("warning: summary pass skipped: {error}");
                }
            }
        }
        // Escalation handoff (always printed): files the fix loop and
        // model drafts could not converge, with the complete error.
        // Stuck sidecars keep a truthful stale verdict so the next sync
        // re-surfaces the same handoff until a human or agent fixes them.
        print_fix_handoff(&fix_escalated, &fix_drafts);
        if !fix_escalated.is_empty() {
            let stuck: Vec<String> = fix_escalated.iter().map(|esc| esc.file.clone()).collect();
            backdate_stuck(&guidance_dir, &src_dirs, &stuck);
        }
        if verbose {
            if generated > 0 {
                println!("Generated {generated} files.");
            }
            println!("Sync complete.");
        }
    }

    if watch {
        let src_dirs = src_dirs_from_config(&workspace_path);
        start_watcher(
            guidance_dir,
            &src_dirs,
            workspace_path,
            force,
            verbose,
            watch_debounce_ms,
        )
        .await;
    }
}

/// Fragment ingest plus dependent propagation plus node sync: the
/// derived-index half of a sync, shared by `sync` and `run` (one copy —
/// the two commands differ in whether they fix, never in how they
/// reindex). Returns regen'd files for the caller's report.
async fn refresh_search_index(
    workspace_path: &Path,
    guidance_dir: &Path,
    db_path: &str,
    src_dirs: &[PathBuf],
    verbose: bool,
) -> usize {
    let _total = profile::Phase::new("index.total");
    let mut generated = 0usize;
    // Generation gate: a rotated schema refuses the index read and
    // rebuilds fully (stored rows may misread); content drift alone
    // rides the normal incremental ingest below, which re-derives
    // exactly the changed rows from sidecars.
    let index_dir = Path::new(db_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut full_rebuild = false;
    {
        let _phase = profile::Phase::new("index.generation_guard");
        let schema_now = guidance_core::index_generation::schema_generation();
        match guidance_core::index_generation::stored_schema(&index_dir) {
            // No marker: first build (or a crash mid-write) rebuilds.
            None => {
                if verbose {
                    eprintln!("index generation unknown; rebuilding from sidecars");
                }
                let _ = std::fs::remove_file(db_path);
                full_rebuild = true;
            }
            Some(stored) if stored != schema_now => {
                if verbose {
                    eprintln!("index schema rotated; rebuilding from sidecars");
                }
                let _ = std::fs::remove_file(db_path);
                full_rebuild = true;
            }
            Some(_) => {}
        }
    }
    // Fragment ingestion first: its change gate reports the
    // fingerprint-changed set (plus files missing on disk) that
    // seeds dependent propagation below.
    // Roots may be files (the `sync --file` scope) or directories;
    // missing roots stay skipped.
    let existing_dirs: Vec<PathBuf> = src_dirs
        .iter()
        .filter(|dir| dir.exists())
        .cloned()
        .collect();
    let ingest_phase = profile::Phase::new("index.ingest");
    let frag_stats = match index_cmd::ingest_workspace_fragments(
        Path::new(db_path),
        workspace_path,
        &existing_dirs,
    ) {
        Ok(stats) => {
            if verbose {
                println!(
                    "Ingested {} files ({} fragments, {} unchanged skipped, {} images skipped, {} failed) to {db_path}",
                    stats.files, stats.fragments, stats.skipped_unchanged, stats.skipped_images, stats.failed
                );
            }
            stats
        }
        Err(e) => {
            eprintln!("Warning: fragment ingestion failed: {e}");
            index_cmd::FragmentIngestStats::default()
        }
    };
    drop(ingest_phase);
    let _propagation = profile::Phase::new("index.propagation");
    // Regen roots for sidecar mapping: ingest roots may be file-scoped
    // (`sync --file`), but sidecars mirror source roots, so file entries
    // fall back to the configured roots for the regen below.
    let regen_roots: Vec<PathBuf> = if existing_dirs.iter().all(|dir| dir.is_dir()) {
        existing_dirs.clone()
    } else {
        src_dirs_from_config(workspace_path)
    };
    // Propagation: seeds (changed + deleted + pre-resolved
    // deleted-dependents) expand through the hydrated dependents
    // closure; affected files re-process via
    // the existing single-file regen path (forced, so
    // dependency-derived state reconverges even when the
    // dependent's own bytes are unchanged). Deleted files drop
    // their JSON sidecars with the existing deleter.
    // A rebuild's changed set is every file; sidecars remain the source of
    // truth, so dependency-driven regeneration would rewrite the mirror.
    if !full_rebuild
        && (!frag_stats.changed.is_empty()
            || !frag_stats.deleted.is_empty()
            || !frag_stats.deleted_dependents.is_empty())
    {
        let roots: Vec<String> = regen_roots
            .iter()
            .map(|dir| dir.to_string_lossy().into_owned())
            .collect();
        // Dependents resolved pre-purge (their graph rows are gone
        // by now) re-enter as seeds so the post-purge closure can
        // still expand transitively through surviving nodes. Seeds are
        // sig-gated: only interface-moved files expand to dependents
        // (unknown interfaces fail open); macro-defining changes widen
        // to their package since edges hide macro expansions.
        let mut seeds = frag_stats.sig_moved.clone();
        seeds.extend(frag_stats.deleted_dependents.iter().cloned());
        profile::count("index.changed_files", frag_stats.changed.len());
        profile::count("index.deleted_files", frag_stats.deleted.len());
        profile::count(
            "index.deleted_dependents",
            frag_stats.deleted_dependents.len(),
        );
        let affected = match GuidanceDb::open(Path::new(db_path)) {
            Ok(db) => {
                let mut affected =
                    index_cmd::affected_for_sync(&db, &seeds, &frag_stats.deleted, &roots);
                if !frag_stats.macro_moved.is_empty() {
                    let known: Vec<(String, String)> = db
                        .list_files()
                        .map(|files| {
                            files
                                .into_iter()
                                .map(|file| (file.absolute_path, file.root_path))
                                .collect()
                        })
                        .unwrap_or_default();
                    affected.extend(guidance_core::invalidation::macro_expansion(
                        &known,
                        &frag_stats.macro_moved,
                    ));
                    affected.sort();
                    affected.dedup();
                }
                affected
            }
            Err(error) => {
                eprintln!("Warning: graph hydration failed ({error}); re-processing seeds only");
                let mut seeds = seeds;
                seeds.extend(frag_stats.deleted.iter().cloned());
                seeds.sort();
                seeds.dedup();
                seeds
            }
        };
        profile::count("index.affected_files", affected.len());
        if verbose && profile::enabled() {
            for path in frag_stats.changed.iter().take(5) {
                eprintln!("[profile] changed: {path}");
            }
        }
        if !affected.is_empty() {
            let pool = runtime::ast_pool();
            // Shared backpressure with the walk above: regen waves stay
            // bounded however large the affected set is.
            let semaphore = Arc::new(tokio::sync::Semaphore::new(AST_SUBMIT_IN_FLIGHT));
            let source_dirs = Arc::new(regen_roots.clone());
            let workspace = workspace_path.to_path_buf();
            let guidance = guidance_dir.to_path_buf();
            // Bound the wave: the pool has finite queue slots, and
            // sequential submits leave its workers idle on a hub change.
            // Regen failures aggregate below: per-file lines are verbose
            // detail, the count is the contract.
            let mut regen_failed = 0usize;
            for chunk in affected.chunks(PROPAGATION_CONCURRENCY) {
                let mut handles = Vec::new();
                for path in chunk {
                    let candidate = PathBuf::from(path);
                    if !candidate.is_file() {
                        continue;
                    }
                    let ext = candidate.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if !guidance_core::sync_engine::MEMBER_SOURCE_EXTENSIONS.contains(&ext) {
                        continue;
                    }
                    let pool = Arc::clone(&pool);
                    let semaphore = Arc::clone(&semaphore);
                    let source_dirs = Arc::clone(&source_dirs);
                    let workspace = workspace.clone();
                    let guidance = guidance.clone();
                    handles.push(tokio::spawn(async move {
                        regen_single_file(
                            &pool,
                            &semaphore,
                            candidate,
                            &source_dirs,
                            &workspace,
                            &guidance,
                            verbose,
                        )
                        .await
                    }));
                    generated += 1;
                }
                for handle in handles {
                    if !handle.await.unwrap_or(false) {
                        regen_failed += 1;
                    }
                }
            }
            if regen_failed > 0 {
                eprintln!("Warning: {regen_failed} dependents failed to regenerate");
            }
        }
        for prefix in &frag_stats.deleted {
            remove_sidecars(guidance_dir, &existing_dirs, Path::new(prefix));
        }
    }
    drop(_propagation);
    let json_src = guidance_dir.join("src");
    // Node counts now derive from the fragment index (see
    // `handle_guidance_status`); the legacy member-node table is no
    // longer rewritten on every sync — `sync_from_dir` remains for
    // explicit use, but the live path leaves it alone.
    // Index-final stamp: the generation marker commits only after the
    // derived index rebuilt, so the next read never serves a mixed pair.
    {
        let _phase = profile::Phase::new("index.generation_write");
        let generation = guidance_core::index_generation::generation_of(&json_src);
        if let Err(error) =
            guidance_core::index_generation::write_generation(&index_dir, &generation)
        {
            eprintln!("warning: generation marker not written: {error}");
        }
    }
    generated
}

fn src_dirs_from_config_with(workspace_path: &Path, cfg: &config::ProjectConfig) -> Vec<PathBuf> {
    if cfg.src_dirs.is_empty() {
        vec![workspace_path.to_path_buf()]
    } else {
        cfg.src_dirs
            .iter()
            .map(|d| workspace_path.join(d))
            .collect()
    }
}

/// Component-rotation sweep: one header-only pass per source root
/// backdates exactly the receipts that consumed a rotated component.
/// Runs before dirty-set derivation so rotated files re-enter the set.
fn sweep_for_rotation(guidance_dir: &Path, src_dirs: &[PathBuf], verbose: bool) -> usize {
    use guidance_core::components::ComponentVector;
    use guidance_core::policy::MaintenancePolicy;
    let current = ComponentVector::current(&MaintenancePolicy::default());
    let sidecar_dir = guidance_dir.join("src");
    let mut backdated = 0usize;
    for src_dir in src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        for outcome in guidance_core::components::sweep_sidecars(&sidecar_dir, src_dir, &current) {
            if matches!(
                outcome.decision,
                guidance_core::components::SweepDecision::Backdated
            ) {
                backdated += 1;
            }
        }
    }
    if backdated > 0 && verbose {
        println!("swept components: {backdated} receipts backdated");
    }
    backdated
}

/// Rename adoption: sources without a sidecar whose bytes match exactly
/// one orphaned sidecar inherit it instead of reprocessing. Runs before
/// generation so renames never count as new work.
fn adopt_renames(guidance_dir: &Path, src_dirs: &[PathBuf], verbose: bool) -> usize {
    use guidance_core::freshness::adopt_renamed_sidecar;
    let sidecar_dir = guidance_dir.join("src");
    let mut adopted = 0usize;
    for src_dir in src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        let engine = SyncEngine::new(guidance_dir.to_path_buf(), src_dir.clone());
        let mut sources = Vec::new();
        walk::walk_files(
            src_dir,
            guidance_core::sync_engine::MEMBER_SOURCE_EXTENSIONS,
            |path| {
                sources.push(path.to_path_buf());
            },
        );
        sources.sort();
        for source in &sources {
            // Test files never have sidecars; probing them made adoption
            // hash every test on every sync (the 9 s warm-sync cost).
            if guidance_core::offload::is_test_path(source) {
                continue;
            }
            if engine.sidecar_path(source).exists() {
                continue;
            }
            let Some(hash) = guidance_core::diff::hash_file(source) else {
                continue;
            };
            if adopt_renamed_sidecar(&sidecar_dir, src_dir, source, &hash).is_some() {
                adopted += 1;
            }
        }
    }
    if adopted > 0 && verbose {
        println!("adopted {adopted} renamed sidecars");
    }
    adopted
}

/// Stuck files keep a truthful stale verdict: mark escalated sidecars
/// stuck and backdate them below their sources while the snapshot
/// explains why. The flag carries the verdict past the content-hash
/// shortcut; the backdated mtime carries it past the clock check.
fn backdate_stuck(guidance_dir: &Path, src_dirs: &[PathBuf], files: &[String]) -> usize {
    let mut backdated = 0usize;
    for file in files {
        let source = PathBuf::from(file);
        // Absolutize for the match: recorded paths may be relative
        // while the walk that produced them absolutizes elsewhere.
        let abs = guidance_core::sync_engine::absolute_or(source.clone());
        let Some(src_dir) = src_dirs.iter().find(|root| {
            abs.starts_with(&guidance_core::sync_engine::absolute_or((*root).clone()))
        }) else {
            continue;
        };
        let engine = SyncEngine::new(guidance_dir.to_path_buf(), src_dir.clone());
        let sidecar = engine.sidecar_path(&source);
        if !sidecar.is_file() {
            continue;
        }
        guidance_core::freshness::mark_stuck_sidecar(&sidecar);
        if guidance_core::freshness::backdate_sidecar(&sidecar, &source) {
            backdated += 1;
        }
    }
    backdated
}

fn src_dirs_from_config(workspace_path: &Path) -> Vec<PathBuf> {
    let cfg = load_project_config(workspace_path);
    src_dirs_from_config_with(workspace_path, &cfg)
}

async fn walk_and_gen_async(
    guidance_dir: PathBuf,
    walk_dir: PathBuf,
    map_dir: PathBuf,
    force: bool,
    verbose: bool,
    filter_exts: &[&str],
    db_path: Option<&str>,
) -> usize {
    let exts: Vec<&str> = if filter_exts.is_empty() {
        guidance_core::sync_engine::MEMBER_SOURCE_EXTENSIONS.to_vec()
    } else {
        filter_exts.to_vec()
    };
    let mut files = Vec::new();
    let walk_phase = profile::Phase::new("sync.walk_and_gen.walk");
    walk::walk_files(&walk_dir, &exts, |p| files.push(p.to_path_buf()));
    drop(walk_phase);
    if files.is_empty() {
        return 0;
    }
    // Stored source hashes for the member-JSON clock gate (one small
    // listing; empty on any failure — the gate then keeps mtime behavior).
    let hash_phase = profile::Phase::new("sync.walk_and_gen.source_hashes");
    let source_hashes: std::collections::HashMap<String, String> = db_path
        .and_then(|path| search_vector::GuidanceDb::open(Path::new(path)).ok())
        .and_then(|db| db.source_content_hashes().ok())
        .unwrap_or_default();
    drop(hash_phase);

    // AST pool built lazily on the first stale file: no-work walks
    // (warm syncs, quiet watch cycles) build zero workers instead of
    // parking a full complement for nothing. Owned per call as before;
    // no statics, no lifecycle change when work exists.
    let mut pool: Option<
        Arc<
            fluent_concurrency::pool::ResultPool<
                runtime::AstGenPayload,
                fluent_types::GuidanceDoc,
                guidance_core::sync_engine::SyncEngineError,
            >,
        >,
    > = None;
    // Backpressure shared by every task below: at most a handful of pool
    // submissions are in flight however many files the walk found.
    let semaphore = Arc::new(tokio::sync::Semaphore::new(AST_SUBMIT_IN_FLIGHT));
    let mut handles = Vec::with_capacity(files.len());
    let mut generated = 0usize;

    let staleness_phase = profile::Phase::new("sync.walk_and_gen.staleness");
    for path in &files {
        // Test files have no sidecars by policy; without this filter the
        // walker queues every test on every sync and the pool rejects each.
        if guidance_core::offload::is_test_path(path) {
            continue;
        }
        let rel = path.strip_prefix(&map_dir).unwrap_or(path);
        // Single shared mapping with the pipeline: staleness reads the
        // same `guidance_dir/src/<rel>.json` the gen step writes.
        let json_path =
            guidance_core::sync_engine::guidance_json_path(path, &map_dir, &guidance_dir);
        let should_gen = force
            || match source_hashes
                .get(path.to_string_lossy().as_ref())
                .map(String::as_str)
            {
                Some(stored) => guidance_core::sync::staleness::should_generate_with_stored(
                    &json_path,
                    path,
                    Some(stored),
                    || guidance_core::diff::hash_file(path),
                ),
                None => guidance_core::sync::staleness::should_generate(&json_path, path),
            };
        if !should_gen {
            if verbose {
                println!("  skip: {}", rel.display());
            }
            continue;
        }

        let pool = Arc::clone(pool.get_or_insert_with(runtime::ast_pool));
        let source_path = path.clone();
        let src_dir = map_dir.clone();
        let gd = guidance_dir.clone();
        let rel_str = rel.display().to_string();
        let semaphore = Arc::clone(&semaphore);
        handles.push(tokio::spawn(async move {
            let result = submit_ast(
                &pool,
                &semaphore,
                runtime::AstGenPayload {
                    source_path,
                    source_dir: src_dir,
                    guidance_dir: gd,
                    config: guidance_core::sync_engine::GenConfig::default(),
                },
            )
            .await;
            (rel_str, result)
        }));
    }

    drop(staleness_phase);
    let gen_phase = profile::Phase::new("sync.walk_and_gen.generate");
    let mut gen_failed = 0usize;
    for handle in handles {
        let (rel_path, result) = handle.await.unwrap();
        match result {
            Ok(doc) => {
                generated += 1;
                if verbose {
                    println!("  gen: {rel_path} ({} members)", doc.members.len());
                }
            }
            Err(ResultPoolError::Inner(e)) => {
                gen_failed += 1;
                if verbose {
                    eprintln!("  warn: {rel_path} — {e}");
                }
            }
            Err(ResultPoolError::Canceled) => {
                gen_failed += 1;
                if verbose {
                    eprintln!("  warn: {rel_path} — pool response canceled");
                }
            }
            Err(ResultPoolError::Pool(e)) => {
                gen_failed += 1;
                if verbose {
                    eprintln!("  warn: {rel_path} — pool queue {e}");
                }
            }
        }
    }

    drop(gen_phase);
    if gen_failed > 0 {
        eprintln!("Warning: {gen_failed} files failed to generate");
    }

    generated
}

/// Coordinator revision authority for `--watch` (binside `RootRuntime`).
struct WatchRuntime {
    root: String,
    dirty: std::sync::atomic::AtomicU64,
    indexed: std::sync::atomic::AtomicU64,
    verbose: bool,
}

impl WatchRuntime {
    fn new(root: String, verbose: bool) -> Self {
        Self {
            root,
            dirty: std::sync::atomic::AtomicU64::new(0),
            indexed: std::sync::atomic::AtomicU64::new(0),
            verbose,
        }
    }
}

impl guidance_core::coordinator::RootRuntime for WatchRuntime {
    fn canonical_root(&self) -> String {
        self.root.clone()
    }
    fn mark_dirty(&self) -> u64 {
        self.dirty.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
    }
    fn mark_indexed(&self, revision: u64) {
        self.indexed
            .store(revision, std::sync::atomic::Ordering::SeqCst);
        if self.verbose {
            println!("  indexed revision {revision}");
        }
    }
    fn mark_reconciled(&self, revision: u64, epoch: u64) {
        self.indexed
            .store(revision, std::sync::atomic::Ordering::SeqCst);
        println!("Reconciled revision {revision} at epoch {epoch}");
    }
    fn require_full_reconciliation(&self) {
        if self.verbose {
            println!("  full reconciliation required");
        }
    }
}

/// Harvest code files under `src_dirs` for graph-assisted invalidation
/// (single linear read+parse pass, no embeddings).
fn harvest_watch_inputs(src_dirs: &[PathBuf]) -> Vec<guidance_core::graph_index::HarvestInput> {
    use guidance_core::extractor::adapter::format_for_extension;
    use guidance_core::graph_index::HarvestInput;
    let mut inputs = Vec::new();
    for dir in src_dirs {
        let mut files = Vec::new();
        walk::walk_files(
            dir,
            guidance_core::sync_engine::MEMBER_SOURCE_EXTENSIONS,
            |p| {
                files.push(p.to_path_buf());
            },
        );
        for path in files {
            // Test code never enters the graph: it is neither indexed nor
            // described, so it must not drive invalidation either.
            if guidance_core::offload::is_test_path(&path) {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let Some(format) = format_for_extension(ext) else {
                continue;
            };
            if let Ok(text) = std::fs::read_to_string(&path) {
                inputs.push(HarvestInput {
                    path: path.to_string_lossy().into_owned(),
                    format: format.to_string(),
                    text,
                });
            }
        }
    }
    inputs
}

/// Regenerate one source file through the AST pool (per-file isolation:
/// one failure never aborts the batch).
async fn regen_single_file(
    pool: &Arc<
        fluent_concurrency::pool::ResultPool<
            runtime::AstGenPayload,
            fluent_types::GuidanceDoc,
            guidance_core::sync_engine::SyncEngineError,
        >,
    >,
    semaphore: &Arc<tokio::sync::Semaphore>,
    source_path: PathBuf,
    src_dirs: &[PathBuf],
    workspace_path: &Path,
    guidance_dir: &Path,
    verbose: bool,
) -> bool {
    let source_dir =
        find_source_dir(&source_path, src_dirs).unwrap_or_else(|| workspace_path.to_path_buf());
    match submit_ast(
        pool,
        semaphore,
        runtime::AstGenPayload {
            source_path: source_path.clone(),
            source_dir,
            guidance_dir: guidance_dir.to_path_buf(),
            config: guidance_core::sync_engine::GenConfig::default(),
        },
    )
    .await
    {
        Ok(doc) => {
            if verbose {
                println!(
                    "  regenerated: {} ({} members)",
                    source_path.display(),
                    doc.members.len()
                );
            }
            true
        }
        Err(ResultPoolError::Inner(e)) => {
            if verbose {
                eprintln!("  regeneration failed: {e}");
            }
            false
        }
        Err(ResultPoolError::Canceled) => {
            if verbose {
                eprintln!("  regeneration canceled");
            }
            false
        }
        Err(ResultPoolError::Pool(e)) => {
            if verbose {
                eprintln!("  regeneration queue error: {e}");
            }
            false
        }
    }
}

async fn start_watcher(
    guidance_dir: PathBuf,
    src_dirs: &[PathBuf],
    workspace_path: PathBuf,
    _force: bool,
    verbose: bool,
    debounce_ms: u64,
) {
    use fluent_concurrency::stream::StreamAbort;
    use guidance_core::coordinator::{
        EnqueueReason, IndexCoordinator, JobProgress, ProgressSink, ReconciliationProof,
    };
    use guidance_core::scheduler::{BoxFuture, JobError, JobScheduler};
    use guidance_core::watcher::{
        NotifyBackend, WatchManager, WatchOptions, WatchPlatform, WatchReason,
    };

    if src_dirs.is_empty() {
        return;
    }

    // R.6: the old bespoke `--watch` loop is gone. Watching now runs on
    // the P3 `WatchManager` (debounce + storm compaction + error resume)
    // feeding the `IndexCoordinator` (revision ordering + followup
    // coalescing); `--watch-debounce-ms` still drives the debounce delay.
    let scheduler = JobScheduler::new(1, 3, 500);
    let runtime = Arc::new(WatchRuntime::new(
        workspace_path.to_string_lossy().into_owned(),
        verbose,
    ));
    let ast_pool = runtime::ast_pool();
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let src_dirs_owned: Vec<PathBuf> = src_dirs.to_vec();

    let run = {
        let guidance_dir = guidance_dir.clone();
        let workspace_path = workspace_path.clone();
        let src_dirs_owned = src_dirs_owned.clone();
        let ast_pool = Arc::clone(&ast_pool);
        let epoch = Arc::clone(&epoch);
        Arc::new(
            move |snapshot: guidance_core::change_set::ChangeSetSnapshot,
                  report: ProgressSink,
                  _abort: StreamAbort|
                  -> BoxFuture<Result<Option<ReconciliationProof>, JobError>> {
                let guidance_dir = guidance_dir.clone();
                let workspace_path = workspace_path.clone();
                let src_dirs_owned = src_dirs_owned.clone();
                let ast_pool = Arc::clone(&ast_pool);
                let epoch = Arc::clone(&epoch);
                Box::pin(async move {
                    if snapshot.force_full_reconcile {
                        let mut generated = 0usize;
                        for src_dir in &src_dirs_owned {
                            if src_dir.is_dir() {
                                generated += walk_and_gen_async(
                                    guidance_dir.clone(),
                                    src_dir.clone(),
                                    src_dir.clone(),
                                    false,
                                    verbose,
                                    &[],
                                    None,
                                )
                                .await;
                            }
                        }
                        report(JobProgress {
                            done: generated,
                            total: generated,
                            message: "full reconcile".to_string(),
                        });
                        let next = epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                        return Ok(Some(ReconciliationProof {
                            reconciled: true,
                            reconciliation_epoch: next,
                        }));
                    }
                    // Incremental: expand watcher paths through the
                    // dependents closure (unit of staleness = affected
                    // subgraph, not the workspace), then regen.
                    let graph = guidance_core::graph_index::GraphIndex::build(
                        &harvest_watch_inputs(&src_dirs_owned),
                        &src_dirs_owned
                            .iter()
                            .map(|dir| dir.to_string_lossy().into_owned())
                            .collect::<Vec<_>>(),
                    )
                    .map_err(|e| JobError::terminal(e.to_string()))?;
                    let mut seeds: Vec<String> = snapshot.touched_files.clone();
                    for dir in &snapshot.rescan_directories {
                        let mut files = Vec::new();
                        walk::walk_files(
                            &PathBuf::from(dir),
                            guidance_core::sync_engine::MEMBER_SOURCE_EXTENSIONS,
                            |p| files.push(p.to_path_buf()),
                        );
                        seeds.extend(files.iter().map(|p| p.to_string_lossy().into_owned()));
                    }
                    let affected = graph.dependents_closure(&seeds);
                    let mut done = 0usize;
                    // Sequential awaits never pressure the pool, but the
                    // regen helper takes a shared semaphore: one permit
                    // total here is exact, not a bound.
                    let semaphore = Arc::new(tokio::sync::Semaphore::new(AST_SUBMIT_IN_FLIGHT));
                    for path in &affected {
                        let path = PathBuf::from(path);
                        if !path.is_file() {
                            continue;
                        }
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                        if !guidance_core::sync_engine::MEMBER_SOURCE_EXTENSIONS.contains(&ext) {
                            continue;
                        }
                        regen_single_file(
                            &ast_pool,
                            &semaphore,
                            path,
                            &src_dirs_owned,
                            &workspace_path,
                            &guidance_dir,
                            verbose,
                        )
                        .await;
                        done += 1;
                        report(JobProgress {
                            done,
                            total: affected.len(),
                            message: "incremental reconcile".to_string(),
                        });
                    }
                    // Deleted prefixes: drop their JSON sidecars (best effort).
                    for prefix in &snapshot.deleted_prefixes {
                        remove_sidecars(&guidance_dir, &src_dirs_owned, &PathBuf::from(prefix));
                    }
                    Ok(None)
                })
            },
        )
    };

    let coordinator = Arc::new(IndexCoordinator::new(runtime, scheduler, run));
    if verbose {
        let progress = verbose;
        coordinator.on_progress(Arc::new(move |progress_update| {
            if progress {
                println!(
                    "  progress: {}/{} {}",
                    progress_update.done, progress_update.total, progress_update.message
                );
            }
        }));
    }
    let manager = WatchManager::new(WatchOptions {
        root: workspace_path.clone(),
        platform: WatchPlatform::Current,
        debounce_ms,
        max_wait_ms: guidance_core::search_constants::WATCH_MAX_WAIT_MS,
        reconcile_interval_ms: guidance_core::search_constants::WATCH_RECONCILE_INTERVAL_MS,
        resume_check_interval_ms: guidance_core::search_constants::WATCH_RESUME_CHECK_INTERVAL_MS,
        resume_threshold_ms: guidance_core::search_constants::WATCH_RESUME_THRESHOLD_MS,
        max_changed_paths: guidance_core::search_constants::CHANGE_SET_PATH_BUDGET,
        backend: Arc::new(NotifyBackend::new()),
        root_paths: Vec::new(),
        on_changes: {
            let coordinator = Arc::clone(&coordinator);
            Arc::new(move |snapshot, reason| {
                coordinator.enqueue(
                    &snapshot,
                    match reason {
                        WatchReason::Watch => EnqueueReason::Watch,
                        WatchReason::Reconcile => EnqueueReason::Reconcile,
                    },
                );
                Box::pin(async {}) as BoxFuture<()>
            })
        },
        on_pending_change: None,
        on_activity: None,
    });
    manager.start();
    println!("Watching for changes (debounce: {debounce_ms}ms)...");

    tokio::signal::ctrl_c().await.ok();
    manager.close().await;
    coordinator.close();
}

/// Remove JSON sidecars for a deleted source prefix (best effort).
fn remove_sidecars(guidance_dir: &Path, src_dirs: &[PathBuf], prefix: &Path) {
    let Some(source_dir) = find_source_dir(prefix, src_dirs) else {
        return;
    };
    let Ok(relative) = prefix.strip_prefix(&source_dir) else {
        return;
    };
    let sidecar_base = guidance_dir.join("src").join(relative);
    // Creation-side naming appends `.json` (`importee.rs.json`), so the
    // existence probe must use that same shape: `with_extension` would
    // probe `importee.json`, which never exists, and a deleted file's
    // sidecar would silently survive.
    let sidecar = if relative.extension().is_some() {
        PathBuf::from(format!("{}.json", sidecar_base.display()))
    } else {
        sidecar_base.clone()
    };
    if prefix.is_file() || !prefix.exists() && sidecar.is_file() {
        let _ = std::fs::remove_file(&sidecar);
        return;
    }
    // Directory prefix: drop sidecars beneath it.
    let mut stale = Vec::new();
    walk::walk_files(&sidecar_base, &["json"], |p| stale.push(p.to_path_buf()));
    for sidecar in stale {
        let _ = std::fs::remove_file(&sidecar);
    }
}

fn find_source_dir(path: &Path, src_dirs: &[PathBuf]) -> Option<PathBuf> {
    // Sidecar mapping roots are directories: absolutize both sides so a
    // relative workspace (`-w .`) still prefixes absolute walks. A file
    // entry never matches — it scopes an ingest, not a source tree.
    let abs = guidance_core::sync_engine::absolute_or(path.to_path_buf());
    for dir in src_dirs {
        if !dir.is_dir() {
            continue;
        }
        let root = guidance_core::sync_engine::absolute_or(dir.clone());
        if abs.starts_with(&root) {
            return Some(dir.clone());
        }
    }
    None
}

/// Seal the workspace for human review or CI: matrix clippy, full
/// build, full tests, and a witness pass. Reports and exits; never
/// fixes, never runs inside `run` or `watch`.
fn cmd_seal(workspace: &str) {
    use guidance_core::fidelity::CfgMatrix;
    use guidance_core::seal::{run_seal, SealCtx};
    use std::sync::Arc;
    let root = PathBuf::from(workspace);
    let clippy = guidance_core::fidelity::live_matrix_runner(
        None,
        guidance_core::clippy::shaping_for(false),
    );
    let build = Arc::new(|package: &Path| {
        let output = std::process::Command::new("cargo")
            .arg("build")
            .arg("--manifest-path")
            .arg(package.join("Cargo.toml"))
            .output()
            .map_err(std::io::Error::other)?;
        Ok::<_, std::io::Error>(guidance_core::seal::BuildVerdict {
            package: package.to_string_lossy().into_owned(),
            clean: output.status.success(),
            output: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    });
    let test = Arc::new(|package: &Path| {
        let output = std::process::Command::new("cargo")
            .arg("test")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(package.join("Cargo.toml"))
            .output()
            .map_err(std::io::Error::other)?;
        let mut new_failures = Vec::new();
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            for line in stdout
                .lines()
                .chain(String::from_utf8_lossy(&output.stderr).lines())
            {
                let line = line.trim_start_matches("test ").trim_start();
                if let Some((name, rest)) = line.split_once(' ') {
                    if rest.trim_start().starts_with("FAILED") {
                        new_failures.push(name.to_string());
                    }
                }
            }
            if new_failures.is_empty() {
                new_failures.push("cargo test failed".to_string());
            }
        }
        Ok::<_, std::io::Error>(guidance_core::gates::TestReport {
            new_failures,
            output: stdout.into_owned(),
        })
    });
    let report = run_seal(&SealCtx {
        workspace_root: root,
        cells: CfgMatrix::default_cells(),
        clippy,
        build,
        test,
        target_dir: None,
    });
    println!(
        "Seal: {} packages, {} cells",
        report.packages.len(),
        report.cells.len()
    );
    println!(
        "  build: {}",
        if report.build_ok { "clean" } else { "FAILED" }
    );
    println!(
        "  tests: {}",
        if report.tests_ok { "clean" } else { "FAILED" }
    );
    println!(
        "  witness: {}",
        if report.witness_clean {
            "clean"
        } else {
            "DIRTY"
        }
    );
    for detail in &report.details {
        println!("  - {detail}");
    }
    std::process::exit(report.exit.as_i32());
}

/// Report what a change touches: dependents, owners, narrowest gates,
/// edge tiers, and the tests exercising it.
fn cmd_impact(target: &str, workspace: &str) {
    use guidance_core::fidelity::safe_to_delete;
    use guidance_core::graph_index::GraphIndex;
    use guidance_core::query::context_pack::{impact_report, tests_touching, UNSEEN_CHANNELS};
    let root = PathBuf::from(workspace);
    let mut inputs = harvest_watch_inputs(std::slice::from_ref(&root));
    let mut test_files = Vec::new();
    walk::walk_files(&root, &["rs"], |path| {
        if guidance_core::offload::is_test_path(path) {
            if let Ok(text) = std::fs::read_to_string(path) {
                test_files.push((path.to_string_lossy().into_owned(), text));
            }
        }
    });
    // Test files join the impact graph read-only: their import and call
    // refs become tests-touching evidence, but nothing in the fix or
    // invalidation planes reads this graph. The watch harvest above
    // still excludes them, so test code never drives invalidation.
    for (path, text) in &test_files {
        inputs.push(guidance_core::graph_index::HarvestInput {
            path: path.clone(),
            format: "rust".to_string(),
            text: text.clone(),
        });
    }
    let roots = vec![root.to_string_lossy().into_owned()];
    let graph = GraphIndex::build(&inputs, &roots)
        .unwrap_or_else(|_| GraphIndex::build(&[], &roots).expect("empty graph builds"));
    let target_path = PathBuf::from(target);
    let changed: Vec<String> = if target_path.exists() {
        vec![target.to_string()]
    } else {
        let mut files: Vec<String> = graph
            .definitions_of(target)
            .iter()
            .map(|def| def.file.clone())
            .collect();
        files.sort();
        files.dedup();
        if files.is_empty() {
            eprintln!("No definitions found for {target} (none found in covered index)");
            std::process::exit(1);
        }
        files
    };
    // Index first: committed edges over token guesses. The mention
    // heuristic only fills an empty index result, labelled as such.
    let report = impact_report(&graph, &changed);
    let mut tests = report.tests.clone();
    let mut heuristic = false;
    if tests.is_empty() {
        tests = tests_touching(&test_files, &[target.to_string()]);
        heuristic = !tests.is_empty();
    }
    println!("Impact for {target}:");
    for dependent in &report.dependents {
        println!("  dependent: {dependent}");
    }
    for (owner, gate) in &report.owners {
        println!("  owner: {owner} (gate: {})", gate.gate);
    }
    for test in &tests {
        if heuristic {
            println!("  test (heuristic, unverified): {test}");
        } else {
            println!("  test: {test}");
        }
    }
    println!(
        "  tiers: {}",
        if report.tiers.is_empty() {
            "(no edges)".to_string()
        } else {
            report.tiers.join(", ")
        }
    );
    // Conservative deletion verdict: any dependent or exercising test
    // is proof of use (`no`); otherwise unknown with the unseen channels
    // named — the index can never prove a bare yes. Heuristic (token)
    // test guesses never count as proof, only committed edges do.
    let mut usages: Vec<String> = report
        .dependents
        .iter()
        .filter(|dependent| !changed.contains(dependent))
        .cloned()
        .collect();
    if !heuristic {
        for test in &tests {
            if !usages.contains(test) {
                usages.push(test.clone());
            }
        }
    }
    let verdict = match safe_to_delete(false, &usages, false) {
        guidance_core::fidelity::DeleteVerdict::No => "no (has dependents)",
        guidance_core::fidelity::DeleteVerdict::UnreferencedWithCaveats => {
            "unreferenced-with-caveats (private, dead-code clean)"
        }
        guidance_core::fidelity::DeleteVerdict::Unknown => "unknown",
    };
    println!("  deletion: {verdict}");
    println!("  unseen: {}", UNSEEN_CHANNELS.join(", "));
}

/// Narrow inbound surface: validated queue posts. The queue snapshot
/// loads from `queue_path` (empty when absent), validates, enqueues as
/// queued work only, and saves back. Prints the snapshot as JSON.
fn cmd_queue_post(id: &str, scope: &str, kind: &str, deps: &str, queue_path: &str) {
    use guidance_core::orchestrator::{post_work, PostWork};
    let mut queue = load_queue_snapshot(queue_path);
    let req = PostWork {
        id: id.to_string(),
        scope: scope.to_string(),
        kind: kind.to_string(),
        deps: deps
            .split(',')
            .map(str::trim)
            .filter(|dep| !dep.is_empty())
            .map(str::to_string)
            .collect(),
    };
    if let Err(error) = post_work(&mut queue, &req) {
        eprintln!("refused: {error}");
        std::process::exit(2);
    }
    save_queue_snapshot(queue_path, &queue);
    println!(
        "{}",
        serde_json::to_string_pretty(&queue.snapshot()).unwrap_or_default()
    );
}

/// Load a queue snapshot file; absent or unreadable means an empty queue.
fn load_queue_snapshot(path: &str) -> guidance_core::maintenance::MaintenanceQueue {
    use guidance_core::maintenance::MaintenanceQueue;
    let Ok(raw) = std::fs::read_to_string(path) else {
        return MaintenanceQueue::new();
    };
    let Ok(items) = serde_json::from_str::<Vec<guidance_core::maintenance::WorkItem>>(&raw) else {
        return MaintenanceQueue::new();
    };
    let mut queue = MaintenanceQueue::new();
    if queue.restore(&items).is_err() {
        return MaintenanceQueue::new();
    }
    queue
}

/// Save a queue snapshot file (atomic write).
fn save_queue_snapshot(path: &str, queue: &guidance_core::maintenance::MaintenanceQueue) {
    let raw = serde_json::to_string_pretty(&queue.snapshot()).unwrap_or_default();
    if common_core::io::write_atomic(Path::new(path), raw.as_bytes()).is_err() {
        eprintln!("could not save {path}");
        std::process::exit(3);
    }
}

/// Load an escalation ledger file; absent means empty.
fn load_ledger(path: &str) -> guidance_core::orchestrator::EscalationLedger {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Save an escalation ledger file (atomic write).
fn save_ledger(path: &str, ledger: &guidance_core::orchestrator::EscalationLedger) {
    let raw = serde_json::to_string_pretty(ledger).unwrap_or_default();
    if common_core::io::write_atomic(Path::new(path), raw.as_bytes()).is_err() {
        eprintln!("could not save {path}");
        std::process::exit(3);
    }
}

fn cmd_escalation_file(id: &str, ledger: &str) {
    let mut entries = load_ledger(ledger);
    if let Err(error) = entries.file(id) {
        eprintln!("refused: {error}");
        std::process::exit(2);
    }
    save_ledger(ledger, &entries);
    println!(
        "{}",
        serde_json::to_string_pretty(&entries).unwrap_or_default()
    );
}

fn cmd_escalation_claim(id: &str, owner: &str, ledger: &str) {
    let mut entries = load_ledger(ledger);
    if let Err(error) = entries.claim(id, owner) {
        eprintln!("refused: {error}");
        std::process::exit(2);
    }
    save_ledger(ledger, &entries);
    println!(
        "{}",
        serde_json::to_string_pretty(&entries).unwrap_or_default()
    );
}

fn cmd_escalation_resolve(id: &str, owner: &str, ledger: &str) {
    let mut entries = load_ledger(ledger);
    if let Err(error) = entries.resolve(id, owner) {
        eprintln!("refused: {error}");
        std::process::exit(2);
    }
    save_ledger(ledger, &entries);
    println!(
        "{}",
        serde_json::to_string_pretty(&entries).unwrap_or_default()
    );
}

/// Import plan items from a JSON array file into a reviewable plan.
fn cmd_plan_import(id: &str, file: &str) {
    let Ok(raw) = std::fs::read_to_string(file) else {
        eprintln!("cannot read {file}");
        std::process::exit(2);
    };
    let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) else {
        eprintln!("cannot parse {file} as a JSON array");
        std::process::exit(2);
    };
    let mut plan_items = Vec::new();
    for value in items {
        let (Some(item_id), Some(target)) = (
            value.get("id").and_then(serde_json::Value::as_str),
            value.get("target").and_then(serde_json::Value::as_str),
        ) else {
            eprintln!("refused: plan items need string id and target");
            std::process::exit(2);
        };
        plan_items.push(guidance_core::plans::PlanItem {
            id: item_id.to_string(),
            target: target.to_string(),
            acceptance: guidance_core::plans::AcceptanceGate {
                package: value
                    .get("package")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(".")
                    .to_string(),
                test_name: value
                    .get("test")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            },
            state: guidance_core::plans::PlanItemState::Open,
        });
    }
    match guidance_core::orchestrator::import_plan(id, plan_items) {
        Ok(plan) => println!(
            "{}",
            serde_json::to_string_pretty(&plan).unwrap_or_default()
        ),
        Err(error) => {
            eprintln!("refused: {error}");
            std::process::exit(2);
        }
    }
}

/// Tail the journal ledger from a cursor.
fn cmd_ledger_tail(journal_dir: &str, since: usize, count: usize) {
    for line in guidance_core::orchestrator::tail_ledger(Path::new(journal_dir), since, count) {
        println!("{line}");
    }
}

/// Request a context pack from a JSON PackInput file.
fn cmd_pack(file: &str) {
    let Ok(raw) = std::fs::read_to_string(file) else {
        eprintln!("cannot read {file}");
        std::process::exit(2);
    };
    let Ok(input) = serde_json::from_str::<guidance_core::query::context_pack::PackInput>(&raw)
    else {
        eprintln!("cannot parse {file} as a pack input");
        std::process::exit(2);
    };
    let (pack, manifest) = guidance_core::orchestrator::request_pack(&input);
    let out = serde_json::json!({ "pack": pack, "manifest": manifest });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

/// Measure a decision point on a gold corpus plus harvested journal
/// negatives; store a calibration entry only when the precision/recall
/// bars pass. Measurement itself is never an error: exit 0 carries the
/// AUTHORIZED or HELD verdict.
#[allow(clippy::too_many_arguments)]
/// Measure a decision point on a gold corpus plus harvested journal
/// negatives; store a calibration entry only when the precision/recall
/// bars pass. Measurement itself is never an error: exit 0 carries the
/// AUTHORIZED or HELD verdict.
fn cmd_calibrate(
    point: &str,
    dataset: &str,
    model_fingerprint: Option<String>,
    state_dir: &str,
    journal_dir: &str,
) {
    use fluent_llm::decision::CalibrationRegistry;
    use guidance_core::triage::{self, ROUTE_POINT, STATIC_PROCEDURE_FINGERPRINT, TRIAGE_POINT};
    if point == TRIAGE_POINT {
        let fingerprint = model_fingerprint
            .as_deref()
            .unwrap_or(STATIC_PROCEDURE_FINGERPRINT);
        let outcome = match triage::calibrate_point(
            Path::new(dataset),
            Path::new(journal_dir),
            fingerprint,
        ) {
            Ok(outcome) => outcome,
            Err(detail) => {
                eprintln!("calibrate: {detail}");
                std::process::exit(2);
            }
        };
        println!("point: {point}");
        println!("accuracy: {:.3}", outcome.report.accuracy);
        println!(
            "macro precision/recall: {:.3}/{:.3}",
            outcome.report.macro_precision, outcome.report.macro_recall
        );
        println!("support: {}", outcome.report.total);
        println!(
            "harvested: {} (skipped {})",
            outcome.harvested, outcome.skipped
        );
        println!(
            "control fires: {}/{}",
            outcome.control_fires.len(),
            outcome.control_total
        );
        match outcome.entry {
            Some(entry) => {
                let path = Path::new(state_dir).join("calibration.json");
                let mut registry = if path.exists() {
                    match CalibrationRegistry::load(&path) {
                        Ok(registry) => registry,
                        Err(detail) => {
                            eprintln!("calibrate: {detail}");
                            std::process::exit(2);
                        }
                    }
                } else {
                    CalibrationRegistry::empty()
                };
                registry.upsert(entry);
                if let Err(detail) = registry.save(&path) {
                    eprintln!("calibrate: {detail}");
                    std::process::exit(2);
                }
                println!("verdict: AUTHORIZED (entry stored at {})", path.display());
            }
            None => println!("verdict: HELD (below the precision/recall bars; nothing stored)"),
        }
    } else if point == ROUTE_POINT {
        match triage::route_agreement(Path::new(dataset)) {
            Ok((agreed, total)) => {
                println!("point: {point}");
                println!("static agreement: {agreed}/{total}");
                println!("verdict: MEASURED ONLY (no verdict scorer yet; nothing stored)");
            }
            Err(detail) => {
                eprintln!("calibrate: {detail}");
                std::process::exit(2);
            }
        }
    } else if point == guidance_core::capability::CAPABILITY_POINT {
        let nlp = guidance_core::query::ingest::default_en_pipeline();
        match guidance_core::capability::calibrate_capability_corpus(
            Path::new(dataset),
            nlp.as_ref(),
        ) {
            Ok(report) => {
                use guidance_core::capability::MIN_CAPABILITY_RECALL_AT_1;
                let hits = (report.recall_at_1 * report.support as f64).round() as usize;
                println!("point: {point}");
                println!(
                    "recall@1: {:.3} ({}/{})",
                    report.recall_at_1, hits, report.support
                );
                println!(
                    "control fires: {}/{}",
                    report.control_fires, report.control_total
                );
                println!("bars: recall@1 >= {MIN_CAPABILITY_RECALL_AT_1:.3}, control fires == 0");
                // Promotion is a separate explicit human decision (same
                // rule as scorecard baselines): the command measures and
                // reports, never stores — rank-only and shadow-only
                // stand until a human records a promotion with reason.
                if report.passed {
                    println!(
                        "verdict: PASS (bars met; promotion needs explicit human approval — nothing stored)"
                    );
                } else {
                    println!("verdict: HELD (below the bars; nothing stored)");
                }
            }
            Err(detail) => {
                eprintln!("calibrate: {detail}");
                std::process::exit(2);
            }
        }
    } else {
        eprintln!(
            "calibrate: unknown point {point} ({TRIAGE_POINT}, {ROUTE_POINT}, {})",
            guidance_core::capability::CAPABILITY_POINT
        );
        std::process::exit(2);
    }
}

/// Stale mirror files for a workspace, display paths relative to it.
/// Same roots and mirror as `cmd_status`, so strict mode and status
/// never disagree about what is stale.
fn mirror_stale_files(workspace: &str) -> Vec<String> {
    use guidance_core::sync_engine::absolute_or;
    let workspace_path = absolute_or(PathBuf::from(workspace));
    let cfg = load_project_config(&workspace_path);
    let gdir = workspace_path.join(".guidance");
    let mut out = Vec::new();
    for src_dir in src_dirs_from_config_with(&workspace_path, &cfg) {
        if !src_dir.is_dir() {
            continue;
        }
        let engine = SyncEngine::new(gdir.clone(), src_dir);
        for path in engine.stale_files() {
            let display = path.strip_prefix(&workspace_path).unwrap_or(&path);
            out.push(display.to_string_lossy().into_owned());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether the recorded index schema disagrees with this binary:
/// stored rows may misread, so reads refuse until a rebuild. Content
/// drift alone never refuses — freshness labels cover it, and the next
/// sync re-derives exactly the changed rows.
fn index_mismatched(db: &str) -> bool {
    use guidance_core::index_generation::{schema_generation, stored_schema};
    let index_dir = Path::new(db)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    // No marker means no index was ever built: reads as empty.
    let Some(stored) = stored_schema(&index_dir) else {
        return false;
    };
    stored != schema_generation()
}

/// Refuse an index read on generation mismatch: re-running sync
/// rebuilds instead of serving a mixed pair. rg reads live bytes and
/// never consults the marker.
fn refuse_stale_index(db: &str) -> bool {
    if !index_mismatched(db) {
        return false;
    }
    eprintln!("refused: index generation mismatch (re-run sync to rebuild)");
    true
}

/// Refuse a strict read when the mirror is not fresh. Prints the
/// freshness report naming files; true means the caller stops.
fn strict_refuse(workspace: &str) -> bool {
    let stale = mirror_stale_files(workspace);
    if stale.is_empty() {
        return false;
    }
    eprintln!("refused: mirror is not fresh (strict mode)");
    for file in &stale {
        eprintln!("stale: {file}");
    }
    eprintln!("run `guidance sync` to converge, then retry");
    true
}

fn cmd_verify(path: &str, workspace: &str, deep: bool) {
    use guidance_core::freshness::{probe_timestamp_granularity, verify_sidecar, VerifyVerdict};
    use guidance_core::sync_engine::absolute_or;
    let workspace_path = absolute_or(PathBuf::from(workspace));
    let candidate = absolute_or(PathBuf::from(path));
    if !candidate.is_file() {
        eprintln!("verify: not a file: {path}");
        std::process::exit(2);
    }
    let cfg = load_project_config(&workspace_path);
    let gdir = workspace_path.join(&cfg.guidance_dir);
    let mut engine_opt = None;
    for src_dir in src_dirs_from_config_with(&workspace_path, &cfg) {
        // Absolutize for the match: the candidate is absolute while the
        // configured roots may be workspace-relative.
        let root = guidance_core::sync_engine::absolute_or(src_dir.clone());
        if candidate.strip_prefix(&root).is_ok() {
            engine_opt = Some(SyncEngine::new(gdir.clone(), src_dir));
            break;
        }
    }
    let Some(engine) = engine_opt else {
        eprintln!("verify: {path} is outside the configured source roots");
        std::process::exit(2);
    };
    let sidecar = engine.sidecar_path(&candidate);
    let granularity = probe_timestamp_granularity(&gdir);
    let display = candidate
        .strip_prefix(&workspace_path)
        .unwrap_or(&candidate)
        .to_string_lossy()
        .into_owned();
    let code = match verify_sidecar(&sidecar, &candidate, granularity) {
        VerifyVerdict::Fresh => {
            println!("fresh: {display}");
            0
        }
        VerifyVerdict::Stale => {
            println!("stale: {display} (sidecar predates its inputs; re-run the loop)");
            1
        }
        VerifyVerdict::SourceChanged => {
            println!("source-changed: {display} (bytes moved under a preserved mtime)");
            1
        }
        VerifyVerdict::SkewSuspect => {
            println!("skew-suspect: {display} (bytes moved under a complete-or-newer clock)");
            1
        }
        VerifyVerdict::Tampered => {
            println!("tampered: {display} (sidecar body no longer matches its header)");
            1
        }
        VerifyVerdict::Unknown => {
            println!("unknown: {display} (no sidecar or no source to audit)");
            1
        }
    };
    if deep {
        print_verify_deep(&sidecar, &candidate, granularity);
    }
    if code != 0 {
        std::process::exit(code);
    }
}

/// Deep audit detail: recorded vs live hashes plus the clock reading.
/// Display only; the verdict above is already decided.
fn print_verify_deep(
    sidecar: &std::path::Path,
    source: &std::path::Path,
    granularity: std::time::Duration,
) {
    let content = std::fs::read_to_string(sidecar).unwrap_or_default();
    let (header, _) = fluent_types::sidecar::strip_header(&content);
    let recorded = header
        .and_then(|h| h.src_hash)
        .unwrap_or_else(|| "none".to_string());
    let live = std::fs::read(source)
        .ok()
        .map(|bytes| common_core::hash::sha256_hex(&bytes))
        .unwrap_or_else(|| "unreadable".to_string());
    let clock = match (
        common_core::io::mtime(sidecar),
        common_core::io::mtime(source),
    ) {
        (Some(a), Some(b)) if a > b => "sidecar newer than source",
        (Some(a), Some(b)) if a < b => "sidecar older than source",
        (Some(_), Some(_)) => "mtimes equal",
        _ => "clock unreadable",
    };
    println!("recorded: {recorded}");
    println!("live: {live}");
    println!("clock: {clock} (granularity {}ms)", granularity.as_millis());
}

fn cmd_status(guidance_dir: &str) {
    let gdir = PathBuf::from(guidance_dir);
    let workspace_path = std::env::current_dir().unwrap_or_default();
    let cfg = load_project_config(&workspace_path);
    let src_dirs = src_dirs_from_config_with(&workspace_path, &cfg);
    let mut total_files = 0usize;
    let mut stale_files = 0usize;
    let mut up_to_date = 0usize;
    for src_dir in &src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        let engine = SyncEngine::new(gdir.clone(), src_dir.clone());
        match engine.status() {
            Ok(status) => {
                total_files += status.total_files;
                stale_files += status.stale_files;
                up_to_date += status.up_to_date;
            }
            Err(e) => eprintln!("error: {e}"),
        }
    }
    println!("Sync Status:");
    println!("  Total files: {total_files}");
    println!("  Stale files: {stale_files}");
    println!("  Up to date:  {up_to_date}");
    println!("  Clean:       {}", stale_files == 0);
}

fn cmd_clean(json_dir: &str, db_path: &str) {
    println!("Cleaning generated files...");
    let db = Path::new(db_path);
    if db.exists() {
        std::fs::remove_file(db)
            .unwrap_or_else(|e| eprintln!("Warning: could not remove {db_path}: {e}"));
        println!("  Removed {db_path}");
    }
    let guidance_src = Path::new(json_dir).join("src");
    if guidance_src.exists() {
        walk::walk_files(&guidance_src, &["json"], |path| {
            std::fs::remove_file(path)
                .unwrap_or_else(|e| eprintln!("Warning: could not remove {:?}: {e}", path));
        });
        println!("  Removed generated JSON files");
    }
    clean_build_cache(Path::new(json_dir));
    println!("Clean complete.");
}

/// Best-effort byte count under a directory (symlinks never
/// followed; unreadable entries count zero, never abort the walk).
fn dir_size_bytes(dir: &Path) -> u64 {
    let mut bytes = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && !path.is_symlink() {
                stack.push(path);
            } else if let Ok(meta) = std::fs::symlink_metadata(&path) {
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }
    bytes
}

/// Human byte count beside the exact figure (`1.5 KiB (1536 bytes)`).
fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["bytes", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];
    for next in &UNITS[1..] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if unit == "bytes" {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {unit} ({bytes} bytes)")
    }
}

/// Whether a supervised run holds the build cache: cargo serializes
/// builds on an exclusive flock of `target/.cargo-lock`, which is
/// exactly what the fix-wave clippy invocations hold mid-build. A
/// missing lock file means no holder (nothing to contend with); any
/// other lock error fails open to removal, whose own error path
/// reports the skip.
fn target_cache_held(target: &Path) -> bool {
    let lock_path = target.join(".cargo-lock");
    let Ok(file) = std::fs::File::open(&lock_path) else {
        return false;
    };
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => {
            let _ = fs2::FileExt::unlock(&file);
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(_) => false,
    }
}

/// Remove the regenerable clippy build cache under the guidance dir,
/// reporting bytes reclaimed. Refuses with a message while a
/// supervised run holds it (cargo target lock busy, or removal
/// itself fails mid-race) — never touches sources, never hard-fails.
fn clean_build_cache(guidance_dir: &Path) {
    let target = guidance_dir.join("target");
    if !target.exists() {
        return;
    }
    if target_cache_held(&target) {
        println!(
            "  Skipped build cache {}: held by a supervised run (target lock busy).",
            target.display()
        );
        return;
    }
    let bytes = dir_size_bytes(&target);
    match std::fs::remove_dir_all(&target) {
        Ok(()) => println!(
            "  Removed build cache {} ({})",
            target.display(),
            format_bytes(bytes)
        ),
        Err(error) => println!(
            "  Skipped build cache {}: {error} (a supervised run may hold it).",
            target.display()
        ),
    }
}

fn cmd_check(workspace: &str, check_ready: bool, fixpoint: bool) {
    let workspace_path = PathBuf::from(workspace);
    let cfg = load_project_config(&workspace_path);
    let guidance_dir = workspace_path.join(".guidance");
    let mut all_passed = true;

    // Pre-commit convergence: bring the mirror to a fixpoint for the
    // changed set in bounded passes before the stages verify. Baseline
    // stages (test/lint/fmt) still verify only — the loop reports what,
    // if anything, remains instead of spinning.
    if fixpoint
        && !check_ready
        && !converge_mirror(&workspace_path, &workspace_path.join(".guidance"), &cfg)
    {
        all_passed = false;
    }

    let src_dirs = src_dirs_from_config_with(&workspace_path, &cfg);
    let present_exts = walk::collect_extensions(&src_dirs);

    type StageFn = Box<dyn Fn() -> Result<(), String>>;
    let mut stages: Vec<(&str, StageFn)> = Vec::new();

    // Full `check` runs project test/lint/fmt subprocesses first; the
    // `--check-ready` script contract probes readiness only.
    if !check_ready {
        for (ext, argv) in &cfg.test_commands {
            if !present_exts.contains(ext.as_str()) {
                continue;
            }
            let argv: Vec<String> = argv.clone();
            let ext = ext.clone();
            stages.push((
                "test",
                Box::new(move || {
                    let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                    if run_command(&argv_refs) {
                        Ok(())
                    } else {
                        Err(format!("{ext}: test command failed"))
                    }
                }),
            ));
        }

        for (ext, argv) in &cfg.lint_commands {
            if !present_exts.contains(ext.as_str()) {
                continue;
            }
            let argv: Vec<String> = argv.clone();
            let ext = ext.clone();
            stages.push((
                "lint",
                Box::new(move || {
                    let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                    if run_command(&argv_refs) {
                        Ok(())
                    } else {
                        Err(format!("{ext}: lint command failed"))
                    }
                }),
            ));
        }

        for (ext, argv) in &cfg.fmt_commands {
            if !present_exts.contains(ext.as_str()) {
                continue;
            }
            let argv: Vec<String> = argv.clone();
            let ext = ext.clone();
            stages.push((
                "fmt",
                Box::new(move || {
                    let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                    if run_command(&argv_refs) {
                        Ok(())
                    } else {
                        Err(format!("{ext}: fmt command failed"))
                    }
                }),
            ));
        }
    }

    let cfg_gen = cfg.clone();
    let ws_gen = workspace_path.clone();
    let gd_gen = guidance_dir.clone();
    stages.push((
        "gen",
        Box::new(move || {
            let src_dirs = src_dirs_from_config_with(&ws_gen, &cfg_gen);
            let mut total_stale = 0usize;
            for src_dir in &src_dirs {
                if !src_dir.is_dir() {
                    continue;
                }
                let engine = SyncEngine::new(gd_gen.clone(), src_dir.clone());
                let status = engine.status().map_err(|e| format!("status: {e}"))?;
                total_stale += status.stale_files;
            }
            if total_stale > 0 {
                Err(format!("{total_stale} stale files need regeneration"))
            } else {
                Ok(())
            }
        }),
    ));

    let structure_path = workspace_path.join("STRUCTURE.md");
    stages.push((
        "structure",
        Box::new(move || {
            if structure_path.exists() {
                Ok(())
            } else {
                Err("STRUCTURE.md not found".into())
            }
        }),
    ));

    let db_path = workspace_path.join(".guidance.db");
    stages.push((
        "db",
        Box::new(move || {
            if db_path.exists() {
                Ok(())
            } else {
                Err(".guidance.db not found".into())
            }
        }),
    ));

    for (name, stage_fn) in &stages {
        if check_ready {
            if let Err(reason) = stage_fn() {
                println!("NOT READY: {name}: {reason}");
                std::process::exit(1);
            }
            continue;
        }
        print!("{name}... ");
        match stage_fn() {
            Ok(()) => println!("OK"),
            Err(e) => {
                println!("FAILED: {e}");
                all_passed = false;
                break;
            }
        }
    }

    if check_ready {
        println!("READY");
    } else if all_passed {
        println!("\nAll checks passed");
    } else {
        std::process::exit(1);
    }
}

fn converge_mirror(
    workspace_path: &Path,
    guidance_dir: &Path,
    cfg: &config::ProjectConfig,
) -> bool {
    const MAX_PASSES: u32 = 5;
    let src_dirs = src_dirs_from_config_with(workspace_path, cfg);
    adopt_renames(guidance_dir, &src_dirs, true);
    for pass in 1..=MAX_PASSES {
        let mut generated = 0usize;
        let mut failed = 0usize;
        for src_dir in src_dirs_from_config_with(workspace_path, cfg) {
            if !src_dir.is_dir() {
                continue;
            }
            let mut engine = SyncEngine::new(guidance_dir.to_path_buf(), src_dir.clone());
            match engine.gen_scoped(&src_dir) {
                Ok((made, bad)) => {
                    generated += made;
                    failed += bad;
                }
                Err(error) => {
                    println!("fixpoint pass {pass}: scope error: {error}");
                    return false;
                }
            }
        }
        println!("fixpoint pass {pass}: {generated} generated, {failed} failed");
        if failed > 0 {
            println!("fixpoint: {failed} files failed, remainder reported by check");
            return false;
        }
        if generated == 0 {
            if pass > 1 {
                println!("fixpoint: mirror clean");
            }
            return true;
        }
    }
    println!("fixpoint: still converging after {MAX_PASSES} passes");
    false
}

/// Converge the workspace through the deterministic fix loop: mirror
/// to fixpoint, fix the stale set in waves (format + machine-applicable
/// lint, journaled and witnessed), reindex, and report with contract
/// exit codes (0 clean, 1 work remains, 2 stuck, 4 halted).
async fn cmd_run(workspace: &str, json_dir: &str, db: &str, max_waves: usize, autonomy: &str) {
    use guidance_core::maintenance::{MaintenanceQueue, WorkState};
    let workspace_path = PathBuf::from(workspace);
    let guidance_dir = PathBuf::from(json_dir);
    let cfg = load_project_config(&workspace_path);
    ensure_dir_or_panic(&guidance_dir);
    let policy = guidance_core::policy::MaintenancePolicy {
        autonomy: autonomy.to_string(),
        ..guidance_core::policy::MaintenancePolicy::default()
    };

    // 1. Rotation sweep first: backdated receipts re-enter the stale
    // set derived below instead of hiding behind fresh mtimes.
    sweep_for_rotation(
        &guidance_dir,
        &src_dirs_from_config_with(&workspace_path, &cfg),
        true,
    );
    // 1b. Dirty set from staleness (make semantics: fresh files
    // carry a verifying receipt and need no work). Rust only: format
    // and clippy are the toolchain rungs the loop owns. The mirror
    // converges next, but the set is already pinned.
    let src_dirs = src_dirs_from_config_with(&workspace_path, &cfg);
    let mut dirty: Vec<PathBuf> = Vec::new();
    for src_dir in &src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        let engine = SyncEngine::new(guidance_dir.clone(), src_dir.clone());
        for path in engine.stale_files() {
            if path.extension().and_then(|ext| ext.to_str()) == Some("rs") && path.is_file() {
                dirty.push(path);
            }
        }
    }
    dirty.sort();
    dirty.dedup();

    // 2. Converge the mirror above the pinned set: the fix loop works
    // above fresh sidecars, and clippy attribution reads live bytes.
    if !converge_mirror(&workspace_path, &guidance_dir, &cfg) {
        eprintln!("run: mirror did not converge; fix the scope errors above");
        std::process::exit(3);
    }

    if dirty.is_empty() {
        // The index gate still runs with no fix work: generation is
        // verified (and rebuilt on mismatch) on every run, not just
        // on runs that rewrite files.
        refresh_search_index(&workspace_path, &guidance_dir, db, &src_dirs, true).await;
        println!("run: clean (0 stale files)");
        return;
    }

    // Autonomy cap: L0 converges the mirror and the index, never the
    // tree. The queue still records what a higher autonomy would own.
    if !guidance_core::policy::autonomy_allows_fix(&policy) {
        refresh_search_index(&workspace_path, &guidance_dir, db, &src_dirs, true).await;
        println!(
            "run: clean (autonomy L0: index only, {} stale files held)",
            dirty.len()
        );
        return;
    }

    // 3. Durable queue record of what this run owns (crash-resume).
    let state_dir = guidance_dir.join("state");
    if common_core::io::ensure_dir(&state_dir).is_err() {
        eprintln!("warning: state dir not writable");
    }
    let snapshot_path = state_dir.join("queue.json");
    let mut queue = MaintenanceQueue::new();
    for path in &dirty {
        let scope = path
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = queue.enqueue(&path.to_string_lossy(), &scope, "clippy-fix", &[]);
    }
    queue.set_snapshot_path(&snapshot_path);
    if queue.save_snapshot(&snapshot_path).is_err() {
        eprintln!("warning: queue snapshot not written");
    }

    // 4. Fix waves plus the model-draft rung (shared with `sync`):
    // mechanical rungs first, then local code-model drafts for whatever
    // the waves could not converge.
    let fix = run_fix_scope(&dirty, &guidance_dir, &cfg, &policy, max_waves, true);
    let mut report = fix.waves;
    let drafted: std::collections::HashSet<String> = fix.drafts.kept_files.into_iter().collect();
    report
        .escalated
        .retain(|esc| !drafted.contains(esc.file.as_str()));
    report.fixed += drafted.len();
    if !drafted.is_empty() {
        // Drafts landed after the witness wave, so the witness no longer
        // covers the tree: the next run re-witnesses to clean.
        report.witness_clean = false;
        println!(
            "model drafts: {} kept, {} failed, {} skipped",
            fix.drafts.kept, fix.drafts.failed, fix.drafts.skipped
        );
    }

    // 5. Record wave outcomes on the queue: converged files verify,
    // escalated files stay owned with a reason.
    for path in &dirty {
        let id = path.to_string_lossy().into_owned();
        let escalated = report.escalated.iter().any(|esc| esc.file == id);
        queue.set_state(
            &id,
            if escalated {
                WorkState::Escalated
            } else {
                WorkState::Verified
            },
        );
    }
    if queue.save_snapshot(&snapshot_path).is_err() {
        eprintln!("warning: queue snapshot not written");
    }

    // 6. Reindex what the waves changed so the knowledge base is
    // current when the report lands.
    refresh_search_index(&workspace_path, &guidance_dir, db, &src_dirs, true).await;

    // 5b. Stuck files keep a truthful stale verdict: backdate escalated
    // sidecars below their sources while the snapshot explains why.
    // Runs after the final refresh — the reindex would otherwise
    // regenerate (and re-fresh) the stuck record it just explained.
    let stuck: Vec<String> = report
        .escalated
        .iter()
        .map(|esc| esc.file.clone())
        .collect();
    let backdated = backdate_stuck(&guidance_dir, &src_dirs, &stuck);
    if backdated > 0 {
        println!("stuck: {backdated} files backdated below their sources");
    }

    // 7. Contract report: fixed, escalated with reasons, witness.
    println!(
        "run: {} files, {} waves, {} clippy runs",
        dirty.len(),
        report.waves_run,
        report.clippy_invocations
    );
    println!("fixed: {} (formatted: {})", report.fixed, report.formatted);
    println!("escalated: {}", report.escalated.len());
    for esc in &report.escalated {
        println!("  {} ({})", esc.file, esc.reason);
    }
    println!(
        "witness: {}",
        if report.witness_clean {
            "clean"
        } else {
            "not clean"
        }
    );
    let code = report.exit_code().as_i32();
    if code != 0 {
        std::process::exit(code);
    }
}

/// Waves budget for the sync fix pre-pass: one fixing wave plus its
/// witness. Escalations fall through to the model-draft rung.
const SYNC_FIX_MAX_WAVES: usize = 2;
/// Pre-pass file budget: beyond this many dirty files the fix loop is
/// skipped (whole-tree convergence belongs to `run`), so a cleared or
/// initial tree still indexes swiftly instead of fixing the world.
const SYNC_FIX_MAX_FILES: usize = 32;

/// Escalation handoff: the only fix-loop output on a quiet run. Names
/// every file the waves plus model drafts could not converge, with the
/// reason the ladder stopped and the last draft detail, so the
/// orchestrating human or agent inherits the complete error. Silent
/// when nothing escalated.
fn print_fix_handoff(
    escalated: &[guidance_core::waves::Escalation],
    drafts: &guidance_core::waves::ModelDraftReport,
) {
    if escalated.is_empty() {
        return;
    }
    let files = escalated.len();
    println!(
        "sync: {} {} attention (unfixed after lint, fix, and model-draft tries; drafts kept {})",
        files,
        if files == 1 {
            "file needs"
        } else {
            "files need"
        },
        drafts.kept
    );
    for esc in escalated {
        println!("  {} ({})", esc.file, esc.reason);
        println!("    fingerprint: {}", esc.fingerprint);
        for (file, detail) in &drafts.failures {
            if file == &esc.file {
                println!("    model draft: {detail}");
            }
        }
    }
}

/// Fix-scope outcome: mechanical waves plus the model-draft rung.
struct FixScopeOutcome {
    waves: guidance_core::waves::WaveReport,
    drafts: guidance_core::waves::ModelDraftReport,
}

/// Lint, fix, and format `files` before indexing: rustfmt plus the
/// mechanical clippy rungs under the policy attempt budget, then up to
/// three local code-model drafts per file the waves could not converge.
/// Honors the autonomy cap (L0 never rewrites); a dark or unreachable
/// model endpoint skips the draft rung instead of failing. Shared by
/// `run` (workspace scope) and `sync` (workspace or single file): fix
/// always precedes member gen and indexing on the same bytes.
fn run_fix_scope(
    files: &[PathBuf],
    guidance_dir: &Path,
    cfg: &config::ProjectConfig,
    policy: &guidance_core::policy::MaintenancePolicy,
    max_waves: usize,
    verbose: bool,
) -> FixScopeOutcome {
    // Live toolchain driver, the configured decision endpoint, and a
    // dedicated target dir (no lock fights with the user's own cargo).
    // The calibration registry loads once; a missing file is the
    // seed-empty registry (shadow-only holds).
    let registry_path = cfg
        .calibration_registry
        .clone()
        .unwrap_or_else(|| guidance_dir.join("state").join("calibration.json"));
    let calibration = match std::fs::read_to_string(&registry_path) {
        Ok(raw) => match serde_json::from_str(&raw) {
            Ok(registry) => registry,
            Err(error) => {
                if verbose {
                    eprintln!("warning: calibration registry unreadable: {error}");
                }
                fluent_llm::decision::CalibrationRegistry::empty()
            }
        },
        Err(_) => fluent_llm::decision::CalibrationRegistry::empty(),
    };
    let decision = guidance_core::waves::DecisionEndpoint {
        base_url: cfg.decision.base_url.clone(),
        model: cfg.decision.model.clone(),
        timeout_ms: cfg.decision.timeout_ms,
        enabled: cfg.decision.enabled,
    };
    let clippy = guidance_core::waves::live_runner(guidance_core::clippy::ClippyCtx {
        target_dir: Some(guidance_dir.join("target")),
        shaping: guidance_core::clippy::shaping_for(false),
        extra_args: Vec::new(),
    });
    let protected: Vec<String> = Vec::new();
    let wave_ctx = guidance_core::waves::WaveCtx {
        journal_dir: guidance_dir.join("journal"),
        wal_dir: guidance_dir.join("wal"),
        protected_globs: &protected,
        allow_pub_change: false,
        allow_cfg_change: false,
        max_bytes: 1_000_000,
        max_changed_lines: 500,
        max_waves,
        max_attempts: policy.attempt_limit,
        decision,
        calibration,
        mechanical_classes: &policy.mechanical_classes,
        requires_approval: &policy.requires_approval,
        clippy,
    };
    let waves = guidance_core::waves::run_fix_waves(files, &wave_ctx);
    let escalated: Vec<(PathBuf, String)> = waves
        .escalated
        .iter()
        .map(|esc| (PathBuf::from(&esc.file), esc.reason.clone()))
        .collect();
    let generative = guidance_core::waves::GenerativeEndpoint {
        base_url: cfg.generative.base_url.clone(),
        model: cfg.generative.model.clone(),
        timeout_ms: cfg.generative.timeout_ms,
        enabled: cfg.generative.enabled,
    };
    let drafts = if escalated.is_empty() {
        guidance_core::waves::ModelDraftReport::default()
    } else {
        guidance_core::waves::run_model_draft_rung(&escalated, &generative)
    };
    FixScopeOutcome { waves, drafts }
}

/// Knowledge-base summary for orchestrators: one JSON object naming
/// the mirror, its freshness, the derived index, and the commands that
/// consume this workspace. Counts come from sidecars on disk (parse
/// failures count zero members, never fail the command).
fn cmd_kb(workspace: &str, json_dir: &str, db: &str) {
    let workspace_path = PathBuf::from(workspace);
    let guidance_dir = PathBuf::from(json_dir);
    let cfg = load_project_config(&workspace_path);
    let src_dirs = src_dirs_from_config_with(&workspace_path, &cfg);
    let mut files = 0usize;
    let mut stale = 0usize;
    for src_dir in &src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        let engine = SyncEngine::new(guidance_dir.clone(), src_dir.clone());
        match engine.status() {
            Ok(status) => {
                files += status.total_files;
                stale += status.stale_files;
            }
            Err(error) => eprintln!("warning: status failed: {error}"),
        }
    }
    let mut members = 0usize;
    walk::walk_files(&guidance_dir.join("src"), &["json"], |path| {
        if let Ok(raw) = std::fs::read_to_string(path) {
            // Sidecars carry a header first line; members live in the
            // JSON body below it (never fail the command on odd files).
            let body = fluent_types::sidecar::strip_header(&raw).1;
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
                if let Some(items) = value.get("members").and_then(|m| m.as_array()) {
                    members += items.len();
                }
            }
        }
    });
    let out = serde_json::json!({
        "workspace": workspace,
        "mirror": json_dir,
        "files": files,
        "stale": stale,
        "fresh": stale == 0,
        "members": members,
        "schema": guidance_core::query::envelope::ENVELOPE_SCHEMA_VERSION,
        "index": {"db": db, "exists": Path::new(db).is_file()},
        "query": [
            "guidance search --fuse --workspace <ws> \"<question>\"",
            "guidance explain --workspace <ws> \"<question>\"",
            "guidance impact --workspace <ws> <symbol|path>",
        ],
        "maintain": [
            "guidance run --workspace <ws>",
            "guidance verify --workspace <ws> <file>",
            "guidance seal --workspace <ws>",
        ],
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

fn cmd_component_gate(path: &str) {
    // Read-only lookup: prints the owning package, matrix component (if
    // any), and narrowest gate, then exits. No side effects.
    let resolved = guidance_core::component_map::component_gate_for_path(path);
    println!("path: {path}");
    if resolved.package.is_empty() {
        println!("package: (unknown)");
    } else {
        println!("package: {}", resolved.package);
    }
    match resolved.component {
        Some(component) => println!("component: {component}"),
        None => println!("component: (none)"),
    }
    println!("gate: {}", resolved.gate);
}

#[allow(clippy::too_many_arguments)]
async fn cmd_scorecard(
    format: &str,
    deep: bool,
    live: bool,
    baseline: &str,
    update_baseline: bool,
    reason: &str,
    allow_regression: bool,
    tolerance: &[String],
    bound: &[String],
) {
    use guidance_core::scorecard::{
        apply_baseline_update, baseline_replays, baseline_to_value, load_baseline, render_human,
        render_json, run_deep_extra, run_fast_tier, score_metrics, today_utc,
    };
    // Live tier prints the exact model-gated commands and runs nothing:
    // hermetic runs never touch a model endpoint.
    if live {
        println!("live scorecard tier — run these exact commands with a model endpoint:");
        println!(
            "  LLM_BASE_URL=http://127.0.0.1:8080 LLM_MODEL=liquidai/d1-2b make guidance-test-live"
        );
        println!("  # per-point calibration and promotion: doc/guidance/CALIBRATION.md");
        return;
    }
    let mut measured = run_fast_tier().await;
    if deep {
        measured.extend(run_deep_extra());
    }
    let baseline_path = std::path::Path::new(baseline);
    // The one explicit baseline command: fresh values in, reason and diff
    // recorded, regressions refused unless explicitly allowed. The file is
    // never hand-edited — history replay proves every value went through
    // this path.
    if update_baseline {
        let current = load_baseline(baseline_path);
        match apply_baseline_update(
            current.as_ref(),
            &measured,
            tolerance,
            bound,
            reason,
            allow_regression,
            &today_utc(),
        ) {
            Ok(next) => {
                debug_assert!(baseline_replays(&next));
                let value = baseline_to_value(&next, reason, &today_utc());
                let text = serde_json::to_string_pretty(&value).expect("serializes");
                if let Some(parent) = baseline_path.parent() {
                    common_core::io::ensure_dir(parent).expect("baseline dir");
                }
                std::fs::write(baseline_path, format!("{text}\n")).expect("baseline writes");
                let entry = next.history.last().expect("history appended");
                println!(
                    "baseline updated: {} ({})",
                    baseline_path.display(),
                    entry.date
                );
                println!("reason: {}", entry.reason);
                let mut changes: Vec<(&String, &(Option<f64>, f64))> =
                    entry.changes.iter().collect();
                changes.sort_by(|left, right| left.0.cmp(right.0));
                for (name, (old, new)) in changes {
                    match old {
                        Some(old) => println!("  {name}: {old} -> {new}"),
                        None => println!("  {name}: (new) -> {new}"),
                    }
                }
            }
            Err(error) => {
                eprintln!("baseline update refused: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let base = load_baseline(baseline_path);
    if base.is_none() {
        eprintln!("warning: no baseline at {baseline}; every metric reports no-baseline");
    }
    let card = score_metrics(&measured, &base);
    match format {
        "json" => println!("{}", render_json(&card)),
        _ => print!("{}", render_human(&card)),
    }
    if card.exit_code != 0 {
        std::process::exit(card.exit_code);
    }
}

fn cmd_structure(json_dir: &str) {
    let gdir = PathBuf::from(json_dir);
    match structure::generate(&gdir) {
        Ok(output) => {
            common_core::io::write_atomic(std::path::Path::new("STRUCTURE.md"), output.as_bytes())
                .expect("write STRUCTURE.md");
            let line_count = output.lines().count();
            println!("STRUCTURE.md generated ({} lines)", line_count);
        }
        Err(e) => {
            eprintln!("error generating structure: {e}");
        }
    }
}

/// Removed surface: the subcommand is parsed so existing scripts get a
/// pointed message instead of a bare usage error. State lives in files
/// and the mirror database; nothing here writes history or metrics.
fn removed_command(name: &str, use_instead: &str) {
    eprintln!("removed: `guidance {name}` is not part of the navigation engine; use {use_instead}");
    std::process::exit(2);
}

fn cmd_mcp(db_path: &str, workspace: &str, json_dir: &str, toolset: &str) {
    let toolset = match mcp::Toolset::parse(toolset) {
        Ok(toolset) => toolset,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    let db = PathBuf::from(db_path);
    if let Err(e) = mcp::serve_stdio_with_options(
        &db,
        Some(PathBuf::from(workspace)),
        Some(PathBuf::from(json_dir)),
        toolset,
    ) {
        eprintln!("MCP server error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn test_cli_help() {
        let mut cmd = super::Cli::command();
        let help = cmd.render_help();
        assert!(!help.to_string().is_empty());
    }

    #[test]
    fn rotation_sweep_backdates_only_rotated_receipts() {
        use std::time::SystemTime;
        // A sidecar whose recorded toolchain differs from the current
        // vector is backdated; a current one is untouched.
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("a.rs"), "fn a() {}\n").expect("write");
        std::fs::write(src.join("b.rs"), "fn b() {}\n").expect("write");
        // Sleep-free staleness: backdate via explicit timestamps.
        let now = SystemTime::now();
        let sidecar_dir = dir.path().join(".guidance").join("src");
        std::fs::create_dir_all(&sidecar_dir).expect("mkdir");
        let stale_header = "GHDR1 src=a\tbody=b\tcomp=toolchain=old\n";
        std::fs::write(
            sidecar_dir.join("a.rs.json"),
            format!("{stale_header}{{\"members\":[]}}"),
        )
        .expect("write");
        std::fs::write(
            sidecar_dir.join("b.rs.json"),
            format!("{stale_header}{{\"members\":[]}}"),
        )
        .expect("write");
        for name in ["a.rs.json", "b.rs.json"] {
            let path = sidecar_dir.join(name);
            let file = std::fs::File::options()
                .write(true)
                .open(&path)
                .expect("open");
            file.set_modified(now).expect("touch");
        }
        let backdated = super::sweep_for_rotation(
            &dir.path().join(".guidance"),
            std::slice::from_ref(&src),
            false,
        );
        assert_eq!(backdated, 2, "both receipts rotated");
        for name in ["a.rs.json", "b.rs.json"] {
            let mtime = std::fs::metadata(sidecar_dir.join(name))
                .expect("meta")
                .modified()
                .expect("mtime");
            assert!(mtime < now, "backdated below the read time");
        }
    }

    #[test]
    fn find_source_dir_prefers_source_root_over_parent() {
        // Sidecars mirror the source root, never the file's parent: a
        // nested file resolves to `src`, so its sidecar keeps the
        // parallel `sub/` segment instead of flattening beside sources.
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("sub")).expect("mkdir");
        let file = src.join("sub").join("nested.rs");
        std::fs::write(&file, "pub fn nested() {}\n").expect("write");
        let found = super::find_source_dir(&file, std::slice::from_ref(&src));
        assert_eq!(found.as_deref(), Some(src.as_path()));
    }

    #[test]
    fn find_source_dir_ignores_file_entries_and_misses_outside_roots() {
        // A file entry scopes an ingest, not a source tree: it never
        // resolves, and neither does a file outside every root.
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        let file = src.join("a.rs");
        std::fs::write(&file, "pub fn a() {}\n").expect("write");
        assert!(super::find_source_dir(&file, std::slice::from_ref(&file)).is_none());
        let elsewhere = dir.path().join("other.rs");
        std::fs::write(&elsewhere, "pub fn o() {}\n").expect("write");
        assert!(super::find_source_dir(&elsewhere, std::slice::from_ref(&src)).is_none());
    }
}
