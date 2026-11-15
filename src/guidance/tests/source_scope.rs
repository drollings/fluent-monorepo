//! Source-scope classification invariants.

use crate::search_types::FileKind;
use crate::source_scope::{classify, is_fixture_path, SourceScope};

#[test]
fn product_code_is_product() {
    assert_eq!(
        classify("src/guidance/src/sync_engine.rs", Some(FileKind::Code)),
        SourceScope::Product
    );
}

#[test]
fn fixture_trees_are_fixture_at_any_depth() {
    for path in [
        "src/guidance/fixture_read/README.md",
        "src/guidance/fixture_graph/h_a.rs",
        "fixtures/input.rs",
        "src/guidance/fixture_read/nested/deep/file.rs",
    ] {
        assert!(is_fixture_path(path), "{path}");
        assert_eq!(classify(path, None), SourceScope::Fixture, "{path}");
    }
}

#[test]
fn tests_eval_data_and_docs_are_separate() {
    assert_eq!(
        classify("src/guidance/tests/read_engine.rs", Some(FileKind::Code)),
        SourceScope::Test
    );
    assert_eq!(
        classify("doc/guidance/eval/questions.jsonl", None),
        SourceScope::Eval
    );
    assert_eq!(
        classify("src/guidance/corpora/benchmark_tasks.jsonl", None),
        SourceScope::Eval
    );
    assert_eq!(classify("data/yago/x.ttl", None), SourceScope::Data);
    assert_eq!(
        classify("doc/guidance/ARCHITECTURE.md", Some(FileKind::Text)),
        SourceScope::Doc
    );
    assert_eq!(
        classify("src/llm/CALIBRATION.md", Some(FileKind::Text)),
        SourceScope::Doc
    );
}

#[test]
fn separators_normalize_before_classification() {
    assert_eq!(
        classify(".\\src\\guidance\\fixture_read\\README.md", None),
        SourceScope::Fixture
    );
    assert_eq!(
        classify("./src/guidance/src/lib.rs", Some(FileKind::Code)),
        SourceScope::Product
    );
}
