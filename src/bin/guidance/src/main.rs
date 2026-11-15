#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use common_core::ensure_dir_or_panic;
use common_core::profile;
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
mod freshen;
mod index_cmd;
mod mcp;
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

        /// Include fixture-scope rows (default excludes them).
        #[arg(long)]
        include_fixtures: bool,

        #[arg(long)]
        allow_degraded: bool,

        /// Section budget override (tokens); per-intent table otherwise.
        #[arg(long)]
        budget: Option<usize>,

        /// Decision fingerprint authorizing filtering; absent is shadow-only.
        #[arg(long)]
        decision_fingerprint: Option<String>,

        /// Compact output: omit the duplicated Coverage section (Envelope keeps it).
        #[arg(long)]
        compact: bool,

        /// Converge the stale files behind the citations over bounded runs.
        /// Sidecars plus index only: no fix waves.
        #[arg(long)]
        freshen: bool,

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
    /// Converge the workspace: fix every stale file, refresh the mirror
    /// and index, report. Exit 0 clean, 1 work remains, 2 stuck, 4 halted.
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

        /// Autonomy cap (`L0` index only; `L1`+ deterministic fixers;
        /// `L3` adds model drafts).
        #[arg(long, default_value = "L2")]
        autonomy: String,

        /// Wave budget for the fix loop.
        #[arg(long, default_value_t = 4)]
        max_waves: usize,

        /// Also run the background comment-summary pass after the index
        /// rebuilds (opt-in; the query path never calls a model).
        #[arg(long)]
        summarize: bool,

        /// Enable embedding backfill (default off; explicit opt-in).
        #[arg(long)]
        embed: bool,
    },
    Status {
        #[arg(short = 'g', long, default_value = ".guidance")]
        guidance_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        #[arg(long)]
        json: bool,
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

        #[arg(long)]
        allow_degraded: bool,

        /// Include fixture-scope rows (default excludes them).
        #[arg(long)]
        include_fixtures: bool,

        /// Converge the stale files behind the hits over bounded re-runs.
        /// Sidecars plus index only; live-byte `--rg` ignores this.
        #[arg(long)]
        freshen: bool,
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
    /// Alias for `sync` (full workspace convergence). Exit 0 clean,
    /// 1 work remains, 2 stuck, 4 halted.
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

        /// Autonomy cap (`L0` index only; `L1`+ deterministic fixers;
        /// `L3` adds model drafts).
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
        /// fields ignored). Required: tasks are never synthesized from
        /// the index under test (the checked-in orientation battery is
        /// `src/guidance/corpora/benchmark_tasks.jsonl`).
        #[arg(long)]
        tasks: Option<String>,

        /// Task cap (first `num` file tasks).
        #[arg(long, default_value_t = 10)]
        num: usize,

        /// Finding limit behind the shipping arm (like `explain`).
        #[arg(short = 'l', long, default_value_t = 10)]
        limit: usize,

        /// Output format: human or json (json is the agent handoff).
        #[arg(long, default_value = "human")]
        format: String,

        /// Print per-arm source-scope counts in the human report.
        #[arg(long)]
        scope_report: bool,
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
    },
    Impact {
        /// Symbol name or file path whose impact to report.
        target: String,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        /// Include fixture-scope callers (default excludes them).
        #[arg(long)]
        include_fixtures: bool,
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
            include_fixtures,
            allow_degraded,
            budget,
            decision_fingerprint,
            compact,
            freshen,
            filter: _,
        }) => {
            if cli.strict && strict_refuse(workspace) {
                std::process::exit(2);
            }
            let readiness = readiness_preflight(workspace, db, *allow_degraded);
            let started = std::time::Instant::now();
            cmd_explain(
                query,
                guidance,
                db,
                workspace,
                *limit,
                *no_llm,
                *include_fixtures,
                *budget,
                cli.show_prompts,
                cli.strict,
                readiness,
                started,
                decision_fingerprint.clone(),
                *compact,
                *allow_degraded,
                *freshen,
            );
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
            max_waves,
            summarize,
            embed,
        }) => {
            let code = cmd_sync(
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
                *max_waves,
                *summarize,
                *embed,
            )
            .await;
            if code != 0 && !watch {
                std::process::exit(code);
            }
        }
        Some(Commands::Status {
            guidance_dir,
            db,
            json,
        }) => cmd_status(guidance_dir, db, *json),
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
            allow_degraded,
            include_fixtures,
            freshen,
        }) => {
            if cli.strict && strict_refuse(workspace) {
                std::process::exit(2);
            }
            // rg reads live bytes; every other mode reads the index.
            if !rg {
                readiness_preflight(workspace, db, *allow_degraded);
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
                *include_fixtures,
                *freshen,
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
        }) => {
            let code = cmd_sync(
                None, None, workspace, json_dir, db, false, false, false, false, false, 500,
                autonomy, *max_waves, false, false,
            )
            .await;
            if code != 0 {
                std::process::exit(code);
            }
        }
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
            scope_report,
        }) => {
            benchmark::cmd_benchmark(
                workspace,
                db,
                tasks.as_deref(),
                *num,
                *limit,
                format,
                *scope_report,
            );
        }
        Some(Commands::Summarize {
            workspace,
            json_dir,
            db,
        }) => cmd_summarize(workspace, json_dir, db).await,
        Some(Commands::Impact {
            target,
            workspace,
            include_fixtures,
        }) => cmd_impact(target, workspace, *include_fixtures),
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
                    false,
                    false,
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

