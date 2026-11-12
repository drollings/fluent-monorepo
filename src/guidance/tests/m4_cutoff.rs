//! Cutoff and invalidation pins: converged passes do zero work, waves
//! batch embeds, sig changes pull dependents, macro changes widen,
//! failures isolate without poisoning siblings.

use super::*;
use search_vector::db::GuidanceDb;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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

fn options() -> IndexOptions {
    IndexOptions {
        max_embed_batch: 8,
        wave_files: 64,
        retry_max_attempts: 3,
        retry_base_ms: 1,
        rebuild: false,
    }
}

/// Counting stub: constant vectors, batch-call count, embedded texts.
#[derive(Clone)]
struct CountingEmbedding {
    dims: u32,
    batches: Arc<AtomicUsize>,
    texts: Arc<Mutex<Vec<String>>>,
    fail_marker: Option<String>,
}

impl CountingEmbedding {
    fn ok() -> Self {
        Self {
            dims: 4,
            batches: Arc::new(AtomicUsize::new(0)),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_marker: None,
        }
    }
}

impl fluent_llm::embeddings::EmbeddingProvider for CountingEmbedding {
    fn name(&self) -> &'static str {
        "counting"
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
        self.batches.fetch_add(1, Ordering::SeqCst);
        if let Some(marker) = &self.fail_marker {
            if texts.iter().any(|t| t.contains(marker)) {
                return Err(EmbeddingError::RequestFailed("fixture failure".to_string()));
            }
        }
        common_core::sync::lock(&self.texts).extend(texts.iter().map(ToString::to_string));
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

fn run(
    db: &Arc<GuidanceDb>,
    provider: CountingEmbedding,
    root: &std::path::Path,
) -> Result<IndexStats, IndexError> {
    index_workspace(
        Arc::clone(db),
        provider,
        &selection_for(root),
        &options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
}

fn write(root: &std::path::Path, name: &str, content: &str) {
    std::fs::write(root.join(name), content).expect("write");
}

#[test]
fn converged_pass_does_zero_work() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    for i in 0..6 {
        write(
            root,
            &format!("f{i:02}.rs"),
            &format!("pub fn f{i:02}() -> u64 {{ {i} }}\n"),
        );
    }
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = CountingEmbedding::ok();
    let first = run(&db, provider.clone(), root).expect("first");
    assert_eq!(first.files_indexed, 6, "{first:?}");
    // Cross-file packing: one batch call for six small files, not six.
    assert!(
        provider.batches.load(Ordering::SeqCst) < 6,
        "waves must batch, not invoke per file: {:?}",
        provider.batches.load(Ordering::SeqCst)
    );
    let batches_first = provider.batches.load(Ordering::SeqCst);
    let second = run(&db, provider.clone(), root).expect("second");
    assert_eq!(second.files_indexed, 0, "{second:?}");
    assert_eq!(second.files_unchanged, 6, "{second:?}");
    assert_eq!(second.reprocessed_clean, 0, "{second:?}");
    assert_eq!(
        provider.batches.load(Ordering::SeqCst),
        batches_first,
        "converged pass embeds nothing"
    );
}

#[test]
fn body_edit_skips_dependents_sig_edit_pulls_them() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "a.rs", "pub fn foo() -> u64 {\n    1\n}\n");
    write(
        root,
        "b.rs",
        "use crate::a::foo;\npub fn bar() -> u64 {\n    foo()\n}\n",
    );
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = CountingEmbedding::ok();
    run(&db, provider.clone(), root).expect("first");
    // Body-only edit: the dependent is not re-harvested.
    write(root, "a.rs", "pub fn foo() -> u64 {\n    2\n}\n");
    let second = run(&db, provider.clone(), root).expect("second");
    assert_eq!(second.files_indexed, 1, "{second:?}");
    // Signature edit: the dependent re-harvests through its edge.
    write(root, "a.rs", "pub fn foo(x: u64) -> u64 {\n    x\n}\n");
    let third = run(&db, provider.clone(), root).expect("third");
    assert_eq!(third.files_indexed, 2, "{third:?}");
    assert_eq!(third.reprocessed_clean, 0, "{third:?}");
}

#[test]
fn macro_change_widens_to_the_package() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "m.rs", "macro_rules! m {\n    () => { 1 };\n}\n");
    write(
        root,
        "n.rs",
        "use crate::m::m;\npub fn n() -> u64 {\n    1\n}\n",
    );
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = CountingEmbedding::ok();
    run(&db, provider.clone(), root).expect("first");
    // Body-only macro change: no interface moves, but the package
    // re-harvests because macro uses hide from edges.
    write(root, "m.rs", "macro_rules! m {\n    () => { 2 };\n}\n");
    let second = run(&db, provider.clone(), root).expect("second");
    assert_eq!(second.files_indexed, 2, "{second:?}");
}

