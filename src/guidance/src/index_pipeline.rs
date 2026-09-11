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
use fluent_llm::embeddings::{BatchEmbedding, CachedEmbeddingProvider, EmbeddingError, EmbeddingProvider};
use fluent_wvr::{
    Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit,
    impl_component,
};
use internment::ArcIntern;
use search_vector::db::{FragmentLemma, GuidanceDb, ZgFileRecord};

use crate::ast_parser::AstParser;
use crate::diff::{ScannedFile, compute_diff, hash_file, make_file_id};
use crate::extractor::{
    ChunkOptions, ExtractSource, FragmentMetadata, PreparedFragment,
};
use crate::extractor::code::{CodeExtractor, HarvestedFile};
use crate::query::db_storage::{fragment_lemma, zg_fragment_record};
use crate::query::ingest::query_lemmas;
use crate::selection::{FileSelection, ScanDiagnostics, SelectedFile, select_files};
use crate::zg_types::{
    CodeEntityModifier, EntityFragment, EntityMetadata, FileKind, ZgContent, ZgRange,
};

/// Files per bounded wave (named constant, tuned in P6).
pub const INDEX_WAVE_FILES: usize = 64;
/// Fragment-index schema version enforced by the `SyncEngine` gate.
pub const INDEX_SCHEMA_VERSION: u32 = 1;/// Default cross-file embed packing size.
pub const INDEX_EMBED_BATCH: usize = 32;
/// Transient embed retry budget (zvec C.4: 3×500ms).
pub const EMBED_RETRY_ATTEMPTS: u32 = 3;
/// Transient embed retry base (ms).
pub const EMBED_RETRY_BASE_MS: u64 = 500;
/// Retry backoff jitter (percent).
pub const EMBED_RETRY_JITTER_PCT: u32 = 50;

/// Pipeline options.
#[derive(Debug, Clone)]
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
}