pub(crate) fn load_project_config(workspace: &Path) -> config::ProjectConfig {
    config::load_config(workspace).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
/// `explain`: thin adapter over `ReadEngine`. Assembly, budgets,
/// claims, and the envelope live in the engine; this only injects
/// the workspace, the index, and the code-model synthesizer, then
/// renders. `--no-llm` builds no model pipeline at all (no spaCy,
/// no synthesis): evidence-only answers need none.
fn answer_once(
    engine: &guidance_core::read::engine::ReadEngine,
    request: &guidance_core::read::engine::ExplainRequest,
    ctx: &guidance_core::read::engine::ExplainContext<'_>,
    synth: Option<&dyn guidance_core::read::engine::Synthesizer>,
    judge: Option<&guidance_core::read::engine::JudgeCtx<'_>>,
) -> guidance_core::read::engine::ReadAnswer {
    match (synth, judge) {
        (None, None) => engine.explain(request, ctx, None),
        (None, Some(judge)) => engine.explain_with_judge(request, ctx, None, Some(judge)),
        (Some(synth), None) => engine.explain(request, ctx, Some(synth)),
        (Some(synth), Some(judge)) => {
            engine.explain_with_judge(request, ctx, Some(synth), Some(judge))
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// `explain`: thin adapter over `ReadEngine`. Assembly, budgets,
/// claims, and the envelope live in the engine; this only injects
/// the workspace, the index, and the code-model synthesizer, then
/// renders. `--no-llm` builds no model pipeline at all (no spaCy,
/// no synthesis): evidence-only answers need none.
fn cmd_explain(
    query: &str,
    guidance_dir: &str,
    db_path: &str,
    workspace: &str,
    limit: usize,
    no_llm: bool,
    include_fixtures: bool,
    budget: Option<usize>,
    show_prompts: bool,
    strict: bool,
    readiness: guidance_core::read::readiness::Readiness,
    started: std::time::Instant,
    decision_fingerprint_flag: Option<String>,
    compact: bool,
    allow_degraded: bool,
    freshen: bool,
) {
    let _explain = profile::Phase::new("explain.total");
    use guidance_core::read::engine::{ExplainContext, ExplainRequest, ReadEngine, Synthesizer};
    let workspace_root = PathBuf::from(workspace);
    let cfg = load_project_config(&workspace_root);
    let db_file = if Path::new(db_path).is_absolute() {
        PathBuf::from(db_path)
    } else {
        workspace_root.join(db_path)
    };
    let db = match search_vector::GuidanceDb::open(&db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!("error: cannot open index at {}: {error}", db_file.display());
            std::process::exit(2);
        }
    };
    // No model pipeline on the extractive path: lemma fallback and
    // FTS need no spaCy, and synthesis is off. The holder builds at
    // most once per command, on first SVO need.
    let nlp_holder = LazyNlp::new();
    let nlp = nlp_holder.get_unless(no_llm);
    let ctx = ExplainContext {
        workspace: &workspace_root,
        db: &db,
        nlp,
        coverage: Some(readiness.coverage().clone()),
    };
    let request = ExplainRequest {
        query: query.to_string(),
        limit,
        budget_override: budget,
        no_llm,
        include_fixtures,
    };
    struct CodeSynth<'a> {
        backend: &'a dyn fluent_llm::client::ChatBackend,
    }
    impl Synthesizer for CodeSynth<'_> {
        fn synthesize(&self, prompt: &str) -> Result<Option<String>, String> {
            let messages = vec![fluent_llm::protocol::ChatMessage::with_role(
                fluent_llm::roles::ChatRole::User,
                prompt,
            )];
            let extras = serde_json::json!({
                "temperature": 0.0,
                "max_tokens": search::EXPLAIN_ANSWER_MAX_TOKENS,
            });
            match self.backend.chat_complete_with_extras(&messages, &extras) {
                Ok(text) if text.trim().is_empty() => Ok(None),
                Ok(text) => Ok(Some(text)),
                Err(error) => Err(error.to_string()),
            }
        }
    }
    let engine = ReadEngine::new();
    // Shadow judge wiring: SystemOne decision role only; anything else
    // stays shadow-only with a labelled gap. `--no-llm` builds no judge.
    let fingerprint = decision_fingerprint_flag.or(cfg.decision_fingerprint.clone());
    let _calibration = guidance_core::config::calibration_path(&cfg, &workspace_root);
    let judge_backend: Option<fluent_llm::decision::SystemOneBackend> = if no_llm
        || !cfg.decision.enabled
        || cfg.decision.transport != guidance_core::config::ModelTransport::SystemOne
    {
        None
    } else {
        Some(fluent_llm::decision::SystemOneBackend::new(
            cfg.decision.base_url.clone(),
            fluent_llm::decision::DecisionTransport::local_only(),
        ))
    };
    let judge = judge_backend
        .as_ref()
        .map(|backend| guidance_core::read::engine::JudgeCtx {
            backend,
            model: cfg.decision.model.clone(),
            timeout_ms: cfg.decision.timeout_ms,
            fingerprint: fingerprint.clone(),
        });
    let client = if !no_llm && cfg.generative.enabled {
        let role = &cfg.generative;
        let mut client = fluent_llm::client::LlmClient::new(&role.base_url, &role.model);
        client.config.timeout_ms = role.timeout_ms;
        client.config.show_prompts = show_prompts;
        Some(client)
    } else {
        None
    };
    let code_synth = client.as_ref().map(|backend| CodeSynth { backend });
    let synth: Option<&dyn Synthesizer> = code_synth
        .as_ref()
        .map(|code_synth| code_synth as &dyn Synthesizer);
    let mut answer = answer_once(&engine, &request, &ctx, synth, judge.as_ref());
    if freshen {
        // Bounded converge loop (max 3): refreshed recall can surface
        // new stale files each round; failures keep the stale answer.
        for _ in 0..3 {
            let stale = crate::freshen::stale_citation_files(&answer, &workspace_root);
            if stale.is_empty() {
                break;
            }
            let src_dirs = src_dirs_from_config_with(&workspace_root, &cfg);
            let guidance_path = if Path::new(guidance_dir).is_absolute() {
                PathBuf::from(guidance_dir)
            } else {
                workspace_root.join(guidance_dir)
            };
            match crate::freshen::freshen_files(
                &workspace_root,
                &guidance_path,
                &db,
                &db_file,
                &src_dirs,
                &stale,
            ) {
                Ok((0, 0)) => break,
                Ok(_) => {
                    let readiness = readiness_preflight(workspace, db_path, allow_degraded);
                    let ctx = ExplainContext {
                        workspace: &workspace_root,
                        db: &db,
                        nlp,
                        coverage: Some(readiness.coverage().clone()),
                    };
                    answer = answer_once(&engine, &request, &ctx, synth, judge.as_ref());
                }
                Err(error) => {
                    answer.gaps.push(format!(
                        "freshen failed ({error}); serving indexed evidence"
                    ));
                    break;
                }
            }
        }
    }
    if compact {
        let mut compact_answer = answer.clone();
        compact_answer
            .sections
            .retain(|section| section.title != "Coverage");
        print!(
            "{}",
            guidance_core::read::render::render_answer(&compact_answer)
        );
    } else {
        print!("{}", guidance_core::read::render::render_answer(&answer));
    }
    // Strict refuses stale or unverified citations the same way the
    // readiness gate refuses stale trees: a wrong line is never
    // served quietly because a flag was set.
    if strict {
        let bad: Vec<&guidance_core::source_ref::Citation> = answer
            .citations
            .iter()
            .filter(|cite| cite.stale || cite.unverified)
            .collect();
        if !bad.is_empty() {
            eprintln!(
                "refused: {} stale or unverified citations (strict mode)",
                bad.len()
            );
            for citation in &bad {
                eprintln!("  stale: {citation}");
            }
            std::process::exit(2);
        }
    }
    let latency_ms = started.elapsed().as_millis() as u64;
    let envelope = search::envelope_json_v2(
        &format!("{:?}", answer.intent),
        &answer.route,
        &answer,
        latency_ms,
    );
    println!(
        "\n### Envelope\n```json\n{}\n```",
        serde_json::to_string_pretty(&envelope).unwrap_or_default()
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
            "decision": { "base_url": "http://localhost:8080", "model": "liquidai/d1-2b", "timeout_ms": 30000, "enabled": true, "transport": "system_one" },
            "generative": { "base_url": "http://localhost:8080", "model": "code", "timeout_ms": 30000, "enabled": true, "transport": "chat", "max_tokens": 2048, "thinking": false },
            "decision_thresholds": {},
            "calibration_registry": ".guidance/state/calibration.json"
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
async fn cmd_summarize(workspace: &str, json_dir: &str, db_path: &str) {
    let workspace_path = PathBuf::from(workspace);
    let guidance_dir = PathBuf::from(json_dir);
    let cfg = load_project_config(&workspace_path);
    let src_dirs = src_dirs_from_config_with(&workspace_path, &cfg);
    match summarize_cmd::run_summarize(
        &workspace_path,
        &guidance_dir,
        db_path,
        &src_dirs,
        &cfg,
        false,
    )
    .await
    {
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

#[allow(clippy::too_many_arguments, clippy::needless_return)]
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
    max_waves: usize,
    summarize: bool,
    embed: bool,
) -> i32 {
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
            return 0;
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
                println!("sync: clean (1 file up to date)");
                return 0;
            }
            if dry_run {
                print_sync_summary(
                    1,
                    0,
                    1,
                    &guidance_core::waves::WaveReport::default(),
                    &guidance_core::waves::ModelDraftReport::default(),
                    autonomy,
                    true,
                    no_db,
                );
                return 0;
            }
            // Fix first: the member sidecar and the index must be built
            // from the bytes that ship, not from pre-format source.
            let mut report = guidance_core::waves::WaveReport::default();
            let mut drafts = guidance_core::waves::ModelDraftReport::default();
            if source_path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                let mut policy = cfg.policy.clone();
                policy.autonomy = autonomy.to_string();
                if guidance_core::policy::autonomy_allows_fix(&policy) {
                    let _phase = profile::Phase::new("sync.fix");
                    let scope = vec![source_path.to_path_buf()];
                    let fix =
                        run_fix_scope(&scope, &guidance_dir, &cfg, &policy, max_waves, verbose);
                    report = fix.waves;
                    drafts = fix.drafts;
                }
            }
            if report.stopped.is_none()
                && report.escalated.is_empty()
                && report.package_failures.is_empty()
            {
                report.witness_clean = true;
            }
            let mut exits = report.exit_code().as_i32();
            match runtime::ast_pool()
                .submit(runtime::AstGenPayload {
                    source_path: source_path.to_path_buf(),
                    source_dir,
                    guidance_dir: guidance_dir.clone(),
                })
                .await
            {
                Ok(doc) => {
                    if verbose {
                        println!("  gen: {path} ({} members)", doc.members.len());
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
                    if !no_db {
                        let scoped = vec![guidance_core::sync_engine::absolute_or(
                            source_path.to_path_buf(),
                        )];
                        refresh_search_index(
                            &workspace_path,
                            &guidance_dir,
                            db_path,
                            &scoped,
                            verbose,
                            embed,
                        )
                        .await;
                    }
                    let mut stuck = sync_stuck_paths(&report);
                    if report.stopped.is_some() {
                        stuck.push(path.to_string());
                    }
                    if !stuck.is_empty() {
                        backdate_stuck(&guidance_dir, &src_dirs, &stuck);
                        exits = exits.max(2);
                    }
                }
                Err(ResultPoolError::Inner(e)) => {
                    eprintln!("error generating {path}: {e}");
                    exits = 2;
                }
                Err(ResultPoolError::Canceled) => {
                    eprintln!("error: pool response canceled for {path}");
                    exits = 2;
                }
                Err(ResultPoolError::Pool(e)) => {
                    eprintln!("error: pool queue {e} for {path}");
                    exits = 2;
                }
            }
            let stale = report.residual.len() + report.escalated.len();
            let indexed = usize::from(!no_db);
            print_sync_summary(1, indexed, stale, &report, &drafts, autonomy, false, no_db);
            print_fix_handoff(&report.escalated, &drafts, &report.package_failures);
            return if autonomy == "L0" { 0 } else { exits };
        }
    } else if let Some(scan_dir) = scan {
        let scan_path = PathBuf::from(scan_dir);
        if dry_run {
            println!("sync: dry run (scan {scan_dir}; no writes)");
            return 0;
        }
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
        println!("sync: scanned {scan_dir} ({generated} generated)");
        return 0;
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

        // Fix set: every stale Rust file, never a capped pre-pass. The
        // loop's own attempt budget is the only retry bound.
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

        let state_dir = guidance_dir.join("state");
        let snapshot_path = state_dir.join("queue.json");
        let mut queue = guidance_core::maintenance::MaintenanceQueue::new();
        if !dry_run && !dirty.is_empty() && autonomy != "L0" {
            if common_core::io::ensure_dir(&state_dir).is_err() {
                eprintln!("warning: state dir not writable");
            }
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
        }

        let mut report = guidance_core::waves::WaveReport::default();
        let mut drafts = guidance_core::waves::ModelDraftReport::default();
        {
            let mut policy = cfg.policy.clone();
            policy.autonomy = autonomy.to_string();
            if !dry_run && !dirty.is_empty() && guidance_core::policy::autonomy_allows_fix(&policy)
            {
                let _phase = profile::Phase::new("sync.fix");
                let fix = run_fix_scope(&dirty, &guidance_dir, &cfg, &policy, max_waves, verbose);
                report = fix.waves;
                drafts = fix.drafts;
            }
        }
        if dirty.is_empty() {
            report.witness_clean = true;
        }
        if !dry_run && !dirty.is_empty() && autonomy != "L0" {
            let mut stuck_paths = sync_stuck_paths(&report);
            if report.stopped.is_some() {
                stuck_paths.extend(dirty.iter().map(|path| path.to_string_lossy().into_owned()));
            }
            let stuck: std::collections::HashSet<String> = stuck_paths.into_iter().collect();
            for path in &dirty {
                let id = path.to_string_lossy().into_owned();
                queue.set_state(
                    &id,
                    if stuck.contains(&id) {
                        guidance_core::maintenance::WorkState::Escalated
                    } else {
                        guidance_core::maintenance::WorkState::Verified
                    },
                );
            }
            if queue.save_snapshot(&snapshot_path).is_err() {
                eprintln!("warning: queue snapshot not written");
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

            if !dry_run && (stale_files > 0 || force) {
                let _phase = profile::Phase::new("sync.walk_and_gen");
                generated += walk_and_gen_async(
                    guidance_dir.clone(),
                    src_dir.clone(),
                    src_dir.clone(),
                    force,
                    verbose,
                    &[],
                    if no_db { None } else { Some(db_path) },
                )
                .await;
            }
        }

        if verbose {
            println!("Syncing {total_files} total files ({stale_files} stale)...");
        }

        if !no_db && !dry_run {
            generated += refresh_search_index(
                &workspace_path,
                &guidance_dir,
                db_path,
                &src_dirs,
                verbose,
                embed,
            )
            .await;
        }
        // Doc→code jump funnel: derived after the index rebuilds, gated
        // on manifest staleness so converged trees pay one stat sweep
        // and no rewrite. Respects dry runs (no writes, no report).
        if !dry_run {
            match guidance_core::jumps::sync_jumps(&workspace_path, &guidance_dir) {
                Ok(report) => {
                    if verbose {
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
            match summarize_cmd::run_summarize(
                &workspace_path,
                &guidance_dir,
                db_path,
                &src_roots,
                &cfg,
                verbose,
            )
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
        let mut stuck = sync_stuck_paths(&report);
        if report.stopped.is_some() {
            stuck.extend(dirty.iter().map(|path| path.to_string_lossy().into_owned()));
            stuck.sort();
            stuck.dedup();
        }
        if !stuck.is_empty() {
            backdate_stuck(&guidance_dir, &src_dirs, &stuck);
        }
        let stale_after = stale_file_count(&src_dirs, &guidance_dir);
        print_sync_summary(
            total_files,
            generated,
            stale_after,
            &report,
            &drafts,
            autonomy,
            dry_run,
            no_db,
        );
        print_fix_handoff(&report.escalated, &drafts, &report.package_failures);
        let mut code = report.exit_code().as_i32();
        if code == 0 && stale_after > 0 {
            code = 1;
        }
        if autonomy == "L0" || dry_run {
            code = 0;
        }
        if verbose && generated > 0 {
            println!("Generated {generated} files.");
        }
        if watch {
            let watch_dirs = src_dirs_from_config(&workspace_path);
            start_watcher(
                guidance_dir,
                &watch_dirs,
                workspace_path,
                force,
                verbose,
                watch_debounce_ms,
            )
            .await;
            return 0;
        }
        return code;
    }
}

/// Fragment ingest plus dependent propagation plus node sync: the
/// derived-index half shared by workspace and single-file sync. Returns
/// regen'd files for the caller's report.
async fn refresh_search_index(
    workspace_path: &Path,
    guidance_dir: &Path,
    db_path: &str,
    src_dirs: &[PathBuf],
    verbose: bool,
    embed: bool,
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
                eprintln!("index generation unknown; rebuilding from sidecars");
                let _ = std::fs::remove_file(db_path);
                full_rebuild = true;
            }
            Some(stored) if stored != schema_now => {
                eprintln!("index schema rotated; rebuilding from sidecars");
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
                    "Ingested {} files ({} fragments, {} unchanged skipped, {} docs skipped, {} images skipped, {} failed) to {db_path}",
                    stats.files,
                    stats.fragments,
                    stats.skipped_unchanged,
                    stats.skipped_docs,
                    stats.skipped_images,
                    stats.failed
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
    {
        let _phase = profile::Phase::new("index.cards");
        import_derived_cards(workspace_path, guidance_dir, db_path, verbose);
    }
    {
        let _phase = profile::Phase::new("index.embed");
        run_embed_phase(workspace_path, db_path, verbose, embed);
    }
    let json_src = guidance_dir.join("src");
    // Node counts now derive from the fragment index (see
    // `handle_guidance_status`); the shared `nodes` document store is
    // a generic primitive with its own goldens, not on the sync path.
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

/// Embed phase for `sync`: backfill vectors for stored fragments
/// without any, reusing the wave batch/retry contract. No backend
/// means no vectors (labelled, never silent); failures warn and
/// continue — readiness reports the resulting coverage.
fn run_embed_phase(workspace_path: &Path, db_path: &str, verbose: bool, embed_flag: bool) {
    let cfg = load_project_config(workspace_path);
    // Explicit policy: off by default; `--embed` or config enables.
    // Enabling is measured on a paraphrase corpus, never a relevance gate.
    let enabled =
        embed_flag || !matches!(cfg.embed_policy, guidance_core::config::EmbedPolicy::Off);
    if !enabled {
        return;
    }
    let embedder = crate::embed::embedder_from_config(&cfg);
    if embedder.is_none() {
        return;
    }
    let db = match search_vector::GuidanceDb::open(Path::new(db_path)) {
        Ok(db) => db,
        Err(error) => {
            eprintln!("warning: embed phase skipped (cannot open index at {db_path}): {error}");
            return;
        }
    };
    let options = guidance_core::index_pipeline::IndexOptions::default();
    match guidance_core::index_pipeline::backfill_fragment_embeddings(
        &db,
        embedder.as_deref(),
        &options,
    ) {
        Ok(report) => {
            if verbose {
                println!(
                    "Embedded {} fragments ({} unchanged, {} filtered, {} failed, {} unavailable)",
                    report.embedded,
                    report.skipped_unchanged,
                    report.skipped_filtered,
                    report.failed,
                    report.unavailable
                );
            }
        }
        Err(error) => eprintln!("warning: embed phase failed: {error}"),
    }
}

/// Derived-card import for `sync`: the external capability corpus
/// plus the deterministic project cards. Both are model-free and
/// incremental; failures warn and continue, never fail the sync —
/// readiness reports the resulting coverage.
fn import_derived_cards(workspace_path: &Path, guidance_dir: &Path, db_path: &str, verbose: bool) {
    let cfg = load_project_config(workspace_path);
    let db = match search_vector::GuidanceDb::open(Path::new(db_path)) {
        Ok(db) => db,
        Err(error) => {
            eprintln!("warning: card import skipped (cannot open index at {db_path}): {error}");
            return;
        }
    };
    match guidance_core::read::doc_cards::import_capability_corpus(workspace_path, &cfg, &db) {
        Ok(report) if report.present => {
            if verbose {
                println!(
                    "Imported {} capability cards ({} ready, {} gaps, {} unchanged, {} removed)",
                    report.cards,
                    report.ready,
                    report.gaps,
                    report.skipped_unchanged,
                    report.removed
                );
            }
        }
        Ok(_) => {}
        Err(error) => eprintln!("warning: capability corpus import failed: {error}"),
    }
    let project = workspace_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace")
        .to_string();
    let jumps = guidance_core::jumps::load_jump_index(workspace_path, guidance_dir);
    match guidance_core::read::project_map::import_project_cards_with(
        workspace_path,
        &project,
        &db,
        jumps.as_ref(),
        &cfg,
    ) {
        Ok(report) => {
            if verbose {
                println!(
                    "Stored {} project cards ({} doc, {} unchanged)",
                    report.cards, report.doc_cards, report.skipped_unchanged
                );
            }
        }
        Err(error) => eprintln!("warning: project card import failed: {error}"),
    }
}

pub(crate) fn src_dirs_from_config_with(
    workspace_path: &Path,
    cfg: &config::ProjectConfig,
) -> Vec<PathBuf> {
    if cfg.src_dirs.is_empty() {
        guidance_core::read::project_map::default_source_roots(workspace_path)
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
            abs.starts_with(guidance_core::sync_engine::absolute_or(root.to_path_buf()))
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

pub(crate) fn find_source_dir(path: &Path, src_dirs: &[PathBuf]) -> Option<PathBuf> {
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
fn cmd_impact(target: &str, workspace: &str, include_fixtures: bool) {
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
    let mut report = impact_report(&graph, &changed);
    if !include_fixtures {
        let is_fixture = |path: &str| guidance_core::source_scope::is_fixture_path(path);
        report.dependents.retain(|path| !is_fixture(path));
        report.owners.retain(|(path, _)| !is_fixture(path));
        report.tests.retain(|path| !is_fixture(path));
    }
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
    } else if point == guidance_core::read::calibration::FINDING_POINT {
        let Some(fingerprint) = model_fingerprint.as_deref() else {
            eprintln!("calibrate: --model-fingerprint is required for --point finding");
            std::process::exit(2);
        };
        let rows = match guidance_core::read::calibration::load_labeled_findings(Path::new(dataset))
        {
            Ok(rows) => rows,
            Err(detail) => {
                eprintln!("calibrate: {detail}");
                std::process::exit(2);
            }
        };
        let dataset_bytes = std::fs::read(dataset).unwrap_or_default();
        let dataset_hash = common_core::hash::blake3_hex(&dataset_bytes);
        // Score the real judge (fail-closed when the endpoint is down):
        // one single-candidate judgment per row, in query order.
        let backend = fluent_llm::decision::SystemOneBackend::new(
            "http://localhost:8080",
            fluent_llm::decision::DecisionTransport::local_only(),
        );
        let judge =
            guidance_core::read::decide::FindingJudge::new(&backend, "decision".to_string(), 2000);
        let mut scores = Vec::new();
        let mut support = 0usize;
        let mut control_fires = 0usize;
        for row in &rows {
            let verdicts =
                judge.judge(&row.query, &[(row.finding_id.clone(), row.excerpt.clone())]);
            let score = verdicts.first().map_or(0.5, |v| {
                guidance_core::read::calibration::finding_label_score(&v.label)
            });
            if row.control && score >= guidance_core::read::calibration::FINDING_THRESHOLD {
                control_fires += 1;
            }
            if !row.control {
                support += 1;
            }
            scores.push(fluent_llm::decision::ScoredCase {
                score,
                expected: row.expected_relevant,
                control: row.control,
                provenance: fluent_llm::decision::CaseProvenance::human(
                    "guidance-eval",
                    "2026-11-15",
                ),
            });
        }
        let verdict = guidance_core::read::calibration::gate_scored_cases(
            &scores,
            guidance_core::read::calibration::FINDING_THRESHOLD,
            &guidance_core::read::calibration::FINDING_BARS,
        );
        println!("point: {point}");
        println!(
            "precision: {:.3} recall: {:.3} fpr: {:.3}",
            verdict.precision, verdict.recall, verdict.fpr
        );
        println!("support: {support} control fires: {control_fires}");
        if verdict.passed && control_fires == 0 {
            let corpus = guidance_core::read::calibration::finding_corpus(
                scores,
                fingerprint,
                &dataset_hash,
                dataset,
            );
            let entry = match fluent_llm::decision::RegistryEntry::calibrate_with(
                guidance_core::read::calibration::FINDING_POINT,
                fingerprint,
                guidance_core::read::calibration::FINDING_QUESTION_VERSION,
                &corpus,
                guidance_core::read::calibration::FINDING_THRESHOLD,
                None,
            ) {
                Ok(entry) => entry,
                Err(detail) => {
                    eprintln!("calibrate: {detail}");
                    std::process::exit(2);
                }
            };
            let path = Path::new(state_dir).join("calibration.json");
            match guidance_core::read::calibration::store_entry_if_passed(&path, entry, true, "") {
                Ok(guidance_core::read::calibration::StoreOutcome::Authorized { path }) => {
                    println!("verdict: AUTHORIZED (entry stored at {path})");
                }
                Ok(guidance_core::read::calibration::StoreOutcome::Held { reason }) => {
                    println!("verdict: HELD ({reason}; nothing stored)");
                }
                Err(detail) => {
                    eprintln!("calibrate: {detail}");
                    std::process::exit(2);
                }
            }
        } else {
            println!("verdict: HELD (below the bars; nothing stored)");
        }
    } else if point == guidance_core::read::calibration::SUPPORT_POINT {
        let Some(fingerprint) = model_fingerprint.as_deref() else {
            eprintln!("calibrate: --model-fingerprint is required for --point support");
            std::process::exit(2);
        };
        let _ = fingerprint;
        let rows = match guidance_core::read::calibration::load_claim_cases(Path::new(dataset)) {
            Ok(rows) => rows,
            Err(detail) => {
                eprintln!("calibrate: {detail}");
                std::process::exit(2);
            }
        };
        // Fail-closed support scores (unverified) without a support judge:
        // below the 1.0 bar by construction, so HELD without storing.
        let mut support = 0usize;
        let mut control_fires = 0usize;
        let mut scores = Vec::new();
        for row in &rows {
            let score = 0.5;
            if row.control && score >= guidance_core::read::calibration::SUPPORT_THRESHOLD {
                control_fires += 1;
            }
            if !row.control {
                support += 1;
            }
            scores.push(fluent_llm::decision::ScoredCase {
                score,
                expected: row.expected
                    == guidance_core::read::calibration::ExpectedSupport::Supported,
                control: row.control,
                provenance: fluent_llm::decision::CaseProvenance::human(
                    "guidance-eval",
                    "2026-11-15",
                ),
            });
        }
        let verdict = guidance_core::read::calibration::gate_scored_cases(
            &scores,
            guidance_core::read::calibration::SUPPORT_THRESHOLD,
            &guidance_core::read::calibration::SUPPORT_BARS,
        );
        println!("point: {point}");
        println!(
            "precision: {:.3} recall: {:.3} fpr: {:.3}",
            verdict.precision, verdict.recall, verdict.fpr
        );
        println!("support: {support} control fires: {control_fires}");
        println!("verdict: HELD (below the bars; nothing stored)");
    } else if point == guidance_core::query::decision_points::SCOPE_POINT {
        let Some(fingerprint) = model_fingerprint.as_deref() else {
            eprintln!("calibrate: --model-fingerprint is required for --point scope");
            std::process::exit(2);
        };
        let rows = match guidance_core::read::calibration::load_labeled_scopes(Path::new(dataset)) {
            Ok(rows) => rows,
            Err(detail) => {
                eprintln!("calibrate: {detail}");
                std::process::exit(2);
            }
        };
        let dataset_bytes = std::fs::read(dataset).unwrap_or_default();
        let dataset_hash = common_core::hash::blake3_hex(&dataset_bytes);
        let backend = fluent_llm::decision::SystemOneBackend::new(
            "http://localhost:8080",
            fluent_llm::decision::DecisionTransport::local_only(),
        );
        if let Err(reason) = pin_fingerprint_via_canary(&backend, fingerprint) {
            println!("point: {point}");
            println!("verdict: HELD ({reason}; nothing stored)");
            return;
        }
        // Candidate crates mirror serving: distinct expected crates,
        // sorted and truncated to the bounded option cap.
        let mut crates: Vec<String> = rows
            .iter()
            .filter(|row| !row.control)
            .map(|row| row.expected_crate.clone())
            .collect();
        crates.sort();
        crates.dedup();
        crates.truncate(10);
        let mut support = 0usize;
        let mut control_fires = 0usize;
        let mut scores = Vec::new();
        for row in &rows {
            let funnel = guidance_core::query::funnel::funnel_terms(
                &row.query,
                &search_vector::SemanticAliases::new(),
                None,
            );
            let spec = guidance_core::query::decision_points::scope_spec(&crates);
            let state = guidance_core::query::decision_points::scope_state(
                &row.query,
                &funnel.significant,
                &crates,
            );
            let prompt = fluent_llm::decision::Question {
                spec: &spec,
                state: &state,
                question: &row.query,
            }
            .render();
            let request = fluent_llm::decision::DecideRequest {
                prompt,
                state,
                question: row.query.clone(),
                options: spec
                    .options()
                    .into_iter()
                    .map(|label| fluent_llm::decision::DecisionOption {
                        label,
                        description: String::new(),
                    })
                    .collect(),
                kind: fluent_llm::decision::DecisionKind::Choice,
                version: guidance_core::query::decision_points::SCOPE_VERSION,
                timeout_ms: 2000,
                model: "decision".to_string(),
            };
            // Fail-closed: transport down scores zero, never a verdict.
            let label = {
                use fluent_llm::decision::DecideBackend;
                fluent_llm::client::block_on(backend.decide(request))
            }
            .map(|decision| decision.label)
            .unwrap_or_else(|_| "unknown".to_string());
            let score = guidance_core::read::calibration::scope_label_score(
                &label,
                &row.expected_crate,
                row.control,
            );
            if row.control && score >= guidance_core::read::calibration::SCOPE_THRESHOLD {
                control_fires += 1;
            }
            if !row.control {
                support += 1;
            }
            scores.push(fluent_llm::decision::ScoredCase {
                score,
                expected: !row.control,
                control: row.control,
                provenance: fluent_llm::decision::CaseProvenance::human(
                    "guidance-eval",
                    "2026-11-15",
                ),
            });
        }
        let verdict = guidance_core::read::calibration::gate_scored_cases(
            &scores,
            guidance_core::read::calibration::SCOPE_THRESHOLD,
            &guidance_core::read::calibration::SCOPE_BARS,
        );
        println!("point: {point}");
        println!(
            "precision: {:.3} recall: {:.3} fpr: {:.3}",
            verdict.precision, verdict.recall, verdict.fpr
        );
        println!("support: {support} control fires: {control_fires}");
        if verdict.passed && control_fires == 0 {
            let corpus = guidance_core::read::calibration::scope_corpus(
                scores,
                fingerprint,
                &dataset_hash,
                dataset,
            );
            let entry = match fluent_llm::decision::RegistryEntry::calibrate_with(
                guidance_core::query::decision_points::SCOPE_POINT,
                fingerprint,
                guidance_core::query::decision_points::SCOPE_VERSION,
                &corpus,
                guidance_core::read::calibration::SCOPE_THRESHOLD,
                None,
            ) {
                Ok(entry) => entry,
                Err(detail) => {
                    eprintln!("calibrate: {detail}");
                    std::process::exit(2);
                }
            };
            let path = Path::new(state_dir).join("calibration.json");
            match guidance_core::read::calibration::store_entry_if_passed(&path, entry, true, "") {
                Ok(guidance_core::read::calibration::StoreOutcome::Authorized { path }) => {
                    println!("verdict: AUTHORIZED (entry stored at {path})");
                }
                Ok(guidance_core::read::calibration::StoreOutcome::Held { reason }) => {
                    println!("verdict: HELD ({reason}; nothing stored)");
                }
                Err(detail) => {
                    eprintln!("calibrate: {detail}");
                    std::process::exit(2);
                }
            }
        } else {
            println!("verdict: HELD (below the bars; nothing stored)");
        }
    } else if point == guidance_core::query::decision_points::RERANK_POINT {
        let Some(fingerprint) = model_fingerprint.as_deref() else {
            eprintln!("calibrate: --model-fingerprint is required for --point rerank");
            std::process::exit(2);
        };
        let rows = match guidance_core::read::calibration::load_labeled_findings(Path::new(dataset))
        {
            Ok(rows) => rows,
            Err(detail) => {
                eprintln!("calibrate: {detail}");
                std::process::exit(2);
            }
        };
        let dataset_bytes = std::fs::read(dataset).unwrap_or_default();
        let dataset_hash = common_core::hash::blake3_hex(&dataset_bytes);
        let backend = fluent_llm::decision::SystemOneBackend::new(
            "http://localhost:8080",
            fluent_llm::decision::DecisionTransport::local_only(),
        );
        if let Err(reason) = pin_fingerprint_via_canary(&backend, fingerprint) {
            println!("point: {point}");
            println!("verdict: HELD ({reason}; nothing stored)");
            return;
        }
        // Score the real judge (fail-closed when the endpoint is down):
        // one single-candidate judgment per row, in query order.
        let judge =
            guidance_core::read::decide::FindingJudge::new(&backend, "decision".to_string(), 2000);
        let mut scores = Vec::new();
        let mut support = 0usize;
        let mut control_fires = 0usize;
        for row in &rows {
            let verdicts =
                judge.judge(&row.query, &[(row.finding_id.clone(), row.excerpt.clone())]);
            let score = verdicts.first().map_or(0.5, |v| {
                guidance_core::read::calibration::finding_label_score(&v.label)
            });
            if row.control && score >= guidance_core::read::calibration::RERANK_THRESHOLD {
                control_fires += 1;
            }
            if !row.control {
                support += 1;
            }
            scores.push(fluent_llm::decision::ScoredCase {
                score,
                expected: row.expected_relevant,
                control: row.control,
                provenance: fluent_llm::decision::CaseProvenance::human(
                    "guidance-eval",
                    "2026-11-15",
                ),
            });
        }
        let verdict = guidance_core::read::calibration::gate_scored_cases(
            &scores,
            guidance_core::read::calibration::RERANK_THRESHOLD,
            &guidance_core::read::calibration::RERANK_BARS,
        );
        println!("point: {point}");
        println!(
            "precision: {:.3} recall: {:.3} fpr: {:.3}",
            verdict.precision, verdict.recall, verdict.fpr
        );
        println!("support: {support} control fires: {control_fires}");
        if verdict.passed && control_fires == 0 {
            let corpus = guidance_core::read::calibration::rerank_corpus(
                scores,
                fingerprint,
                &dataset_hash,
                dataset,
            );
            let entry = match fluent_llm::decision::RegistryEntry::calibrate_with(
                guidance_core::query::decision_points::RERANK_POINT,
                fingerprint,
                guidance_core::query::decision_points::RERANK_VERSION,
                &corpus,
                guidance_core::read::calibration::RERANK_THRESHOLD,
                None,
            ) {
                Ok(entry) => entry,
                Err(detail) => {
                    eprintln!("calibrate: {detail}");
                    std::process::exit(2);
                }
            };
            let path = Path::new(state_dir).join("calibration.json");
            match guidance_core::read::calibration::store_entry_if_passed(&path, entry, true, "") {
                Ok(guidance_core::read::calibration::StoreOutcome::Authorized { path }) => {
                    println!("verdict: AUTHORIZED (entry stored at {path})");
                }
                Ok(guidance_core::read::calibration::StoreOutcome::Held { reason }) => {
                    println!("verdict: HELD ({reason}; nothing stored)");
                }
                Err(detail) => {
                    eprintln!("calibrate: {detail}");
                    std::process::exit(2);
                }
            }
        } else {
            println!("verdict: HELD (below the bars; nothing stored)");
        }
    } else {
        eprintln!(
            "calibrate: unknown point {point} ({TRIAGE_POINT}, {ROUTE_POINT}, {}, {}, {}, {}, {})",
            guidance_core::capability::CAPABILITY_POINT,
            guidance_core::read::calibration::FINDING_POINT,
            guidance_core::read::calibration::SUPPORT_POINT,
            guidance_core::query::decision_points::SCOPE_POINT,
            guidance_core::query::decision_points::RERANK_POINT
        );
        std::process::exit(2);
    }
}

/// Pin the model fingerprint to live weights before any run: every
/// canary case must verify, and the probed signature must equal the
/// claimed fingerprint. Drift, mismatch, or an unreachable endpoint
/// holds without storing.
fn pin_fingerprint_via_canary(
    backend: &fluent_llm::decision::SystemOneBackend,
    fingerprint: &str,
) -> Result<String, String> {
    use fluent_llm::decision::{
        canary_signature, canary_suite, verify_canary, DecideBackend, DecideRequest, DecisionKind,
        DecisionOption,
    };
    let mut probed = Vec::new();
    for case in canary_suite() {
        let request = DecideRequest {
            prompt: case.prompt.clone(),
            state: String::new(),
            question: case.prompt.clone(),
            options: case
                .options
                .iter()
                .map(|label| DecisionOption {
                    label: label.clone(),
                    description: String::new(),
                })
                .collect(),
            kind: DecisionKind::Choice,
            version: 1,
            timeout_ms: 2000,
            model: "decision".to_string(),
        };
        match fluent_llm::client::block_on(backend.decide(request)) {
            Ok(decision) => match verify_canary(&decision, &case) {
                Ok(()) => probed.push(decision),
                Err(drift) => {
                    return Err(format!(
                        "canary drift on {}: {}",
                        drift.case_id, drift.detail
                    ));
                }
            },
            Err(error) => return Err(format!("canary unreachable: {error}")),
        }
    }
    let probed_fingerprint = canary_signature(&probed);
    if probed_fingerprint != fingerprint {
        return Err(format!("fingerprint mismatch: probed {probed_fingerprint}"));
    }
    Ok(probed_fingerprint)
}

/// Preflight a read against shared readiness. Blocked state exits 3 unless
/// the caller explicitly allows a labelled degraded answer.
fn readiness_preflight(
    workspace: &str,
    db: &str,
    allow_degraded: bool,
) -> guidance_core::read::readiness::Readiness {
    let workspace_path = PathBuf::from(workspace);
    let cfg = load_project_config(&workspace_path);
    let guidance_dir = if cfg.guidance_dir.is_absolute() {
        cfg.guidance_dir.clone()
    } else {
        workspace_path.join(&cfg.guidance_dir)
    };
    let db_path = if Path::new(db).is_absolute() {
        PathBuf::from(db)
    } else {
        workspace_path.join(db)
    };
    let readiness =
        guidance_core::read::readiness::inspect(&workspace_path, &guidance_dir, &db_path, &cfg);
    match &readiness {
        guidance_core::read::readiness::Readiness::Ready { .. } => {}
        guidance_core::read::readiness::Readiness::Degraded { reasons, .. } => {
            eprintln!("degraded: {reasons:?} (use --allow-degraded to acknowledge)");
        }
        guidance_core::read::readiness::Readiness::Blocked { repairs, .. } => {
            if allow_degraded {
                eprintln!("degraded: blocked readiness allowed explicitly ({repairs:?})");
            } else {
                eprintln!("refused: read state is not trustworthy ({repairs:?})");
                for repair in repairs {
                    eprintln!("repair: {}", repair.command());
                }
                eprintln!("hint: re-run with --allow-degraded to read labelled degraded state");
                std::process::exit(3);
            }
        }
    }
    readiness
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

fn cmd_status(guidance_dir: &str, db_path: &str, json: bool) {
    let gdir = if Path::new(guidance_dir).is_absolute() {
        PathBuf::from(guidance_dir)
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(guidance_dir)
    };
    let workspace_path = std::env::current_dir().unwrap_or_default();
    let cfg = load_project_config(&workspace_path);
    let db_path = if Path::new(db_path).is_absolute() {
        PathBuf::from(db_path)
    } else {
        workspace_path.join(db_path)
    };
    let readiness = guidance_core::read::readiness::inspect(&workspace_path, &gdir, &db_path, &cfg);
    let coverage = readiness.coverage();
    let (total_files, stale_files) = match &coverage.mirror {
        guidance_core::read::readiness::MirrorState::Fresh { total } => (*total, 0usize),
        guidance_core::read::readiness::MirrorState::Stale { total, stale } => (*total, *stale),
        guidance_core::read::readiness::MirrorState::Missing => (0, 0),
    };
    let up_to_date = total_files.saturating_sub(stale_files);
    let snapshot = gdir.join("state").join("queue.json");
    let mut queue = guidance_core::maintenance::MaintenanceQueue::new();
    let counts = if queue.load_snapshot(&snapshot).is_ok() {
        queue.state_counts()
    } else {
        (0, 0, 0, 0, 0)
    };
    let fix = if counts.4 > 0 {
        "stuck"
    } else if counts.0 + counts.1 + counts.3 > 0 {
        "pending"
    } else {
        "clean"
    };
    if json {
        let out = serde_json::json!({
            "total": total_files,
            "stale": stale_files,
            "up_to_date": up_to_date,
            "clean": stale_files == 0,
            "mirror_fresh": stale_files == 0,
            "fix": fix,
            "queue": {
                "queued": counts.0,
                "in_progress": counts.1,
                "verified": counts.2,
                "failed": counts.3,
                "escalated": counts.4,
            },
            "readiness": readiness,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return;
    }
    println!("Sync Status:");
    println!("  Total files: {total_files}");
    println!("  Stale files: {stale_files}");
    println!("  Up to date:  {up_to_date}");
    println!(
        "  Mirror:      {}",
        if stale_files == 0 { "fresh" } else { "stale" }
    );
    println!("  Fix:         {fix}");
    match &readiness {
        guidance_core::read::readiness::Readiness::Ready { .. } => {
            println!("  Readiness:   ready")
        }
        guidance_core::read::readiness::Readiness::Degraded { reasons, .. } => {
            println!("  Readiness:   degraded ({reasons:?})")
        }
        guidance_core::read::readiness::Readiness::Blocked { repairs, .. } => {
            println!("  Readiness:   blocked ({repairs:?})")
        }
    }
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

/// Final stale count after all writes and backdating.
fn stale_file_count(src_dirs: &[PathBuf], guidance_dir: &Path) -> usize {
    let mut stale = 0usize;
    for src_dir in src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        let engine = SyncEngine::new(guidance_dir.to_path_buf(), src_dir.clone());
        if let Ok(status) = engine.status() {
            stale += status.stale_files;
        }
    }
    stale
}

/// Files whose sidecar must stay backdated: escalations plus every
/// residual diagnostic the final witness found.
fn sync_stuck_paths(report: &guidance_core::waves::WaveReport) -> Vec<String> {
    let mut stuck: Vec<String> = report
        .escalated
        .iter()
        .map(|esc| esc.file.clone())
        .collect();
    stuck.extend(report.residual.iter().cloned());
    stuck.sort();
    stuck.dedup();
    stuck
}

fn print_fix_handoff(
    escalated: &[guidance_core::waves::Escalation],
    drafts: &guidance_core::waves::ModelDraftReport,
    package_failures: &[guidance_core::waves::PackageFailure],
) {
    if escalated.is_empty() && package_failures.is_empty() {
        return;
    }
    let files = escalated.len();
    if files > 0 {
        println!(
            "sync: {} {} attention (fix loop did not converge; drafts kept {})",
            files,
            if files == 1 {
                "file needs"
            } else {
                "files need"
            },
            drafts.kept
        );
    }
    for esc in escalated {
        println!("  {} ({})", esc.file, esc.reason);
        if !esc.detail.is_empty() {
            for line in esc.detail.lines() {
                println!("    {line}");
            }
        }
        println!("    fingerprint: {}", esc.fingerprint);
        for (file, detail) in &drafts.failures {
            if file == &esc.file {
                println!("    model draft: {detail}");
            }
        }
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for failure in package_failures {
        if !seen.insert(failure.detail.trim().to_string()) {
            continue;
        }
        println!("  package {} (compile failed)", failure.package);
        for line in failure.detail.lines() {
            println!("    {line}");
        }
    }
}

/// One decisive summary for humans and agents. The counts are the
/// contract; `--verbose` adds the per-stage progress lines.
#[allow(clippy::too_many_arguments)]
/// One decisive summary for humans and agents. The counts are the
/// contract; `--verbose` adds the per-stage progress lines.
fn print_sync_summary(
    files: usize,
    generated: usize,
    stale: usize,
    report: &guidance_core::waves::WaveReport,
    drafts: &guidance_core::waves::ModelDraftReport,
    autonomy: &str,
    dry_run: bool,
    no_db: bool,
) {
    if dry_run {
        println!("sync: dry run ({files} files, {stale} stale; no writes)");
        return;
    }
    if autonomy == "L0" {
        println!("sync: index only (autonomy L0; {files} files, {stale} stale; fixes skipped)");
        return;
    }
    let noun = if files == 1 { "file" } else { "files" };
    let clean = report.fixed == 0
        && report.escalated.is_empty()
        && report.package_failures.is_empty()
        && report.stopped.is_none()
        && report.witness_clean;
    if clean {
        println!(
            "sync: clean ({files} {noun}, {generated} indexed, {stale} stale{})",
            if no_db { ", index skipped" } else { "" }
        );
        return;
    }
    let lint = report
        .fixed
        .saturating_sub(report.formatted + report.compile_fixed);
    println!(
        "sync: {files} {noun}, {} fixed (format {}, lint {}, compile {}), {} escalated, {} package failures, {generated} indexed, {stale} stale{}",
        report.fixed,
        report.formatted,
        lint,
        report.compile_fixed,
        report.escalated.len(),
        report.package_failures.len(),
        if no_db { ", index skipped" } else { "" }
    );
    if drafts.skipped > 0 {
        println!(
            "model drafts: {} skipped (autonomy {autonomy})",
            drafts.skipped
        );
    }
    if drafts.kept > 0 {
        println!(
            "model drafts: {} kept, {} failed, {} skipped",
            drafts.kept, drafts.failed, drafts.skipped
        );
    }
    println!(
        "witness: {}",
        if report.witness_clean {
            "clean"
        } else {
            "not clean"
        }
    );
}

/// Fix-scope outcome: mechanical waves plus the model-draft rung.
struct FixScopeOutcome {
    waves: guidance_core::waves::WaveReport,
    drafts: guidance_core::waves::ModelDraftReport,
}

/// Lint, fix, and format `files` before indexing: rustfmt plus the
/// mechanical clippy rungs under the policy attempt budget. Model drafts
/// are L3-only, so L2 stays deterministic. Fix always precedes member
/// gen and indexing on the same bytes.
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
    let registry_path =
        guidance_core::config::calibration_path(cfg, guidance_dir.parent().unwrap_or(guidance_dir));
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
    } else if !guidance_core::policy::autonomy_allows_model_drafts(policy) {
        guidance_core::waves::ModelDraftReport {
            skipped: escalated.len(),
            ..guidance_core::waves::ModelDraftReport::default()
        }
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
    let guidance_path = if guidance_dir.is_absolute() {
        guidance_dir.clone()
    } else {
        workspace_path.join(&guidance_dir)
    };
    let db_path = if Path::new(db).is_absolute() {
        PathBuf::from(db)
    } else {
        workspace_path.join(db)
    };
    let readiness =
        guidance_core::read::readiness::inspect(&workspace_path, &guidance_path, &db_path, &cfg);
    let coverage = readiness.coverage();
    let (files, stale) = match &coverage.mirror {
        guidance_core::read::readiness::MirrorState::Fresh { total } => (*total, 0usize),
        guidance_core::read::readiness::MirrorState::Stale { total, stale } => (*total, *stale),
        guidance_core::read::readiness::MirrorState::Missing => (0, 0),
    };
    let _ = src_dirs;
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
        "readiness": readiness,
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
