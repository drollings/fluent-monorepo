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
        Self {
            dims: 4,
            fail_marker: None,
            fail_fast: false,
            attempts: Arc::new(AtomicUsize::new(0)),
        }
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
        self.embed_batch(&[text])
            .map(|batch| batch.vector(0).to_vec())
    }
    fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<fluent_llm::embeddings::BatchEmbedding, fluent_llm::embeddings::EmbeddingError>
    {
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
        Ok(fluent_llm::embeddings::BatchEmbedding {
            flat,
            count: texts.len(),
            dims: self.dims as usize,
        })
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
    let stats = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("index");
    assert_eq!(stats.files_indexed, 1, "{stats:?}");
    assert_eq!(stats.files_added, 1, "{stats:?}");
    assert_eq!(stats.files_failed, 0);

    // Search finds the content.
    let hits = db
        .search_fts(
            "GoodNeedle",
            10,
            &search_vector::db::FragmentFilter::default(),
        )
        .expect("search");
    assert!(!hits.is_empty(), "indexed content must be searchable");

    // Immediate re-run indexes zero files.
    let stats = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("re-index");
    assert_eq!(stats.files_indexed, 0, "{stats:?}");
    assert_eq!(stats.files_unchanged, 1, "{stats:?}");

    // Refresh: modify the file.
    write(root, "good.ts", "export const GoodNeedle = 2;\n");
    let stats = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("refresh");
    assert_eq!(stats.files_modified, 1, "{stats:?}");
    assert_eq!(stats.files_indexed, 1, "{stats:?}");

    // Drop: delete the file.
    std::fs::remove_file(root.join("good.ts")).expect("rm");
    let stats = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("drop");
    assert_eq!(stats.files_deleted, 1, "{stats:?}");
    assert!(db
        .search_fts(
            "GoodNeedle",
            10,
            &search_vector::db::FragmentFilter::default()
        )
        .expect("search")
        .is_empty());

    // Drop the whole workspace index.
    write(root, "good.ts", "export const GoodNeedle = 3;\n");
    index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("re-index");
    db.drop_index().expect("drop");
    assert!(db.list_files().expect("list").is_empty());
    assert!(db
        .search_fts(
            "GoodNeedle",
            10,
            &search_vector::db::FragmentFilter::default()
        )
        .expect("search")
        .is_empty());

    // Rebuild re-adds everything.
    write(root, "good.ts", "export const GoodNeedle = 3;\n");
    let mut options = default_options();
    options.rebuild = true;
    let stats = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &options,
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("rebuild");
    assert_eq!(stats.files_added, 1, "{stats:?}");
}

#[test]
fn embedding_status_unavailable_marks_without_failing_file() {
    // A poison fragment fails its embed: the file still commits
    // (unembedded, labelled by the freshness flag), the failure is
    // recorded with its input hash and embedder, and an unchanged repass
    // makes no new embedding calls. Recovery happens through content
    // staleness, never a retry loop.
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
    let stats = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("embed failure must not fail the run");
    assert_eq!(stats.files_indexed, 2, "{stats:?}");
    assert_eq!(stats.files_failed, 0, "{stats:?}");
    assert!(
        !db.search_fts(
            "FailureNeedle",
            10,
            &search_vector::db::FragmentFilter::default()
        )
        .expect("search")
        .is_empty(),
        "failed files still commit their content"
    );
    let failing_id = db
        .list_files()
        .expect("files")
        .into_iter()
        .find(|file| file.relative_path == "failing.ts")
        .expect("failing row")
        .id;
    let statuses = db
        .embedding_status_for_file(&failing_id)
        .expect("status rows");
    assert!(!statuses.is_empty(), "failure must be recorded");
    assert!(
        statuses.iter().all(|row| row.status == "unavailable"),
        "{statuses:?}"
    );
    assert!(
        statuses.iter().all(|row| row.embedder.starts_with("stub")),
        "{statuses:?}"
    );
    assert!(
        statuses.iter().all(|row| row.input_hash.len() == 64),
        "input hashes are recorded: {statuses:?}"
    );
    assert!(
        !db.fragment_freshness(&failing_id)
            .expect("fingerprint")
            .expect("row")
            .embedded,
        "unembedded rows read back as unembedded"
    );
    let attempts = provider.attempts.load(Ordering::SeqCst);
    assert!(attempts > 0);
    let second = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("repass");
    assert_eq!(second.files_indexed, 0, "{second:?}");
    assert_eq!(
        provider.attempts.load(Ordering::SeqCst),
        attempts,
        "unchanged files never re-embed, failed or not"
    );
    // Fix the file: content staleness re-embeds and clears the status.
    write(root, "failing.ts", "export const RecoveredNeedle = 3;\n");
    index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("recovery");
    assert!(
        provider.attempts.load(Ordering::SeqCst) > attempts,
        "edited files re-embed"
    );
    let statuses = db
        .embedding_status_for_file(&failing_id)
        .expect("status rows");
    assert!(
        statuses.iter().all(|row| row.status == "ready"),
        "{statuses:?}"
    );
    assert!(
        db.fragment_freshness(&failing_id)
            .expect("fingerprint")
            .expect("row")
            .embedded
    );
    assert!(db.failed_file_ids().expect("failed").is_empty());
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
    let err = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
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
    let _ = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(!db
        .search_fts("OkOne", 10, &search_vector::db::FragmentFilter::default())
        .expect("search")
        .is_empty());
    assert!(!db
        .search_fts("OkTwo", 10, &search_vector::db::FragmentFilter::default())
        .expect("search")
        .is_empty());
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
    let stats = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &sel,
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("index");
    assert_eq!(stats.image_skipped, 1, "{stats:?}");
    assert_eq!(stats.files_indexed, 2, "{stats:?}");
}