/// Index failure (stored per-file data surfaces as `FilesFailed`; the run
/// still commits every sibling first).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
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
    let mut stats = IndexStats::default();
    let mut diag = ScanDiagnostics::default();
    let selected = select_files(selection, &mut diag).map_err(|e| IndexError::Db(e.to_string()))?;
    let scanned: Vec<ScannedFile> = selected.iter().map(scan_selected).collect();
    stats.files_scanned = scanned.len();

    let mut existing = db.zg_list_files().map_err(|e| IndexError::Db(e.to_string()))?;
    if options.rebuild {
        for file in &existing {
            db.delete_file(&file.id).map_err(|e| IndexError::Db(e.to_string()))?;
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
    let by_id: HashMap<&str, &search_vector::db::ZgFileRecord> =
        existing.iter().map(|file| (file.id.as_str(), file)).collect();
    let in_scope = |id: &str| {
        by_id.get(id).is_some_and(|record| {
            let path = Path::new(&record.absolute_path);
            selection.roots.iter().any(|root| path == root || path.starts_with(root))
        })
    };
    for id in diff.deleted.iter().filter(|id| in_scope(id)) {
        db.delete_file(id).map_err(|e| IndexError::Db(e.to_string()))?;
        stats.files_deleted += 1;
    }

    let mut work: Vec<ScannedFile> =
        diff.added.into_iter().chain(diff.modified).chain(diff.pending).collect();
    // Deterministic file order via the dependency graph (P2 registers file
    // nodes; P3 adds symbol edges; cancellation rides `dependents_of`).
    let mut graph = DependencyGraph::<String>::new();
    for file in &work {
        let _ = graph.register(&file.id, &[], std::slice::from_ref(&file.id));
    }
    let order = graph.topo_sort().map_err(|e| IndexError::Db(e.to_string()))?;
    let position: HashMap<&str, usize> = order.iter().map(String::as_str).zip(0..).collect();
    work.sort_by_key(|file| position.get(file.id.as_str()).copied().unwrap_or(usize::MAX));

    let total = work.len();
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
    for (wave_index, chunk) in work.chunks(options.wave_files.max(1)).map(Vec::from).enumerate() {
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
        let retry_files: Vec<ScannedFile> =
            failed.iter().map(|(file, _)| file.clone()).collect();
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

fn kind_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Text => "text",
        FileKind::Code => "code",
        FileKind::Data => "data",
        FileKind::Image => "image",
    }
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
    let instrumented =
        fluent_wvr::wrapper::Instrumented::with_metrics(unit, name, wave_histogram);
    // Multi-thread runtime: shared provider seams (e.g. the embedding
    // cache) may `block_in_place`; waves still run sequentially.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| IndexError::Db(e.to_string()))?;
    let summary = runtime.block_on(async {
        let mut batch = SupervisedBatch::new(Arc::new(TokioRuntime), fluent_wvr::CapabilitySet::new());
        batch.register(Arc::new(instrumented)).expect("wave unit registers");
        batch.await
    });
    // Fail-fast aborts surface from the outcomes (the unit records the
    // abort before failing); infrastructure failures surface from the
    // batch summary.
    let guard = outcomes.lock().expect("outcomes");
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
    let mut outcomes_guard = outcomes.lock().expect("outcomes");
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
    Indexed { fragments: usize },
    ImageSkipped,
    Failed { file: ScannedFile, reason: String },
    /// Fail-fast abort: stop scheduling at once (no retry, no second pass).
    Abort { reason: String },
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
                PrepareOutcome::Done(outcome) => outcomes.lock().expect("outcomes").push(outcome),
            }
        }
        // Embed phase: cross-file packing (zvec batch parity) with
        // per-file / per-fragment fallback (file + one-by-one parity).
        let (mut by_index, failures) = match embed_ready_files(&self.shared, &ready) {
            Ok((ok, failed)) => (ok.into_iter().collect::<HashMap<_, _>>(), failed),
            Err(reason) => {
                outcomes.lock().expect("outcomes").push(FileOutcome::Abort { reason });
                return Err(WorkError::Execution(format!("{} aborted", self.name)));
            }
        };
        for (index, reason) in failures {
            let file = ready
                .iter()
                .find(|prepared| prepared.index == index)
                .map(|prepared| prepared.file.clone())
                .expect("failed file is prepared");
            outcomes.lock().expect("outcomes").push(FileOutcome::Failed { file, reason });
        }
        // Commit phase: per-file replace + vector-count validation.
        for prepared in &ready {
            if outcomes.lock().expect("outcomes").iter().any(|outcome| match outcome {
                FileOutcome::Failed { file, .. } => file.id == prepared.file.id,
                _ => false,
            }) {
                continue;
            }
            let vectors = by_index.remove(&prepared.index).unwrap_or_default();
            let outcome = commit_prepared(&self.shared, prepared, &vectors);
            outcomes.lock().expect("outcomes").push(outcome);
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
    let _ = shared;
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
    let text = match std::fs::read_to_string(&file.absolute_path) {
        Ok(text) => text,
        Err(e) => {
            return PrepareOutcome::Done(FileOutcome::Failed {
                file: file.clone(),
                reason: format!("read: {e}"),
            });
        }
    };
    let kind = match file.kind.as_deref() {
        Some("code") => FileKind::Code,
        Some("data") => FileKind::Data,
        _ => FileKind::Text,
    };
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
    let (fragments, harvest) = match prepare_fragments(parser, &source, chunk_options) {
        Ok(prepared) => prepared,
        Err(e) => {
            return PrepareOutcome::Done(FileOutcome::Failed {
                file: file.clone(),
                reason: format!("prepare: {e}"),
            });
        }
    };
    let texts: Vec<String> = fragments
        .iter()
        .map(|fragment| {
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
        texts,
        harvest,
    })
}

