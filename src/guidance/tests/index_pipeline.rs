//! P2 pipeline tests (ports `service.test.mjs:688` lifecycle,
//! `:957` failed/retry/stale/rebuild, `:314-627` failure taxonomy,
//! `:1042` error context, plus `index-status` diff counts).

use super::*;
use search_vector::db::GuidanceDb;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

fn selection_for(root: &std::path::Path) -> FileSelection {
    FileSelection {
        roots: vec![root.to_path_buf()],
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extra_skip_dirs: Vec::new(),
        honor_gitignore: false,
        max_bytes_override: None,
    }
}

fn default_options() -> IndexOptions {
    IndexOptions {
        max_embed_batch: 8,
        wave_files: 4,
        retry_max_attempts: 3,
        retry_base_ms: 1,
        rebuild: false,
    }
}

/// Deterministic stub in the `FakeEmbeddingModel` role: constant vectors
/// plus scripted failures.
#[derive(Clone)]
struct StubEmbedding {
    dims: u32,
    /// Marker text that fails the batch.
    fail_marker: Option<String>,
    /// Fail-fast error instead of the retryable one.
    fail_fast: bool,
    attempts: Arc<AtomicUsize>,
}

impl StubEmbedding {
    fn ok() -> Self {
        Self { dims: 4, fail_marker: None, fail_fast: false, attempts: Arc::new(AtomicUsize::new(0)) }
    }
}

impl fluent_llm::embeddings::EmbeddingProvider for StubEmbedding {
    fn name(&self) -> &'static str {
        "stub"
    }
    fn dimensions(&self) -> u32 {
        self.dims
    }
    fn embed(&self, text: &str) -> Result<Vec<f32>, fluent_llm::embeddings::EmbeddingError> {
        self.embed_batch(&[text]).map(|batch| batch.vector(0).to_vec())
    }
    fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<fluent_llm::embeddings::BatchEmbedding, fluent_llm::embeddings::EmbeddingError> {
        use fluent_llm::embeddings::EmbeddingError;
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if let Some(marker) = &self.fail_marker {
            if texts.iter().any(|t| t.contains(marker)) {
                return Err(if self.fail_fast {
                    EmbeddingError::NoApiKey
                } else {
                    EmbeddingError::RequestFailed("fixture embedding failure".to_string())
                });
            }
        }
        let mut flat = Vec::with_capacity(texts.len() * self.dims as usize);
        for (i, text) in texts.iter().enumerate() {
            let base = (text.len() + i) as f32;
            flat.extend([base, base + 0.5, base + 1.0, base + 1.5]);
        }
        Ok(fluent_llm::embeddings::BatchEmbedding { flat, count: texts.len(), dims: self.dims as usize })
    }
}

fn write(root: &std::path::Path, name: &str, content: &str) {
    std::fs::write(root.join(name), content).expect("write");
}

#[test]
fn lifecycle_index_search_refresh_drop_rebuild() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "good.ts", "export const GoodNeedle = 1;\n");
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = StubEmbedding::ok();

    // Index.
    let stats = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {})
        .expect("index");
    assert_eq!(stats.files_indexed, 1, "{stats:?}");
    assert_eq!(stats.files_added, 1, "{stats:?}");
    assert_eq!(stats.files_failed, 0);

    // Search finds the content.
    let hits = db.search_fts("GoodNeedle", 10, &search_vector::db::ZgFragmentFilter::default()).expect("search");
    assert!(!hits.is_empty(), "indexed content must be searchable");

    // Immediate re-run indexes zero files.
    let stats = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {})
        .expect("re-index");
    assert_eq!(stats.files_indexed, 0, "{stats:?}");
    assert_eq!(stats.files_unchanged, 1, "{stats:?}");

    // Refresh: modify the file.
    write(root, "good.ts", "export const GoodNeedle = 2;\n");
    let stats = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {})
        .expect("refresh");
    assert_eq!(stats.files_modified, 1, "{stats:?}");
    assert_eq!(stats.files_indexed, 1, "{stats:?}");

    // Drop: delete the file.
    std::fs::remove_file(root.join("good.ts")).expect("rm");
    let stats = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {})
        .expect("drop");
    assert_eq!(stats.files_deleted, 1, "{stats:?}");
    assert!(db.search_fts("GoodNeedle", 10, &search_vector::db::ZgFragmentFilter::default()).expect("search").is_empty());

    // Drop the whole workspace index.
    write(root, "good.ts", "export const GoodNeedle = 3;\n");
    index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {}).expect("re-index");
    db.drop_index().expect("drop");
    assert!(db.zg_list_files().expect("list").is_empty());
    assert!(db.search_fts("GoodNeedle", 10, &search_vector::db::ZgFragmentFilter::default()).expect("search").is_empty());

    // Rebuild re-adds everything.
    write(root, "good.ts", "export const GoodNeedle = 3;\n");
    let mut options = default_options();
    options.rebuild = true;
    let stats = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &options, None, &AtomicBool::new(false), &mut |_| {})
        .expect("rebuild");
    assert_eq!(stats.files_added, 1, "{stats:?}");
}