#[test]
fn wave_unit_names_are_stable() {
    assert_eq!(wave_name(0), "index-wave-0");
    assert_eq!(wave_name(3), "index-wave-3");
}

#[test]
fn batch_commit_lemmas_come_from_comment_docs() {
    // Item chunks carry comment lemmas only: comment words land in the
    // lemma table while body-only identifiers stay out (FTS serves them).
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(
        root,
        "lib.rs",
        "/// Adds one to the total.\npub fn alpha_beta() {}\n",
    );
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = StubEmbedding::ok();
    index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("index");
    for word in ["adds", "total"] {
        assert!(
            !db.fragments_for_lemmas(&[word.to_string()], 10)
                .expect("lookup")
                .is_empty(),
            "comment word missing from lemmas: {word:?}"
        );
    }
    for body in ["alpha", "beta"] {
        assert!(
            db.fragments_for_lemmas(&[body.to_string()], 10)
                .expect("lookup")
                .is_empty(),
            "body-only identifier leaked into lemmas: {body:?}"
        );
    }
    assert!(
        !db.search_fts(
            "alpha_beta",
            10,
            &search_vector::db::FragmentFilter::default()
        )
        .expect("search")
        .is_empty(),
        "code bodies must stay FTS-visible"
    );
}

#[test]
fn zero_file_rerun_is_clean() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "a.rs", "fn a() {}\n");
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = StubEmbedding::ok();
    index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("index");
    let stats = index_workspace(
        Arc::clone(&db),
        provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("re-index");
    assert_eq!(stats.files_indexed, 0);
    assert_eq!(stats.files_failed, 0);
}