/// Embed every prepared file with cross-file packing; a failed pack falls
/// back to per-file, then per-fragment singles, so a poison fragment fails
/// its file, not its wave (zvec batch/file/one-by-one parity). Returns
/// per-file vectors plus per-file failure reasons, or a fail-fast abort.
#[allow(clippy::type_complexity)]
fn embed_ready_files<P>(
    shared: &SharedPipeline<P>,
    ready: &[PreparedFile],
) -> Result<(Vec<(usize, Vec<Vec<f32>>)>, Vec<(usize, String)>), String>
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
    let mut file_failed: HashMap<usize, String> = HashMap::new();
    for pack in flat.chunks(shared.options.max_embed_batch.max(1)) {
        let refs: Vec<&str> = pack.iter().map(|(_, text)| *text).collect();
        match embed_with_retry(&shared.provider, &refs, &shared.options, &shared.histogram_embed) {
            Ok(batch) => {
                for ((index, _), vector) in pack.iter().zip(batch_vectors(&batch)) {
                    per_file.entry(*index).or_default().push(vector);
                }
            }
            Err(EmbedFailure::FailFast(reason)) => return Err(reason),
            Err(EmbedFailure::Retryable(batch_error)) => {
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
                            file_failed.insert(index, batch_error.clone());
                        }
                    }
                }
            }
        }
    }
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for prepared in ready {
        if let Some(reason) = file_failed.remove(&prepared.index) {
            failed.push((prepared.index, reason));
        } else {
            ok.push((prepared.index, per_file.remove(&prepared.index).unwrap_or_default()));
        }
    }
    Ok((ok, failed))
}

fn batch_vectors(batch: &BatchEmbedding) -> Vec<Vec<f32>> {
    (0..batch.count).map(|i| batch.vector(i).to_vec()).collect()
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
    if vectors.len() != prepared.fragments.len() {
        return FileOutcome::Failed {
            file: prepared.file.clone(),
            reason: format!(
                "vector-count mismatch: {} vectors for {} fragments",
                vectors.len(),
                prepared.fragments.len()
            ),
        };
    }
    let commit_started = Instant::now();
    let result = commit_file(
        shared.db.as_ref(),
        shared.nlp.as_deref(),
        &prepared.file,
        &prepared.fragments,
        vectors,
        &prepared.harvest,
    );
    shared.histogram_commit.observe_duration(commit_started);
    match result {
        Ok(count) => FileOutcome::Indexed { fragments: count },
        Err(reason) => FileOutcome::Failed { file: prepared.file.clone(), reason },
    }
}

fn prepare_fragments(
    parser: &mut AstParser,
    source: &ExtractSource,
    options: ChunkOptions,
) -> Result<(Vec<PreparedFragment>, HarvestedFile), crate::extractor::ExtractError> {
    // Extractor dispatch rides the sync fallback ladder (N.ladder): code,
    // then markdown, then text. Each rung declines sources outside its
    // contract (`Ok(None)`); an empty rung is skipped, never an error.
    // The code rung additionally yields its graph inputs from the same
    // parse — set only when the code rung wins, so the harvest always
    // describes the fragments it accompanies.
    let rungs = ["code", "markdown", "text"];
    let mut code_harvest: Option<HarvestedFile> = None;
    let found = fluent_concurrency::ladder::first_accept_in_order_sync(
        rungs,
        |rung| -> Result<Option<Vec<PreparedFragment>>, crate::extractor::ExtractError> {
            let fragments = match rung {
                "code" => {
                    let (fragments, harvest) =
                        CodeExtractor.extract_with_harvest(parser, source, options)?;
                    if !fragments.is_empty() {
                        code_harvest = Some(harvest);
                    }
                    fragments
                }
                "markdown" => {
                    crate::extractor::markdown::extract_markdown_fragments(source, options)?
                }
                _ => crate::extractor::text::extract_plain_text_fragments(source, options)?,
            };
            Ok(if fragments.is_empty() { None } else { Some(fragments) })
        },
        |_: &crate::extractor::ExtractError| true,
    );
    match found {
        Ok(Some(fragments)) => Ok((fragments, code_harvest.unwrap_or_default())),
        Ok(None) => Ok((Vec::new(), HarvestedFile::default())),
        Err(e) => Err(e),
    }
}

