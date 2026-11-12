//! P2 incremental index pipeline: scan → diff → prepare → embed →
//! commit (port of `pipeline/indexing/index.ts`). Per-file isolation
//! (failure is stored data), bounded waves as `SupervisedBatch` units,
//! cross-file embed packing with one-by-one fallback, content-hash
//! embedding cache, failed-retry-once, vector-count commit validation.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use common_core::metrics::LatencyHistogram;
use common_core::retry::backoff_ms;
use fluent_concurrency::batch::SupervisedBatch;
use fluent_concurrency::runtime::tokio::TokioRuntime;
use fluent_dag::dep_graph::DependencyGraph;
use fluent_llm::embeddings::{
    BatchEmbedding, CachedEmbeddingProvider, EmbeddingError, EmbeddingProvider,
};
use fluent_wvr::{
    impl_component, Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput,
    WorkUnit,
};
use internment::ArcIntern;
use search_vector::db::GuidanceDb;

use crate::ast_parser::{AstParser, CommentBlock};
use crate::diff::{compute_diff, hash_file, make_file_id, ScannedFile};
use crate::extractor::code::{module_fragment_for, CodeExtractor, HarvestedFile};
use crate::extractor::{ChunkOptions, ExtractSource, FragmentMetadata, PreparedFragment};
use crate::query::writer::{scanned_record, FragmentWriter, VectorPolicy};
use crate::search_types::FileKind;
use crate::selection::{select_files, FileSelection, ScanDiagnostics, SelectedFile};

/// Files per bounded wave (named constant, tuned in P6).
pub const INDEX_WAVE_FILES: usize = 64;
/// Fragment-index schema version enforced by the `SyncEngine` gate.
/// v5 stores item chunks from both writers: old whole-file live rows are
/// refused and rebuilt from sidecars.
pub const INDEX_SCHEMA_VERSION: u32 = 5;
/// Default cross-file embed packing size.
pub const INDEX_EMBED_BATCH: usize = 32;
/// Transient embed retry budget (zvec C.4: 3×500ms).
pub const EMBED_RETRY_ATTEMPTS: u32 = 3;
/// Transient embed retry base (ms).
pub const EMBED_RETRY_BASE_MS: u64 = 500;
/// Retry backoff jitter (percent).
pub const EMBED_RETRY_JITTER_PCT: u32 = 50;

/// Pipeline options.
#[derive(Debug, Clone)]
/// Pipeline options.
pub struct IndexOptions {
    /// Cross-file embed packing size.
    pub max_embed_batch: usize,
    /// Files per bounded wave.
    pub wave_files: usize,
    /// Embed retry attempts (transient errors).
    pub retry_max_attempts: u32,
    /// Embed retry base backoff (ms).
    pub retry_base_ms: u64,
    /// Rebuild: drop all stored records, index everything as added.
    pub rebuild: bool,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            max_embed_batch: INDEX_EMBED_BATCH,
            wave_files: INDEX_WAVE_FILES,
            retry_max_attempts: EMBED_RETRY_ATTEMPTS,
            retry_base_ms: EMBED_RETRY_BASE_MS,
            rebuild: false,
        }
    }
}

/// Progress report (phase + monotonic counts).
#[derive(Debug, Clone, PartialEq, Eq)]
/// Progress report (phase + monotonic counts).
pub struct IndexProgress {
    /// `scanning` | `embedding` | `retry`.
    pub phase: &'static str,
    /// Files committed so far.
    pub files_indexed: usize,
    /// Total files in the run.
    pub files_total: usize,
    /// Files failed so far.
    pub files_failed: usize,
    /// Human-readable detail.
    pub detail: String,
}

/// Index run counts (mirrors zvec `IndexStats` + `IndexResult`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Index run counts (mirrors zvec `IndexStats` + `IndexResult`).
pub struct IndexStats {
    /// Files seen by the scan.
    pub files_scanned: usize,
    /// Diff buckets.
    pub files_added: usize,
    /// Diff buckets.
    pub files_modified: usize,
    /// Diff buckets.
    pub files_pending: usize,
    /// Diff buckets.
    pub files_deleted: usize,
    /// Diff buckets.
    pub files_unchanged: usize,
    /// Files committed in this run.
    pub files_indexed: usize,
    /// Files failed in this run.
    pub files_failed: usize,
    /// Failed relative paths.
    pub failed_files: Vec<String>,
    /// Fragments committed.
    pub entities_created: usize,
    /// Image files discovered but not embedded (G0.5).
    pub image_skipped: usize,
    /// Fragments truncated by the model (P4a providers report these).
    pub truncated_fragment_count: usize,
    /// Clean files reprocessed without an invalidation cause. Converged
    /// trees read zero; anything else trips the ratchet.
    pub reprocessed_clean: usize,
}

/// Index failure (stored per-file data surfaces as `FilesFailed`; the run
/// still commits every sibling first).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Index failure (stored per-file data surfaces as `FilesFailed`; the run
/// still commits every sibling first).
pub enum IndexError {
    /// Files failed after the automatic retry (context names files +
    /// reasons, per `:1042`).
    #[error("{0}")]
    FilesFailed(String),
    /// Fail-fast embedding error (auth/config): the run aborts at once.
    #[error("fail-fast embedding error: {0}")]
    FailFast(String),
    /// Database failure.
    #[error("database error: {0}")]
    Db(String),
    /// Cancellation requested.
    #[error("index cancelled")]
    Cancelled,
}

/// Stable wave-unit name.
#[must_use]
/// Stable wave-unit name.
pub fn wave_name(index: usize) -> String {
    format!("index-wave-{index}")
}

/// Run the full index pass over `selection`.
///
/// The provider is wrapped in a content-hash `CachedEmbeddingProvider`
/// (unchanged content never re-embeds); unchanged files never reach the
/// embedder at all (diff fast path). Synchronous data plane; async
/// callers drive it from `spawn_blocking`.
#[allow(clippy::too_many_arguments)]
/// Run the full index pass over `selection`.
///
/// The provider is wrapped in a content-hash `CachedEmbeddingProvider`
/// (unchanged content never re-embeds); unchanged files never reach the
/// embedder at all (diff fast path). Synchronous data plane; async
/// callers drive it from `spawn_blocking`.
pub fn index_workspace<P>(
    db: Arc<GuidanceDb>,
    provider: P,
    selection: &FileSelection,
    options: &IndexOptions,
    nlp: Option<Arc<spacy_rs::pipeline::NlpPipeline>>,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(IndexProgress),
) -> Result<IndexStats, IndexError>
where
    P: EmbeddingProvider + Sync + 'static,
{
    index_workspace_with_components(
        db,
        provider,
        selection,
        options,
        nlp,
        cancel,
        on_progress,
        &crate::components::ComponentVector::default(),
    )
}

