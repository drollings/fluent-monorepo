//! Pack and bench pins: deterministic task packs with manifests,
//! impact with tests-touching, and converged-tree bench counters.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

use crate::query::context_pack::{
    build_task_pack, impact_report_with_tests, tests_touching, PackInput,
};

fn pack_input() -> PackInput {
    PackInput {
        target_source: "pub fn foo() -> u64 {\n    1\n}\n".to_string(),
        target_path: "src/a.rs".to_string(),
        diagnostic: Some("E0308: mismatched types".to_string()),
        callee_sigs: vec!["fn bar() -> u64".to_string()],
        callers: vec!["src/b.rs: use crate::a::foo;".to_string()],
        siblings: vec!["src/c.rs: fn baz() {}".to_string()],
        test_excerpts: vec!["tests/a.rs: assert_eq!(foo(), 1);".to_string()],
        budget_tokens: 8_000,
    }
}

#[test]
fn packs_are_deterministic_with_manifests() {
    let first = build_task_pack(&pack_input());
    let second = build_task_pack(&pack_input());
    assert_eq!(first.0.items, second.0.items, "same inputs yield same pack");
    assert_eq!(first.1.citations, second.1.citations);
    assert_eq!(first.1.hashes, second.1.hashes);
    assert_eq!(
        first.0.items.len() + first.0.dropped,
        second.0.items.len() + second.0.dropped
    );
    // Priority order survives packing: target first.
    assert!(first
        .0
        .items
        .first()
        .is_some_and(|item| item.source == "src/a.rs"));
    // Manifest covers every kept item exactly once.
    assert_eq!(first.1.citations.len(), first.0.items.len());
    assert_eq!(first.1.hashes.len(), first.0.items.len());
}

#[test]
fn packs_respect_small_budgets() {
    let mut input = pack_input();
    input.budget_tokens = 8;
    let (pack, manifest) = build_task_pack(&input);
    assert!(pack.dropped > 0, "tiny budgets drop tail items");
    assert_eq!(manifest.citations.len(), pack.items.len());
}

#[test]
fn tests_touching_matches_item_names_as_tokens() {
    let test_files = vec![
        (
            "tests/a.rs".to_string(),
            "use crate::a::foo;\nassert_eq!(foo(), 1);\n".to_string(),
        ),
        (
            "tests/b.rs".to_string(),
            "use crate::b::bar;\nassert_eq!(bar(), 2);\n".to_string(),
        ),
    ];
    let touched = tests_touching(
        &test_files,
        &["foo".to_string(), "quux_missing".to_string()],
    );
    assert_eq!(touched, vec!["tests/a.rs".to_string()]);
    // Substrings are not tokens: `food` must not match `foo`.
    let tricky = vec![("tests/c.rs".to_string(), "let food = 1;\n".to_string())];
    assert!(tests_touching(&tricky, &["foo".to_string()]).is_empty());
}

#[test]
fn impact_lists_tests_touching_the_change() {
    use crate::graph_index::{GraphIndex, HarvestInput};
    let inputs = vec![
        HarvestInput {
            path: "src/a.rs".to_string(),
            format: "rust".to_string(),
            text: "pub fn foo() -> u64 {\n    1\n}\n".to_string(),
        },
        HarvestInput {
            path: "src/b.rs".to_string(),
            format: "rust".to_string(),
            text: "use crate::a::foo;\npub fn bar() -> u64 {\n    foo()\n}\n".to_string(),
        },
    ];
    let graph = GraphIndex::build(&inputs, &[".".to_string()]).expect("build");
    let test_files = vec![(
        "tests/a.rs".to_string(),
        "use crate::a::foo;\nassert_eq!(foo(), 1);\n".to_string(),
    )];
    let report = impact_report_with_tests(
        &graph,
        &["src/a.rs".to_string()],
        &test_files,
        &["foo".to_string()],
    );
    assert!(report.dependents.contains(&"src/b.rs".to_string()));
    assert!(report.tests.contains(&"tests/a.rs".to_string()));
    assert!(!report.gates.is_empty(), "narrowest gates still joined");
}

/// Deterministic stub: constant vectors plus a batch-call counter.
#[derive(Clone)]
struct CountingEmbedding {
    batches: Arc<AtomicUsize>,
}

impl fluent_llm::embeddings::EmbeddingProvider for CountingEmbedding {
    fn name(&self) -> &'static str {
        "counting"
    }
    fn dimensions(&self) -> u32 {
        4
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
        self.batches.fetch_add(1, Ordering::SeqCst);
        let mut flat = Vec::with_capacity(texts.len() * 4);
        for (i, text) in texts.iter().enumerate() {
            let base = (text.len() + i) as f32;
            flat.extend([base, base + 0.5, base + 1.0, base + 1.5]);
        }
        Ok(fluent_llm::embeddings::BatchEmbedding {
            flat,
            count: texts.len(),
            dims: 4,
        })
    }
}

#[test]
fn bench_counters_on_converged_trees() {
    use crate::index_pipeline::{index_workspace, IndexOptions};
    use crate::selection::FileSelection;
    use search_vector::db::GuidanceDb;
    use std::sync::atomic::AtomicBool;
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    for i in 0..3 {
        std::fs::write(
            root.join(format!("f{i:02}.rs")),
            format!("pub fn f{i:02}() -> u64 {{ {i} }}\n"),
        )
        .expect("write");
    }
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    let selection = FileSelection {
        roots: vec![root.to_path_buf()],
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extra_skip_dirs: Vec::new(),
        honor_gitignore: false,
        max_bytes_override: None,
    };
    let options = IndexOptions {
        max_embed_batch: 8,
        wave_files: 64,
        retry_max_attempts: 3,
        retry_base_ms: 1,
        rebuild: false,
    };
    let provider = CountingEmbedding {
        batches: Arc::new(AtomicUsize::new(0)),
    };
    let run = |provider: CountingEmbedding| {
        index_workspace(
            Arc::clone(&db),
            provider,
            &selection,
            &options,
            None,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .expect("index")
    };
    run(provider.clone());
    let batches_before = provider.batches.load(Ordering::SeqCst);
    let second = run(provider.clone());
    assert_eq!(second.reprocessed_clean, 0, "clean files never reprocess");
    assert_eq!(
        provider.batches.load(Ordering::SeqCst),
        batches_before,
        "converged passes spend zero model calls"
    );
}