#[test]
fn embedding_status_embedder_rotation_backfills() {
    // Same bytes, new embedder identity (dimensions rotated 4 -> 8):
    // recorded rows mismatch and re-embed even though nothing changed on
    // disk. Unchanged repass under the same embedder makes no new calls
    // and never trips the clean-reprocess ratchet.
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "a.rs", "/// Adds one.\npub fn alpha() -> i32 { 1 }\n");
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let first_provider = StubEmbedding {
        dims: 4,
        fail_marker: None,
        fail_fast: false,
        attempts: Arc::new(AtomicUsize::new(0)),
    };
    index_workspace(
        Arc::clone(&db),
        first_provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("first index");
    let file_id = db
        .list_files()
        .expect("files")
        .into_iter()
        .find(|file| file.relative_path == "a.rs")
        .expect("a row")
        .id;
    let statuses = db.embedding_status_for_file(&file_id).expect("status rows");
    assert!(
        statuses.iter().all(|row| row.status == "ready"),
        "{statuses:?}"
    );
    assert!(
        statuses.iter().all(|row| row.embedder == "stub:4"),
        "{statuses:?}"
    );
    let second_provider = StubEmbedding {
        dims: 8,
        fail_marker: None,
        fail_fast: false,
        attempts: Arc::new(AtomicUsize::new(0)),
    };
    let stats = index_workspace(
        Arc::clone(&db),
        second_provider.clone(),
        &selection_for(root),
        &default_options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("rotated index");
    assert_ne!(
        second_provider.attempts.load(Ordering::SeqCst),
        0,
        "rotation must re-embed: {stats:?}"
    );
    assert_eq!(stats.reprocessed_clean, 0, "{stats:?}");
    let statuses = db.embedding_status_for_file(&file_id).expect("status rows");
    assert!(
        statuses.iter().all(|row| row.embedder == "stub:8"),
        "{statuses:?}"
    );
}

#[test]
fn embedding_status_input_modes_cover_documented_nodes() {
    // Coverage calibration over one fixture: Comment reaches documented
    // nodes, Summary reaches summarized nodes, Both reaches the union.
    // Caps and truncation are pinned below; this pins the choice.
    let text = |mode| {
        build_embedding_input(
            "alpha",
            Some("Adds one to the total."),
            Some("Adds one."),
            mode,
        )
    };
    let (comment, _) = text(EmbeddingInput::Comment);
    assert_eq!(comment.as_deref(), Some("Adds one to the total."));
    let (summary, _) = text(EmbeddingInput::Summary);
    assert_eq!(summary.as_deref(), Some("Adds one."));
    let (both, gap) = text(EmbeddingInput::Both);
    let both = both.expect("both covers documented nodes");
    assert!(both.contains("Adds one."), "{both}");
    assert!(both.contains("Adds one to the total."), "{both}");
    assert!(gap.is_none());
    assert!(build_embedding_input("x", None, None, EmbeddingInput::Both)
        .0
        .is_none());
    assert!(
        build_embedding_input("x", None, Some("S."), EmbeddingInput::Comment)
            .0
            .is_none()
    );
    assert!(
        build_embedding_input("x", Some("C."), None, EmbeddingInput::Summary)
            .0
            .is_none()
    );
}

#[test]
fn embedding_status_input_truncation_records_gap() {
    let huge = "word ".repeat(2000);
    let (input, gap) = build_embedding_input("huge", Some(&huge), None, EmbeddingInput::Comment);
    let input = input.expect("truncated input still embeds");
    assert!(input.len() <= MAX_EMBEDDING_INPUT_CHARS, "{}", input.len());
    assert!(input.is_char_boundary(input.len()));
    let gap = gap.expect("oversize input records a gap");
    assert_eq!(gap.node_key, "huge");
    assert_eq!(gap.reason, "truncated");
}

#[test]
fn embedding_status_coverage_prefers_both() {
    // Three documented nodes: alpha has doc + summary, beta has doc only,
    // gamma has summary only. Both covers the union; neither single
    // source does.
    struct Node {
        doc: Option<&'static str>,
        summary: Option<&'static str>,
    }
    let nodes = [
        (
            "alpha",
            Node {
                doc: Some("Adds one."),
                summary: Some("Adds one to the total."),
            },
        ),
        (
            "beta",
            Node {
                doc: Some("Removes two."),
                summary: None,
            },
        ),
        (
            "gamma",
            Node {
                doc: None,
                summary: Some("Stale keeper."),
            },
        ),
    ];
    let coverage = |mode| {
        nodes
            .iter()
            .filter(|(_, node)| {
                build_embedding_input("n", node.doc, node.summary, mode)
                    .0
                    .is_some()
            })
            .count()
    };
    assert_eq!(coverage(EmbeddingInput::Comment), 2);
    assert_eq!(coverage(EmbeddingInput::Summary), 2);
    assert_eq!(coverage(EmbeddingInput::Both), 3);
}

#[test]
fn embedding_status_stale_summary_falls_back_to_comment() {
    use crate::extractor::{FragmentMetadata, FragmentRange, PreparedFragment};
    // A summary row whose hash no longer matches the current block is
    // stale: the builder falls back to the comment instead of embedding
    // stale text.
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let fragment = PreparedFragment {
        id: "fa".to_string(),
        content_text: "fn alpha() {}".to_string(),
        embedding_text: None,
        range: FragmentRange {
            start_line: 1,
            end_line: 1,
            start_offset: 0,
            end_offset: 12,
        },
        group: None,
        metadata: Some(FragmentMetadata::Code {
            symbol_type: crate::search_types::CodeSymbolType::Function,
            symbol_name: Some("alpha".to_string()),
            scope: None,
            node_type: "function_item".to_string(),
            signature: Some("fn alpha()".to_string()),
            doc: Some("Adds one.".to_string()),
            modifiers: Vec::new(),
        }),
        calls: Vec::new(),
    };
    let file = search_vector::db::FileRecord {
        id: "f".to_string(),
        absolute_path: "/repo/lib.rs".to_string(),
        relative_path: "lib.rs".to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 12,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    db.upsert_fragments(
        &file,
        &[search_vector::db::FragmentRecord {
            id: "fa".to_string(),
            group: None,
            file_id: "f".to_string(),
            range_json: "{\"kind\":\"file\"}".to_string(),
            content_kind: "text".to_string(),
            content_text: Some("fn alpha() {}".to_string()),
            cjk_text: String::new(),
            symbol_type: Some("function".to_string()),
            symbol_name: Some("alpha".to_string()),
            scope: None,
            signature: Some("fn alpha()".to_string()),
            doc: Some("Adds one.".to_string()),
            modifiers: String::new(),
            heading: None,
            heading_level: None,
            embedding: None,
        }],
        &[],
    )
    .expect("upsert");
    let stale = search_vector::db::SummaryRecord {
        fragment_id: "fa".to_string(),
        comment_hash: "outdated-hash".to_string(),
        model: "test-model".to_string(),
        prompt_version: 1,
        temperature: 0.0,
        seed: 42,
        summary: "Stale words.".to_string(),
        status: "ready".to_string(),
        generated_at: 0,
        authorized: false,
    };
    db.replace_summaries(&[stale]).expect("seed stale row");
    let block = crate::ast_parser::CommentBlock {
        node_key: "alpha".to_string(),
        node_kind: "function_item".to_string(),
        name: "alpha".to_string(),
        scope: String::new(),
        start_byte: 0,
        end_byte: 8,
        start_line: 1,
        end_line: 1,
        text: "Adds one.".to_string(),
        comment_hash: common_core::hash::blake3_hex("Adds one.".as_bytes()),
    };
    let input = embedding_input_for_fragment(&db, &fragment, &[block]);
    assert_eq!(input.as_deref(), Some("Adds one."));
}