enum EmbedFailure {
    FailFast(String),
    Retryable(String),
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
                return Err(EmbedFailure::Retryable(e.to_string()));
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

fn scanned_record(file: &ScannedFile) -> ZgFileRecord {
    ZgFileRecord {
        id: file.id.clone(),
        absolute_path: file.absolute_path.clone(),
        relative_path: file.relative_path.clone(),
        root_path: file.root_path.clone(),
        size_bytes: file.size_bytes,
        last_modified_time: file.last_modified_time,
        kind: file.kind.clone(),
        format: file.format.clone(),
        content_hash: file.content_hash.clone(),
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

fn commit_file(
    db: &GuidanceDb,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    file: &ScannedFile,
    fragments: &[PreparedFragment],
    vectors: &[Vec<f32>],
    harvest: &HarvestedFile,
) -> Result<usize, String> {
    let record = scanned_record(file);
    let mut fragment_records = Vec::with_capacity(fragments.len());
    let mut lemmas: Vec<FragmentLemma> = Vec::new();
    for (fragment, vector) in fragments.iter().zip(vectors.iter()) {
        let entity = prepared_to_entity(file, fragment);
        let mut row = zg_fragment_record(&entity);
        row.cjk_text = common_core::string::cjk_bigrams(&fragment.content_text).join(" ");
        row.embedding = Some(vector.clone());
        fragment_records.push(row);
        for lemma in query_lemmas(nlp, &fragment.content_text) {
            lemmas.push(fragment_lemma(&fragment.id, &lemma, 1.0));
        }
    }
    let count = db
        .replace_file(&record, &fragment_records, &lemmas)
        .map_err(|e| format!("commit: {e}"))?;
    commit_graph(db, &file.absolute_path, harvest)?;
    Ok(count)
}

/// Persist one file's graph inputs from the extract pass's harvest (the
/// same inputs `assemble` consumes — never a second parse). Unnamed
/// scopes persist under "" (calls only, never a definition).
fn commit_graph(db: &GuidanceDb, path: &str, harvest: &HarvestedFile) -> Result<(), String> {
    let symbols: Vec<(String, Vec<String>)> = harvest
        .symbols
        .iter()
        .map(|symbol| (symbol.name.clone().unwrap_or_default(), symbol.calls.clone()))
        .collect();
    db.replace_file_graph(path, &harvest.imports, &symbols)
        .map_err(|e| format!("graph: {e}"))
}

fn prepared_to_entity(file: &ScannedFile, fragment: &PreparedFragment) -> EntityFragment {
    EntityFragment {
        id: fragment.id.clone(),
        group: fragment.group.clone(),
        file_id: file.id.clone(),
        range: ZgRange::Text {
            start_line: fragment.range.start_line as u32,
            end_line: fragment.range.end_line as u32,
            start_offset: fragment.range.start_offset as u64,
            end_offset: fragment.range.end_offset as u64,
        },
        content: ZgContent::Text { text: fragment.content_text.clone() },
        metadata: fragment.metadata.as_ref().map(prepared_metadata),
    }
}

fn prepared_metadata(metadata: &FragmentMetadata) -> EntityMetadata {
    match metadata {
        FragmentMetadata::Code {
            symbol_type,
            symbol_name,
            scope,
            node_type,
            signature,
            doc,
            modifiers,
        } => EntityMetadata::Code {
            symbol_type: *symbol_type,
            symbol_name: symbol_name.clone(),
            scope: scope.clone(),
            node_type: Some(node_type.clone()),
            signature: signature.clone(),
            doc: doc.clone(),
            modifiers: modifiers
                .iter()
                .filter_map(|name| match name.as_str() {
                    "exported" => Some(CodeEntityModifier::Exported),
                    "async" => Some(CodeEntityModifier::Async),
                    "static" => Some(CodeEntityModifier::Static),
                    "public" => Some(CodeEntityModifier::Public),
                    "private" => Some(CodeEntityModifier::Private),
                    "protected" => Some(CodeEntityModifier::Protected),
                    "internal" => Some(CodeEntityModifier::Internal),
                    _ => None,
                })
                .collect(),
        },
        FragmentMetadata::Markdown { heading, level, scope } => EntityMetadata::Markdown {
            heading: heading.clone(),
            level: *level,
            scope: scope.clone(),
        },
    }
}

#[cfg(test)]
#[path = "../tests/index_pipeline.rs"]
mod tests;