/// Index with an explicit input-component vector: follow-up reuse
/// binds symbol signatures plus these components in one work-key
/// comparison. Default callers pass an empty vector, which decides
/// exactly like the pure signature gate.
pub fn index_workspace_with_components<P>(
    db: Arc<GuidanceDb>,
    provider: P,
    selection: &FileSelection,
    options: &IndexOptions,
    nlp: Option<Arc<spacy_rs::pipeline::NlpPipeline>>,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(IndexProgress),
    components: &crate::components::ComponentVector,
) -> Result<IndexStats, IndexError>
where
    P: EmbeddingProvider + Sync + 'static,
{
    let mut stats = IndexStats::default();
    let mut diag = ScanDiagnostics::default();
    let selected = select_files(selection, &mut diag).map_err(|e| IndexError::Db(e.to_string()))?;
    let scanned: Vec<ScannedFile> = selected.iter().map(scan_selected).collect();
    stats.files_scanned = scanned.len();

    let mut existing = db.list_files().map_err(|e| IndexError::Db(e.to_string()))?;
    if options.rebuild {
        for file in &existing {
            db.delete_file(&file.id)
                .map_err(|e| IndexError::Db(e.to_string()))?;
        }
        existing.clear();
    }
    let diff = compute_diff(&scanned, &existing);
    stats.files_added = diff.added.len();
    stats.files_modified = diff.modified.len();
    stats.files_pending = diff.pending.len();
    stats.files_unchanged = diff.unchanged.len();

    // Scoped runs only delete stored files under the selection scope:
    // out-of-scope files are untouched, never reaped (P3 reconcile).
    let by_id: HashMap<&str, &search_vector::db::FileRecord> = existing
        .iter()
        .map(|file| (file.id.as_str(), file))
        .collect();
    let in_scope = |id: &str| {
        by_id.get(id).is_some_and(|record| {
            let path = Path::new(&record.absolute_path);
            selection
                .roots
                .iter()
                .any(|root| path == root || path.starts_with(root))
        })
    };
    for id in diff.deleted.iter().filter(|id| in_scope(id)) {
        db.delete_file(id)
            .map_err(|e| IndexError::Db(e.to_string()))?;
        stats.files_deleted += 1;
    }

    let bytes_changed: std::collections::HashSet<String> = diff
        .added
        .iter()
        .chain(diff.modified.iter())
        .map(|file| file.id.clone())
        .collect();
    let mut work: Vec<ScannedFile> = diff
        .added
        .into_iter()
        .chain(diff.modified)
        .chain(diff.pending)
        .collect();
    // Embedder rotation backfill: files whose recorded embedder differs
    // from the current one re-ingest even though their bytes are unchanged.
    // Content staleness already covers input changes; unknown (status-less)
    // files are never forced.
    let backfilled: std::collections::HashSet<String> = {
        let embedder = format!("{}:{}", provider.name(), provider.dimensions());
        let rotated = db
            .files_needing_embed_backfill(&embedder)
            .unwrap_or_default();
        let by_id: HashMap<&str, &ScannedFile> = scanned
            .iter()
            .map(|file| (file.id.as_str(), file))
            .collect();
        let mut forced = std::collections::HashSet::new();
        for id in rotated {
            if let Some(file) = by_id.get(id.as_str()) {
                if !work.iter().any(|scheduled| scheduled.id == id) {
                    work.push((*file).clone());
                }
                forced.insert(id);
            }
        }
        forced
    };
    // Clean files scheduled without an invalidation cause trip the
    // ratchet: indexed files whose content hash already matches. Pending
    // retries carry their failure as cause and never count, and neither do
    // embedder-rotation backfills (their cause is the rotated embedder).
    for file in &work {
        let clean = by_id.get(file.id.as_str()).is_some_and(|stored| {
            stored.content_hash.is_some()
                && stored.content_hash == file.content_hash
                && stored
                    .index_status
                    .as_deref()
                    .is_some_and(|s| s != "failed")
        });
        if clean && !backfilled.contains(&file.id) {
            stats.reprocessed_clean += 1;
        }
    }
    // Deterministic file order via the dependency graph (P2 registers file
    // nodes; P3 adds symbol edges; cancellation rides `dependents_of`).
    let mut graph = DependencyGraph::<String>::new();
    for file in &work {
        let _ = graph.register(&file.id, &[], std::slice::from_ref(&file.id));
    }
    let order = graph
        .topo_sort()
        .map_err(|e| IndexError::Db(e.to_string()))?;
    let position: HashMap<&str, usize> = order.iter().map(String::as_str).zip(0..).collect();
    work.sort_by_key(|file| {
        position
            .get(file.id.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });

    let total = work.len();
    // Stored identity hashes per scheduled file, read before waves
    // commit fresh rows: the sig-gated follow-up compares against this.
    let mut before: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for file in &work {
        if let Ok(rows) = db.symbols_for_file(&file.id) {
            before.insert(
                file.id.clone(),
                rows.iter()
                    // Module fragments carry comments, never interfaces.
                    .filter(|row| row.name.as_deref() != Some("$file"))
                    .map(|row| (row.symbol_id.clone(), row.interface_hash.clone()))
                    .collect(),
            );
        }
    }
    // Stored work keys per scheduled file: the follow-up reuses a file
    // whose key (signatures plus consumed components) still matches, so
    // over-approximate syntactic edges cost work only on real change.
    // Pairs frame as `id=hash` so a rename never collides as a bare
    // multiset; sorting keeps the comparison order-insensitive like the
    // signature gate below.
    let mut work_keys: HashMap<String, String> = HashMap::new();
    for file in &work {
        if let Some(rows) = before.get(&file.id) {
            let framed: Vec<String> = rows
                .iter()
                .map(|(id, hash)| format!("{id}={hash}"))
                .collect();
            let sigs: Vec<&str> = framed.iter().map(String::as_str).collect();
            work_keys.insert(
                file.id.clone(),
                crate::item_hashes::work_key(&file.id, &sigs, &components.components),
            );
        }
    }
    let shared = Arc::new(SharedPipeline {
        db,
        provider: Arc::new(CachedEmbeddingProvider::new(provider)),
        nlp,
        options: options.clone(),
        histogram_embed: Arc::new(LatencyHistogram::new()),
        histogram_commit: Arc::new(LatencyHistogram::new()),
        histogram_wave: Arc::new(LatencyHistogram::new()),
    });

    let mut failed: Vec<(ScannedFile, String)> = Vec::new();
    for (wave_index, chunk) in work
        .chunks(options.wave_files.max(1))
        .map(Vec::from)
        .enumerate()
    {
        if cancel.load(Ordering::SeqCst) {
            return Err(IndexError::Cancelled);
        }
        let depends = (wave_index > 0).then(|| ArcIntern::from(wave_name(wave_index - 1)));
        let failed_count = failed.len();
        run_wave(
            &wave_name(wave_index),
            depends,
            chunk,
            Arc::clone(&shared),
            total,
            &mut stats,
            &mut failed,
            failed_count,
            on_progress,
        )?;
    }

    // Failed-retry-once: one automatic second pass over failures.
    // `failed` stays intact until the retry completes so progress counts
    // stay monotonic (a recovered file simply drops out after).
    if !failed.is_empty() {
        on_progress(IndexProgress {
            phase: "scanning",
            files_indexed: stats.files_indexed,
            files_total: total,
            files_failed: failed.len(),
            detail: format!("retrying {} failed files (pass 2)", failed.len()),
        });
        let retry_files: Vec<ScannedFile> = failed.iter().map(|(file, _)| file.clone()).collect();
        let mut retry_failed = Vec::new();
        run_wave(
            "index-wave-retry",
            None,
            retry_files,
            Arc::clone(&shared),
            total,
            &mut stats,
            &mut retry_failed,
            failed.len(),
            on_progress,
        )?;
        failed = retry_failed;
        stats.files_failed = failed.len();
    }

    // Sig-gated follow-up: interfaces committed above may move
    // dependents, macro definitions widen to their package, and
    // dependents of failures re-verify against missing symbols. Only
    // never-scheduled files join; visited-set growth always terminates
    // the loop. Follow-up failures land in the final failed list with
    // no second retry.
    {
        use crate::invalidation::{dependents_of, file_sig_changed, macro_expansion};
        let mut scheduled: std::collections::HashSet<String> =
            work.iter().map(|file| file.id.clone()).collect();
        let mut followup_n = 0usize;
        loop {
            let edges: Vec<(String, String, Option<String>)> = shared
                .db
                .graph_edge_rows()
                .map(|rows| {
                    rows.iter()
                        .map(|row| {
                            (
                                row.importer.clone(),
                                row.specifier.clone(),
                                row.resolved.clone(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut sig_moved: Vec<String> = Vec::new();
            let mut macro_moved: Vec<String> = Vec::new();
            for file in &work {
                let fresh: Vec<(String, String)> = shared
                    .db
                    .symbols_for_file(&file.id)
                    .map(|rows| {
                        rows.iter()
                            .filter(|row| row.name.as_deref() != Some("$file"))
                            .map(|row| (row.symbol_id.clone(), row.interface_hash.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                if file_sig_changed(before.get(&file.id).map_or(&[], Vec::as_slice), &fresh) {
                    // The pure signature gate fired; confirm with the
                    // component-bound work key so a matching key (same
                    // signatures, same consumed inputs) still reuses.
                    let framed: Vec<String> = fresh
                        .iter()
                        .map(|(id, hash)| format!("{id}={hash}"))
                        .collect();
                    let sigs: Vec<&str> = framed.iter().map(String::as_str).collect();
                    let reusable = work_keys.get(&file.id).is_some_and(|stored| {
                        crate::item_hashes::work_key_matches(
                            &file.id,
                            &sigs,
                            &components.components,
                            Some(stored),
                        )
                    });
                    if !reusable {
                        sig_moved.push(file.absolute_path.clone());
                    }
                }
                if bytes_changed.contains(&file.id)
                    && common_core::io::read_to_string_err(Path::new(&file.absolute_path))
                        .as_deref()
                        .ok()
                        .is_some_and(crate::invalidation::defines_macros)
                {
                    macro_moved.push(file.absolute_path.clone());
                }
                before.insert(file.id.clone(), fresh.clone());
                let framed: Vec<String> = fresh
                    .iter()
                    .map(|(id, hash)| format!("{id}={hash}"))
                    .collect();
                let sigs: Vec<&str> = framed.iter().map(String::as_str).collect();
                work_keys.insert(
                    file.id.clone(),
                    crate::item_hashes::work_key(&file.id, &sigs, &components.components),
                );
            }
            let failed_abs: Vec<String> = failed
                .iter()
                .map(|(file, _)| file.absolute_path.clone())
                .collect();
            let scanned_roots: Vec<(String, String)> = scanned
                .iter()
                .map(|file| (file.absolute_path.clone(), file.root_path.clone()))
                .collect();
            let mut wanted: Vec<String> = dependents_of(&edges, &sig_moved);
            wanted.extend(macro_expansion(&scanned_roots, &macro_moved));
            wanted.extend(dependents_of(&edges, &failed_abs));
            wanted.sort();
            wanted.dedup();
            let fresh: Vec<ScannedFile> = scanned
                .iter()
                .filter(|file| {
                    wanted.contains(&file.absolute_path) && !scheduled.contains(&file.id)
                })
                .cloned()
                .collect();
            if fresh.is_empty() {
                break;
            }
            for file in &fresh {
                scheduled.insert(file.id.clone());
                // Baseline seeded pre-commit: the next comparison sees
                // genuine old rows, never an empty adopt-and-flag.
                if !before.contains_key(&file.id) {
                    if let Ok(rows) = shared.db.symbols_for_file(&file.id) {
                        let pairs: Vec<(String, String)> = rows
                            .iter()
                            .map(|row| (row.symbol_id.clone(), row.interface_hash.clone()))
                            .collect();
                        let framed: Vec<String> = pairs
                            .iter()
                            .map(|(id, hash)| format!("{id}={hash}"))
                            .collect();
                        let sigs: Vec<&str> = framed.iter().map(String::as_str).collect();
                        work_keys.insert(
                            file.id.clone(),
                            crate::item_hashes::work_key(&file.id, &sigs, &components.components),
                        );
                        before.insert(file.id.clone(), pairs);
                    }
                }
            }
            followup_n += 1;
            let mut followup_failed = Vec::new();
            run_wave(
                &format!("index-wave-followup-{followup_n}"),
                None,
                fresh,
                Arc::clone(&shared),
                total,
                &mut stats,
                &mut followup_failed,
                failed.len(),
                on_progress,
            )?;
            failed.append(&mut followup_failed);
            stats.files_failed = failed.len();
        }
    }

    for (file, reason) in &failed {
        shared
            .db
            .mark_file_failed(&scanned_record(file), reason)
            .map_err(|e| IndexError::Db(e.to_string()))?;
        stats.failed_files.push(file.relative_path.clone());
    }
    if !failed.is_empty() {
        let mut context = format!(
            "Retried failed files once automatically: failedFiles={}",
            stats.failed_files.join(",")
        );
        for (file, reason) in &failed {
            use std::fmt::Write as _;
            let _ = write!(context, " [{}: {reason}]", file.relative_path);
        }
        return Err(IndexError::FilesFailed(context));
    }
    Ok(stats)
}

fn scan_selected(selected: &SelectedFile) -> ScannedFile {
    let absolute = selected.path.to_string_lossy().to_string();
    ScannedFile {
        id: make_file_id(&absolute),
        absolute_path: absolute,
        relative_path: selected.relative.clone(),
        root_path: selected.root.to_string_lossy().to_string(),
        size_bytes: selected.size_bytes,
        last_modified_time: selected.modified_ms,
        kind: Some(kind_name(selected.kind).to_string()),
        format: selected.format.clone(),
        content_hash: hash_file(&selected.path),
    }
}

pub(crate) fn kind_name(kind: FileKind) -> &'static str {
    kind.as_str()
}

/// Parse a stored kind tag back to a `FileKind` (ingest direction:
/// unknown tags — including `"image"` — fall back to `Text`).
pub(crate) fn parse_scan_kind(kind: Option<&str>) -> FileKind {
    FileKind::parse_scan(kind)
}

#[allow(clippy::too_many_arguments)]
fn run_wave<P>(
    name: &str,
    depends: Option<ArcIntern<str>>,
    files: Vec<ScannedFile>,
    shared: Arc<SharedPipeline<P>>,
    total: usize,
    stats: &mut IndexStats,
    failed: &mut Vec<(ScannedFile, String)>,
    failed_count: usize,
    on_progress: &mut dyn FnMut(IndexProgress),
) -> Result<(), IndexError>
where
    P: EmbeddingProvider + Sync + 'static,
{
    on_progress(IndexProgress {
        phase: "embedding",
        files_indexed: stats.files_indexed,
        files_total: total,
        files_failed: failed_count,
        detail: format!("{name} ({} files)", files.len()),
    });
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let wave_histogram = Arc::clone(&shared.histogram_wave);
    let unit = IndexWaveUnit {
        name: name.to_string(),
        depends: depends.into_iter().collect(),
        provides: vec![ArcIntern::from(name)],
        files,
        shared,
        outcomes: Arc::clone(&outcomes),
    };
    // N.wvr: observability wraps the concrete unit before type erasure.
    let instrumented = fluent_wvr::wrapper::Instrumented::with_metrics(unit, name, wave_histogram);
    // Multi-thread runtime: shared provider seams (e.g. the embedding
    // cache) may `block_in_place`; waves still run sequentially.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| IndexError::Db(e.to_string()))?;
    let summary = runtime.block_on(async {
        let mut batch =
            SupervisedBatch::new(Arc::new(TokioRuntime), fluent_wvr::CapabilitySet::new());
        batch
            .register(Arc::new(instrumented))
            .expect("wave unit registers");
        batch.await
    });
    // Fail-fast aborts surface from the outcomes (the unit records the
    // abort before failing); infrastructure failures surface from the
    // batch summary.
    let guard = common_core::sync::lock(&outcomes);
    if let Some(reason) = guard.iter().find_map(|outcome| match outcome {
        FileOutcome::Abort { reason } => Some(reason.clone()),
        _ => None,
    }) {
        return Err(IndexError::FailFast(reason));
    }
    drop(guard);
    if !summary.failed.is_empty() || !summary.panicked.is_empty() {
        return Err(IndexError::Db(format!(
            "wave {name} failed: {} failed, {} panicked",
            summary.failed.len(),
            summary.panicked.len()
        )));
    }
    let mut outcomes_guard = common_core::sync::lock(&outcomes);
    let mut drained: Vec<FileOutcome> = outcomes_guard.drain(..).collect();
    drop(outcomes_guard);
    for outcome in drained.drain(..) {
        match outcome {
            FileOutcome::Indexed { fragments } => {
                stats.files_indexed += 1;
                stats.entities_created += fragments;
            }
            FileOutcome::ImageSkipped => {
                stats.image_skipped += 1;
                stats.files_indexed += 1;
            }
            FileOutcome::Failed { file, reason } => failed.push((file, reason)),
            FileOutcome::Abort { reason } => {
                return Err(IndexError::FailFast(reason));
            }
        }
    }
    stats.files_failed = failed.len();
    on_progress(IndexProgress {
        phase: "embedding",
        files_indexed: stats.files_indexed,
        files_total: total,
        files_failed: failed.len(),
        detail: format!("{name} done"),
    });
    Ok(())
}

/// Per-file wave outcome (collected under a mutex; siblings continue).
enum FileOutcome {
    Indexed {
        fragments: usize,
    },
    ImageSkipped,
    Failed {
        file: ScannedFile,
        reason: String,
    },
    /// Fail-fast abort: stop scheduling at once (no retry, no second pass).
    Abort {
        reason: String,
    },
}

struct SharedPipeline<P: EmbeddingProvider> {
    db: Arc<GuidanceDb>,
    provider: Arc<CachedEmbeddingProvider<P>>,
    nlp: Option<Arc<spacy_rs::pipeline::NlpPipeline>>,
    options: IndexOptions,
    histogram_embed: Arc<LatencyHistogram>,
    histogram_commit: Arc<LatencyHistogram>,
    histogram_wave: Arc<LatencyHistogram>,
}

/// One bounded wave as a `Component` (N.wvr: concrete unit over the
/// provider type; hot loops monomorphized).
struct IndexWaveUnit<P: EmbeddingProvider> {
    name: String,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
    files: Vec<ScannedFile>,
    shared: Arc<SharedPipeline<P>>,
    outcomes: Arc<Mutex<Vec<FileOutcome>>>,
}

impl<P: EmbeddingProvider> WorkUnit for IndexWaveUnit<P>
where
    P: Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let mut parser = AstParser::new();
        // Prepare phase: every file to fragments + embedding texts.
        let outcomes = Arc::clone(&self.outcomes);
        let mut ready = Vec::new();
        for (index, file) in self.files.iter().enumerate() {
            match prepare_one_file(&mut parser, &self.shared, index, file) {
                PrepareOutcome::Ready(prepared) => ready.push(prepared),
                PrepareOutcome::Done(outcome) => {
                    common_core::sync::lock(&outcomes).push(outcome);
                }
            }
        }
        // Embed phase: cross-file packing (zvec batch parity) with
        // per-file / per-fragment fallback (file + one-by-one parity).
        // A per-file embed failure degrades that file to unembedded rows
        // (recorded in the status table) instead of failing the wave:
        // retries happen on input or embedder change, never in a loop.
        let (mut by_index, failures) = match embed_ready_files(&self.shared, &ready) {
            Ok((ok, failed)) => (ok.into_iter().collect::<HashMap<_, _>>(), failed),
            Err(reason) => {
                common_core::sync::lock(&outcomes).push(FileOutcome::Abort { reason });
                return Err(WorkError::Execution(format!("{} aborted", self.name)));
            }
        };
        let failures: HashMap<usize, EmbedFileFailure> = failures.into_iter().collect();
        // Commit phase: per-file replace + vector-count validation.
        for prepared in &ready {
            if common_core::sync::lock(&outcomes)
                .iter()
                .any(|outcome| match outcome {
                    FileOutcome::Failed { file, .. } => file.id == prepared.file.id,
                    _ => false,
                })
            {
                continue;
            }
            let outcome = match by_index.remove(&prepared.index) {
                Some(vectors) => commit_prepared(&self.shared, prepared, &vectors),
                None => match failures.get(&prepared.index) {
                    Some(failure) => commit_degraded(&self.shared, prepared, failure),
                    None => FileOutcome::Failed {
                        file: prepared.file.clone(),
                        reason: "missing vectors without embed failure".to_string(),
                    },
                },
            };
            common_core::sync::lock(&outcomes).push(outcome);
        }
        Ok(WorkOutput::ok(format!("{} done", self.name)))
    }
}

impl<P: EmbeddingProvider> FieldAccess for IndexWaveUnit<P> {
    fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound("read-only".into()))
    }
    fn get_field(&self, _: &str) -> Result<String, FieldError> {
        Err(FieldError::NotFound("read-only".into()))
    }
    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}

impl<P: EmbeddingProvider> Describable for IndexWaveUnit<P> {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({ "wave": self.name, "files": self.files.len() })
    }
}

impl_component!(generic (P: fluent_llm::embeddings::EmbeddingProvider + 'static) for IndexWaveUnit<P>);

/// A prepared file awaiting embedding.
struct PreparedFile {
    /// Position in the wave's ready list.
    index: usize,
    file: ScannedFile,
    fragments: Vec<PreparedFragment>,
    /// Comment scopes from the same extract pass, mapped to fragments at commit.
    blocks: Vec<CommentBlock>,
    texts: Vec<String>,
    /// Graph inputs harvested from the same extract pass (no second parse).
    harvest: HarvestedFile,
}

enum PrepareOutcome {
    Ready(PreparedFile),
    Done(FileOutcome),
}

fn prepare_one_file<P>(
    parser: &mut AstParser,
    shared: &SharedPipeline<P>,
    index: usize,
    file: &ScannedFile,
) -> PrepareOutcome
where
    P: EmbeddingProvider + Sync + 'static,
{
    if file.kind.as_deref() == Some("image") {
        // Images are discovered and kind-tagged with an (empty) stored
        // record, never embedded (G0.5); the record keeps re-runs clean.
        // Graph rows clear with the same replace semantic as fragments.
        let record = scanned_record(file);
        return PrepareOutcome::Done(match shared.db.replace_file(&record, &[], &[]) {
            Ok(_) => match shared.db.replace_file_graph(&file.absolute_path, &[], &[]) {
                Ok(()) => FileOutcome::ImageSkipped,
                Err(e) => FileOutcome::Failed {
                    file: file.clone(),
                    reason: format!("graph: {e}"),
                },
            },
            Err(e) => FileOutcome::Failed {
                file: file.clone(),
                reason: format!("commit: {e}"),
            },
        });
    }
    let text = match common_core::io::read_to_string_err(Path::new(&file.absolute_path)) {
        Ok(text) => text,
        Err(e) => {
            return PrepareOutcome::Done(FileOutcome::Failed {
                file: file.clone(),
                reason: format!("read: {e}"),
            });
        }
    };
    let kind = parse_scan_kind(file.kind.as_deref());
    let source = ExtractSource {
        file_id: file.id.clone(),
        text,
        format: file.format.clone(),
        kind,
    };
    let chunk_options = match ChunkOptions::resolve(None, None) {
        Ok(options) => options,
        Err(e) => {
            return PrepareOutcome::Done(FileOutcome::Failed {
                file: file.clone(),
                reason: format!("prepare: {e}"),
            });
        }
    };
    let (mut fragments, harvest, blocks) = match prepare_fragments(parser, &source, chunk_options) {
        Ok(prepared) => prepared,
        Err(e) => {
            return PrepareOutcome::Done(FileOutcome::Failed {
                file: file.clone(),
                reason: format!("prepare: {e}"),
            });
        }
    };
    if let Some(module) = module_fragment_for(&file.id, &blocks) {
        fragments.push(module);
    }
    let texts: Vec<String> = fragments
        .iter()
        .map(|fragment| {
            if kind == FileKind::Code {
                if let Some(input) =
                    embedding_input_for_fragment(shared.db.as_ref(), fragment, &blocks)
                {
                    return input;
                }
            }
            crate::extractor::vector_content::vector_content_for_fragment(
                &fragment.metadata,
                fragment.embedding_source(),
                Some(chunk_options.max_chunk_chars),
            )
        })
        .collect();
    PrepareOutcome::Ready(PreparedFile {
        index,
        file: file.clone(),
        fragments,
        blocks,
        texts,
        harvest,
    })
}

/// Embed every prepared file with cross-file packing; a failed pack falls
/// back to per-file, then per-fragment singles, so a poison fragment fails
/// its file, not its wave (zvec batch/file/one-by-one parity). Returns
/// per-file vectors plus per-file failures, or a fail-fast abort.
#[allow(clippy::type_complexity)]
/// Embed every prepared file with cross-file packing; a failed pack falls
/// back to per-file, then per-fragment singles, so a poison fragment fails
/// its file, not its wave (zvec batch/file/one-by-one parity). Returns
/// per-file vectors plus per-file failures, or a fail-fast abort.
fn embed_ready_files<P>(
    shared: &SharedPipeline<P>,
    ready: &[PreparedFile],
) -> Result<(Vec<(usize, Vec<Vec<f32>>)>, Vec<(usize, EmbedFileFailure)>), String>
where
    P: EmbeddingProvider + Sync + 'static,
{
    let mut flat: Vec<(usize, &str)> = Vec::new();
    for prepared in ready {
        for text in &prepared.texts {
            flat.push((prepared.index, text.as_str()));
        }
    }
    let mut per_file: HashMap<usize, Vec<Vec<f32>>> = HashMap::new();
    let mut file_failed: HashMap<usize, EmbedFileFailure> = HashMap::new();
    for pack in flat.chunks(shared.options.max_embed_batch.max(1)) {
        let refs: Vec<&str> = pack.iter().map(|(_, text)| *text).collect();
        match embed_with_retry(
            &shared.provider,
            &refs,
            &shared.options,
            &shared.histogram_embed,
        ) {
            Ok(batch) => {
                for ((index, _), vector) in pack.iter().zip(batch_vectors(&batch)) {
                    per_file.entry(*index).or_default().push(vector);
                }
            }
            Err(EmbedFailure::FailFast(reason)) => return Err(reason),
            Err(EmbedFailure::Retryable(batch_failure)) => {
                // One-by-one fallback per contributing file.
                let mut members: HashMap<usize, Vec<&str>> = HashMap::new();
                for (index, text) in pack {
                    members.entry(*index).or_default().push(text);
                }
                for (index, texts) in members {
                    if file_failed.contains_key(&index) {
                        continue;
                    }
                    match embed_singles(
                        &shared.provider,
                        &texts,
                        &shared.options,
                        &shared.histogram_embed,
                    ) {
                        Ok(vectors) => {
                            per_file.entry(index).or_default().extend(vectors);
                        }
                        Err(EmbedFailure::FailFast(reason)) => return Err(reason),
                        Err(EmbedFailure::Retryable(_)) => {
                            file_failed.insert(index, batch_failure.clone());
                        }
                    }
                }
            }
        }
    }
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for prepared in ready {
        if let Some(failure) = file_failed.remove(&prepared.index) {
            failed.push((prepared.index, failure));
        } else {
            ok.push((
                prepared.index,
                per_file.remove(&prepared.index).unwrap_or_default(),
            ));
        }
    }
    Ok((ok, failed))
}

fn batch_vectors(batch: &BatchEmbedding) -> Vec<Vec<f32>> {
    (0..batch.count).map(|i| batch.vector(i).to_vec()).collect()
}

/// Phase budget for one embedding input: comment and summary text before
/// the model window. Caps cost per fragment; oversize inputs truncate with
/// a recorded gap instead of failing.
pub const MAX_EMBEDDING_INPUT_CHARS: usize = 4000;

/// Candidate embedding input source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Candidate embedding input source.
pub enum EmbeddingInput {
    /// Raw comment text only.
    Comment,
    /// Generated summary text only.
    Summary,
    /// Summary followed by comment text.
    Both,
}

/// Build one fragment's embedding input from its comment and summary.
/// Empty unless at least one side is present under the mode; oversize
/// inputs truncate to the cap and record a gap instead of failing.
pub fn build_embedding_input(
    node_key: &str,
    doc: Option<&str>,
    summary: Option<&str>,
    mode: EmbeddingInput,
) -> (Option<String>, Option<crate::summary::SummaryGap>) {
    let mut parts = Vec::new();
    if matches!(mode, EmbeddingInput::Summary | EmbeddingInput::Both) {
        if let Some(text) = summary.map(str::trim).filter(|text| !text.is_empty()) {
            parts.push(text);
        }
    }
    if matches!(mode, EmbeddingInput::Comment | EmbeddingInput::Both) {
        if let Some(text) = doc.map(str::trim).filter(|text| !text.is_empty()) {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        return (None, None);
    }
    let mut text = parts.join("\n");
    let mut gap = None;
    if text.len() > MAX_EMBEDDING_INPUT_CHARS {
        let mut end = MAX_EMBEDDING_INPUT_CHARS;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        gap = Some(crate::summary::SummaryGap {
            node_key: node_key.to_string(),
            reason: "truncated",
            detail: format!("embedding input cut to {end} chars"),
        });
    }
    (Some(text), gap)
}

/// Embedding input for one prepared code fragment: summary-aware text
/// when either side exists, falling back to the existing content-derived
/// window otherwise. Prose keeps its content-derived input. Summaries
/// apply only when their recorded hash still matches the current block:
/// a stale row degrades to the comment input, never silently.
fn embedding_input_for_fragment(
    db: &GuidanceDb,
    fragment: &PreparedFragment,
    blocks: &[CommentBlock],
) -> Option<String> {
    let (doc, name, scope) = match &fragment.metadata {
        Some(FragmentMetadata::Code {
            doc,
            symbol_name,
            scope,
            ..
        }) => (
            doc.as_deref(),
            symbol_name.as_deref().unwrap_or(""),
            scope.as_deref().unwrap_or(""),
        ),
        _ => return None,
    };
    if name.is_empty() && doc.map_or(true, |doc| doc.trim().is_empty()) {
        return None;
    }
    let node_key = if scope.is_empty() {
        name.to_string()
    } else {
        format!("{scope}::{name}")
    };
    let block_hash = blocks
        .iter()
        .find(|block| block.node_key == node_key)
        .map(|block| block.comment_hash.as_str());
    let summary = db
        .summaries_for_fragment(&fragment.id)
        .ok()
        .and_then(|rows| {
            rows.into_iter().find(|row| {
                row.status == "ready" && block_hash.is_some_and(|hash| hash == row.comment_hash)
            })
        })
        .map(|row| row.summary);
    let (input, _gap) =
        build_embedding_input(&node_key, doc, summary.as_deref(), EmbeddingInput::Both);
    input
}

/// Per-fragment singles with retry (deepest fallback rung).
fn embed_singles<P>(
    provider: &CachedEmbeddingProvider<P>,
    texts: &[&str],
    options: &IndexOptions,
    histogram: &LatencyHistogram,
) -> Result<Vec<Vec<f32>>, EmbedFailure>
where
    P: EmbeddingProvider + Sync + 'static,
{
    let mut out = Vec::with_capacity(texts.len());
    for single in texts {
        let batch = embed_with_retry(provider, std::slice::from_ref(single), options, histogram)?;
        out.push(batch.vector(0).to_vec());
    }
    Ok(out)
}

fn commit_prepared<P>(
    shared: &SharedPipeline<P>,
    prepared: &PreparedFile,
    vectors: &[Vec<f32>],
) -> FileOutcome
where
    P: EmbeddingProvider + Sync + 'static,
{
    let commit_started = Instant::now();
    let result = commit_file(
        shared.db.as_ref(),
        shared.nlp.as_deref(),
        &prepared.file,
        &prepared.fragments,
        &prepared.blocks,
        vectors,
        &prepared.harvest,
        &prepared.texts,
        &embedder_id(&shared.provider),
        true,
        false,
    );
    shared.histogram_commit.observe_duration(commit_started);
    match result {
        Ok(count) => FileOutcome::Indexed { fragments: count },
        Err(reason) => FileOutcome::Failed {
            file: prepared.file.clone(),
            reason,
        },
    }
}

/// Degraded commit for files whose embed failed: rows persist unembedded
/// with a recorded status instead of failing the file. The next attempt
/// waits for new input, a new embedder, or an explicit backfill.
fn commit_degraded<P>(
    shared: &SharedPipeline<P>,
    prepared: &PreparedFile,
    failure: &EmbedFileFailure,
) -> FileOutcome
where
    P: EmbeddingProvider + Sync + 'static,
{
    let commit_started = Instant::now();
    let result = commit_file(
        shared.db.as_ref(),
        shared.nlp.as_deref(),
        &prepared.file,
        &prepared.fragments,
        &prepared.blocks,
        &[],
        &prepared.harvest,
        &prepared.texts,
        &embedder_id(&shared.provider),
        false,
        failure.unavailable,
    );
    shared.histogram_commit.observe_duration(commit_started);
    tracing::info!(
        file = prepared.file.relative_path.as_str(),
        reason = failure.reason.as_str(),
        unavailable = failure.unavailable,
        "embedding degraded to unembedded rows",
    );
    match result {
        Ok(count) => FileOutcome::Indexed { fragments: count },
        Err(reason) => FileOutcome::Failed {
            file: prepared.file.clone(),
            reason,
        },
    }
}

/// Embedder identity bound into status rows: vectors are only reusable
/// under the embedder (and dimensions) that produced them.
fn embedder_id<P: EmbeddingProvider + 'static>(provider: &CachedEmbeddingProvider<P>) -> String {
    format!("{}:{}", provider.name(), provider.dimensions())
}

/// Extractor dispatch shared by the batch pipeline and live sync: code,
/// then markdown, then text over one fallback ladder. The code rung yields
/// fragments, graph inputs, and comment blocks from a single parse.
pub fn prepare_fragments(
    parser: &mut AstParser,
    source: &ExtractSource,
    options: ChunkOptions,
) -> Result<(Vec<PreparedFragment>, HarvestedFile, Vec<CommentBlock>), crate::extractor::ExtractError>
{
    // Extractor dispatch rides the sync fallback ladder (N.ladder): code,
    // then markdown, then text. Each rung declines sources outside its
    // contract (`Ok(None)`); an empty rung is skipped, never an error.
    // The code rung additionally yields its graph inputs from the same
    // parse — set only when the code rung wins, so the harvest always
    // describes the fragments it accompanies.
    let rungs = ["code", "markdown", "text"];
    let mut code_harvest: Option<HarvestedFile> = None;
    let mut code_blocks: Vec<CommentBlock> = Vec::new();
    let found = fluent_concurrency::ladder::first_accept_in_order_sync(
        rungs,
        |rung| -> Result<Option<Vec<PreparedFragment>>, crate::extractor::ExtractError> {
            let fragments = match rung {
                "code" => {
                    let (fragments, harvest, blocks) =
                        CodeExtractor.extract_with_harvest(parser, source, options)?;
                    if !fragments.is_empty() {
                        code_harvest = Some(harvest);
                    }
                    // Blocks ride along even when the rung declines: a
                    // comment-only file still maps to its module fragment.
                    if !blocks.is_empty() {
                        code_blocks = blocks;
                    }
                    fragments
                }
                "markdown" => {
                    crate::extractor::markdown::extract_markdown_fragments(source, options)?
                }
                _ => crate::extractor::text::extract_plain_text_fragments(source, options)?,
            };
            Ok(if fragments.is_empty() {
                None
            } else {
                Some(fragments)
            })
        },
        |_: &crate::extractor::ExtractError| true,
    );
    match found {
        Ok(Some(fragments)) => Ok((
            fragments,
            code_harvest.unwrap_or_default(),
            std::mem::take(&mut code_blocks),
        )),
        Ok(None) => Ok((Vec::new(), HarvestedFile::default(), Vec::new())),
        Err(e) => Err(e),
    }
}

enum EmbedFailure {
    FailFast(String),
    Retryable(EmbedFileFailure),
}

/// One file's embed failure: the reason plus whether the backend (rather
/// than the input) is at fault. Unavailable marks transport trouble and
/// waits; anything else is a terminal input failure. Both record without
/// failing the file — the next attempt waits for new input, a new
/// embedder, or an explicit backfill.
#[derive(Debug, Clone)]
/// One file's embed failure: the reason plus whether the backend (rather
/// than the input) is at fault. Unavailable marks transport trouble and
/// waits; anything else is a terminal input failure. Both record without
/// failing the file — the next attempt waits for new input, a new
/// embedder, or an explicit backfill.
struct EmbedFileFailure {
    reason: String,
    unavailable: bool,
}

/// Transport-shaped embed errors (timeouts, refused connections, rate
/// limits) may succeed later; everything else fails the input.
fn is_unavailable(error: &EmbeddingError) -> bool {
    matches!(error, EmbeddingError::RequestFailed(_))
}

fn embed_with_retry<P>(
    provider: &CachedEmbeddingProvider<P>,
    texts: &[&str],
    options: &IndexOptions,
    histogram: &LatencyHistogram,
) -> Result<BatchEmbedding, EmbedFailure>
where
    P: EmbeddingProvider + Sync + 'static,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        let started = Instant::now();
        match provider.embed_batch(texts) {
            Ok(batch) => {
                histogram.observe_duration(started);
                return Ok(batch);
            }
            Err(e) if is_fail_fast(&e) => return Err(EmbedFailure::FailFast(e.to_string())),
            Err(e) if attempt >= options.retry_max_attempts.max(1) => {
                return Err(EmbedFailure::Retryable(EmbedFileFailure {
                    reason: e.to_string(),
                    unavailable: is_unavailable(&e),
                }));
            }
            Err(_) => {
                let delay = backoff_ms(options.retry_base_ms, attempt, EMBED_RETRY_JITTER_PCT);
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
        }
    }
}

/// Fail-fast errors (auth/config/permanent) abort the run at once instead
/// of burning the retry budget (ports `:314-627` + `:593`).
fn is_fail_fast(error: &EmbeddingError) -> bool {
    matches!(
        error,
        EmbeddingError::UnknownProvider(_)
            | EmbeddingError::InvalidApiUrl
            | EmbeddingError::InsecureApiUrl
            | EmbeddingError::SsrfBlockedUrl
            | EmbeddingError::NoApiKey
    )
}

/// Commit one file's extractor chunks through the unified writer, then its
/// graph rows from the same extract pass, and record per-fragment embedding
/// status. `embedded` selects the vector policy: required vectors fail the
/// file on mismatch, degraded commits persist unembedded rows with a
/// recorded failure instead.
#[allow(clippy::too_many_arguments)]
/// Commit one file's extractor chunks through the unified writer, then its
/// graph rows from the same extract pass, and record per-fragment embedding
/// status. `embedded` selects the vector policy: required vectors fail the
/// file on mismatch, degraded commits persist unembedded rows with a
/// recorded failure instead.
fn commit_file(
    db: &GuidanceDb,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    file: &ScannedFile,
    fragments: &[PreparedFragment],
    blocks: &[CommentBlock],
    vectors: &[Vec<f32>],
    harvest: &HarvestedFile,
    texts: &[String],
    embedder: &str,
    embedded: bool,
    unavailable: bool,
) -> Result<usize, String> {
    let record = scanned_record(file);
    let kind = parse_scan_kind(file.kind.as_deref());
    let policy = if embedded {
        VectorPolicy::Required
    } else {
        VectorPolicy::Optional
    };
    let committed = if embedded { vectors } else { &[] };
    let count =
        FragmentWriter::write_items(db, &record, kind, fragments, blocks, committed, nlp, policy)?;
    commit_graph(db, &file.absolute_path, harvest)?;
    record_embedding_status(db, fragments, texts, embedder, embedded, unavailable)?;
    Ok(count)
}

/// Persist one file's embedding provenance: input hash plus embedder
/// identity per fragment, ready or not. Unchanged files skip on content
/// alone afterwards; the status decides only rotation backfills.
fn record_embedding_status(
    db: &GuidanceDb,
    fragments: &[PreparedFragment],
    texts: &[String],
    embedder: &str,
    embedded: bool,
    unavailable: bool,
) -> Result<(), String> {
    use search_vector::db::EmbeddingStatusRecord;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let status = if embedded {
        "ready"
    } else if unavailable {
        "unavailable"
    } else {
        "failed"
    };
    let rows: Vec<EmbeddingStatusRecord> = fragments
        .iter()
        .zip(texts.iter())
        .map(|(fragment, text)| EmbeddingStatusRecord {
            fragment_id: fragment.id.clone(),
            input_hash: common_core::hash::blake3_hex(text.as_bytes()),
            embedder: embedder.to_string(),
            status: status.to_string(),
            updated_at: now_ms,
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    db.replace_embedding_status(&rows)
        .map_err(|e| format!("status: {e}"))?;
    Ok(())
}

/// Persist one file's graph inputs from the extract pass's harvest (the
/// same inputs `assemble` consumes — never a second parse). Unnamed
/// scopes persist under "" (calls only, never a definition).
fn commit_graph(db: &GuidanceDb, path: &str, harvest: &HarvestedFile) -> Result<(), String> {
    let symbols: Vec<(String, Vec<String>)> = harvest
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.name.clone().unwrap_or_default(),
                symbol.calls.clone(),
            )
        })
        .collect();
    db.replace_file_graph(path, &harvest.imports, &symbols)
        .map_err(|e| format!("graph: {e}"))
}

#[cfg(test)]
#[path = "../tests/index_pipeline.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/m4_cutoff.rs"]
mod m4_cutoff_tests;

#[cfg(test)]
#[path = "../tests/lemma_provenance.rs"]
mod lemma_provenance_tests;