#[test]
fn failure_degrades_without_failing_dependents() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "f.rs", "pub fn f() -> u64 {\n    1\n}\n");
    write(
        root,
        "g.rs",
        "use crate::f::f;\npub fn g() -> u64 {\n    f()\n}\n",
    );
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = CountingEmbedding::ok();
    run(&db, provider.clone(), root).expect("baseline");
    // Break f with an embedding failure: the file commits unembedded
    // instead of failing the run, and the unchanged dependent is left
    // alone (its interface never moved).
    let mut failing = CountingEmbedding::ok();
    failing.fail_marker = Some("FAILME".to_string());
    write(
        root,
        "f.rs",
        "pub fn f() -> u64 {\n    // FAILME\n    1\n}\n",
    );
    let before = failing.batches.load(Ordering::SeqCst);
    let degraded = run(&db, failing.clone(), root).expect("degraded, not failed");
    assert_eq!(degraded.files_failed, 0, "{degraded:?}");
    assert!(
        failing.batches.load(Ordering::SeqCst) > before,
        "the poisoned input reached the embedder"
    );
    let files = db.list_files().expect("files");
    let fingerprint = |relative: &str| {
        let id = files
            .iter()
            .find(|file| file.relative_path == relative)
            .expect("file row")
            .id
            .clone();
        db.fragment_freshness(&id)
            .expect("fingerprint")
            .expect("row")
    };
    assert!(
        !fingerprint("f.rs").embedded,
        "failed files stay unembedded"
    );
    assert!(
        fingerprint("g.rs").embedded,
        "dependents keep their vectors"
    );
    let statuses = db
        .embedding_status_for_file(
            &files
                .iter()
                .find(|file| file.relative_path == "f.rs")
                .expect("file row")
                .id,
        )
        .expect("status rows");
    assert!(
        statuses.iter().any(|row| row.status == "unavailable"),
        "{statuses:?}"
    );
    // Marker removed: the tree converges with zero clean reprocessing.
    write(root, "f.rs", "pub fn f() -> u64 {\n    1\n}\n");
    let recovered = run(&db, CountingEmbedding::ok(), root).expect("recover");
    assert_eq!(recovered.reprocessed_clean, 0, "{recovered:?}");
    assert!(fingerprint("f.rs").embedded, "recovery re-embeds");
}

#[test]
fn moved_symbol_pulls_dangling_dependent() {
    // foo moves a.rs -> b.rs; b.rs drops its import, d.rs keeps the stale
    // `use crate::a::foo`. d.rs is byte-identical, but the interface it
    // consumes is gone: the sig gate must pull it through the persisted
    // edge, or d.rs goes silently stale.
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(root, "a.rs", "pub fn foo() -> u64 {\n    1\n}\n");
    write(
        root,
        "b.rs",
        "use crate::a::foo;\npub fn bar() -> u64 {\n    foo()\n}\n",
    );
    write(
        root,
        "d.rs",
        "use crate::a::foo;\npub fn deep() -> u64 {\n    foo()\n}\n",
    );
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let provider = CountingEmbedding::ok();
    run(&db, provider.clone(), root).expect("baseline");
    // Move foo into b.rs; b.rs drops its import, d.rs dangles.
    write(root, "a.rs", "pub fn unused() -> u64 {\n    0\n}\n");
    write(
        root,
        "b.rs",
        "pub fn foo() -> u64 {\n    1\n}\npub fn bar() -> u64 {\n    foo()\n}\n",
    );
    let seen = common_core::sync::lock(&provider.texts).len();
    let moved = run(&db, provider.clone(), root).expect("move");
    assert!(moved.files_indexed >= 3, "{moved:?}");
    let tail = common_core::sync::lock(&provider.texts)[seen..].join("\n");
    assert!(
        tail.contains("deep"),
        "unchanged d.rs must re-verify when its consumed interface moves: {tail:?}"
    );
    // The symbol table tracks the move: foo left a.rs, arrived in b.rs.
    let symbols = |relative: &str| {
        let id = db
            .list_files()
            .expect("files")
            .into_iter()
            .find(|file| file.relative_path == relative)
            .expect("file row")
            .id;
        db.symbols_for_file(&id)
            .expect("symbols")
            .into_iter()
            .map(|row| row.name.unwrap_or_default())
            .collect::<Vec<_>>()
    };
    assert!(!symbols("a.rs").contains(&"foo".to_string()), "{:?}", symbols("a.rs"));
    assert!(symbols("b.rs").contains(&"foo".to_string()), "{:?}", symbols("b.rs"));
    // d.rs rows stay retrievable under the new graph.
    assert!(!db
        .search_fts(
            "deep",
            10,
            &search_vector::db::FragmentFilter::default()
        )
        .expect("search")
        .is_empty());
}
