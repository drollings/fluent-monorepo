//! One source-scope classification shared by selection, read, and graph.
//!
//! Test, fixture, eval, data, capability, and documentation artifacts must
//! not compete with product code for evidence. The predicates already live
//! in their owning modules; this type composes them instead of adding a
//! second boolean at each consumer.

use std::path::Path;

use crate::eval::is_eval_path;
use crate::search_types::{FileInfo, FileKind};
use crate::selection::is_benchmark_corpus_path;

/// What a source path is for retrieval and maintenance purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// What a source path is for retrieval and maintenance purposes.
pub enum SourceScope {
    /// Product source code.
    Product,
    /// Human documentation (markdown, README/AGENTS, doc tree).
    Doc,
    /// External capability corpus input.
    Capability,
    /// Test or e2e source.
    Test,
    /// Checked-in fixture tree.
    Fixture,
    /// Measurement eval/benchmark corpus.
    Eval,
    /// Structured data corpus.
    Data,
}

impl SourceScope {
    /// Stable report spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Doc => "doc",
            Self::Capability => "capability",
            Self::Test => "test",
            Self::Fixture => "fixture",
            Self::Eval => "eval",
            Self::Data => "data",
        }
    }

    /// Whether this scope may be product evidence or a product dependent.
    #[must_use]
    pub fn is_product(self) -> bool {
        matches!(self, Self::Product)
    }

    /// Whether documentation retrieval owns this path.
    #[must_use]
    pub fn is_doc(self) -> bool {
        matches!(self, Self::Doc)
    }
}

/// Classify a workspace-relative path with an optional file kind.
#[must_use]
/// Classify a workspace-relative path with an optional file kind.
pub fn classify(workspace_path: &str, kind: Option<FileKind>) -> SourceScope {
    let path = workspace_path.trim_start_matches("./").replace('\\', "/");
    if path.contains("doc/capabilities/") || path.starts_with("doc/capabilities/") {
        return SourceScope::Capability;
    }
    if is_eval_path(&path) || is_benchmark_corpus_path(&path) {
        return SourceScope::Eval;
    }
    if is_fixture_path(&path) {
        return SourceScope::Fixture;
    }
    if crate::offload::is_test_path(Path::new(&path)) {
        return SourceScope::Test;
    }
    if kind == Some(FileKind::Data) || path.starts_with("data/") {
        return SourceScope::Data;
    }
    if kind == Some(FileKind::Text) || path.starts_with("doc/") || path.ends_with(".md") {
        return SourceScope::Doc;
    }
    SourceScope::Product
}

/// Classify an indexed file.
#[must_use]
/// Classify an indexed file.
pub fn of_file(file: &FileInfo) -> SourceScope {
    classify(&file.relative_path, file.kind)
}

/// Checked-in fixture trees: `fixture_*`, `fixtures/`, and the known
/// read/graph fixtures. Kept separate from `is_test_path` because fixtures
/// may be legitimate test inputs and must be opt-in for indexing.
#[must_use]
/// Checked-in fixture trees: `fixture_*`, `fixtures/`, and the known
/// read/graph fixtures. Kept separate from `is_test_path` because fixtures
/// may be legitimate test inputs and must be opt-in for indexing.
pub fn is_fixture_path(path: &str) -> bool {
    let normalized = path.trim_start_matches("./").replace('\\', "/");
    normalized.split('/').any(|part| {
        part == "fixtures"
            || part == "fixture_graph"
            || part == "fixture_read"
            || part.starts_with("fixture_")
    })
}

#[cfg(test)]
#[path = "../tests/source_scope.rs"]
mod source_scope_tests;