#[test]
fn failed_files_record_retry_and_rebuild() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "good.ts", "export const GoodNeedle = 1;\n");
    write(root, "failing.ts", "export const FailureNeedle = 2;\n");
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = StubEmbedding {
        dims: 4,
        fail_marker: Some("FailureNeedle".to_string()),
        fail_fast: false,
        attempts: Arc::new(AtomicUsize::new(0)),
    };

    // Progress stays monotonic and reports the retry pass.
    let mut completed = Vec::new();
    let mut saw_retry = false;
    let err = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |progress| {
        if progress.phase == "scanning" && progress.detail.to_lowercase().contains("retry") {
            saw_retry = true;
        }
        completed.push(progress.files_indexed.saturating_sub(progress.files_failed));
    })
    .expect_err("failing file must fail the run");
    let message = err.to_string();
    assert!(message.contains("failing.ts"), "{message}");
    assert!(message.contains("fixture embedding failure"), "{message}");
    assert!(message.contains("Retried failed files once automatically"), "{message}");
    assert!(completed.windows(2).all(|w| w[1] >= w[0]), "{completed:?}");
    assert!(saw_retry, "retry pass must be reported");

    // Retryable errors are retried (batch + one-by-one attempts).
    assert!(provider.attempts.load(Ordering::SeqCst) > 2, "{}", provider.attempts.load(Ordering::SeqCst));

    // Status: 1 failed, 1 indexed; failure is stored data.
    assert_eq!(db.failed_file_ids().expect("failed").len(), 1);
    assert!(!db.search_fts("GoodNeedle", 10, &search_vector::db::ZgFragmentFilter::default()).expect("search").is_empty());

    // Fix the file: the failed record retries via pending and recovers.
    write(root, "failing.ts", "export const RecoveredNeedle = 3;\n");
    let stats = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {})
        .expect("recovery");
    assert_eq!(stats.files_failed, 0, "{stats:?}");
    assert!(stats.files_pending + stats.files_modified >= 1, "{stats:?}");
    assert!(db.failed_file_ids().expect("failed").is_empty());
    assert!(!db.search_fts("RecoveredNeedle", 10, &search_vector::db::ZgFragmentFilter::default()).expect("search").is_empty());
}

#[test]
fn fail_fast_errors_abort_without_retry() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "a.ts", "export const FailureNeedle = 1;\n");
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = StubEmbedding {
        dims: 4,
        fail_marker: Some("FailureNeedle".to_string()),
        fail_fast: true,
        attempts: Arc::new(AtomicUsize::new(0)),
    };
    let err = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {})
        .expect_err("fail-fast must abort");
    assert!(matches!(err, IndexError::FailFast(_)), "{err:?}");
    // No retries on fail-fast errors.
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn per_file_isolation_keeps_siblings() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "ok1.ts", "export const OkOne = 1;\n");
    write(root, "ok2.ts", "export const OkTwo = 2;\n");
    write(root, "bad.ts", "export const FailureNeedle = 3;\n");
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = StubEmbedding {
        dims: 4,
        fail_marker: Some("FailureNeedle".to_string()),
        fail_fast: false,
        attempts: Arc::new(AtomicUsize::new(0)),
    };
    let _ = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {});
    assert!(!db.search_fts("OkOne", 10, &search_vector::db::ZgFragmentFilter::default()).expect("search").is_empty());
    assert!(!db.search_fts("OkTwo", 10, &search_vector::db::ZgFragmentFilter::default()).expect("search").is_empty());
}

#[test]
fn images_are_counted_not_embedded() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "notes.md", "# Hi\n");
    std::fs::write(root.join("photo.png"), vec![0x89u8, 0x50, 0x4e, 0x47]).expect("write");
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = StubEmbedding::ok();
    let mut sel = selection_for(root);
    sel.include_globs = vec!["*.png".to_string(), "*.md".to_string()];
    let stats = index_workspace(Arc::clone(&db), provider.clone(), &sel, &default_options(), None, &AtomicBool::new(false), &mut |_| {}).expect("index");
    assert_eq!(stats.image_skipped, 1, "{stats:?}");
    assert_eq!(stats.files_indexed, 2, "{stats:?}");
}

#[test]
fn wave_unit_names_are_stable() {
    assert_eq!(wave_name(0), "index-wave-0");
    assert_eq!(wave_name(3), "index-wave-3");
}

#[test]
fn zero_file_rerun_is_clean() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "a.rs", "fn a() {}\n");
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = StubEmbedding::ok();
    index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {}).expect("index");
    let stats = index_workspace(Arc::clone(&db), provider.clone(), &selection_for(root), &default_options(), None, &AtomicBool::new(false), &mut |_| {}).expect("re-index");
    assert_eq!(stats.files_indexed, 0);
    assert_eq!(stats.files_failed, 0);
}
