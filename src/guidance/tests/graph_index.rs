//! P3 `graph_index` tests: per-language import harvesting, specifier
//! resolution, dependents closures over imports + calls, and deterministic
//! L4 re-rank with role-coverage tiebreak.

use crate::extractor::adapter::{format_for_extension, resolve_adapter};
use crate::extractor::code::harvest_file_entities;
use crate::extractor::ExtractSource;
use crate::graph_index::{GraphIndex, GraphQuerySignals, HarvestInput};
use crate::zg_types::FileKind;
use crate::ast_parser::AstParser;

fn harvest(format: &str, text: &str) -> crate::extractor::code::HarvestedFile {
    let mut parser = AstParser::new();
    let source = ExtractSource {
        file_id: "test".to_string(),
        text: text.to_string(),
        format: format.to_string(),
        kind: FileKind::Code,
    };
    harvest_file_entities(&mut parser, &source).expect("harvest")
}

#[test]
fn rust_use_and_mod_declarations_are_edges() {
    let harvested = harvest(
        "rust",
        "use crate::graph::index;\nuse super::sibling;\nmod child;\nfn main() {}\n",
    );
    assert!(harvested.imports.iter().any(|s| s.contains("crate::graph::index")), "{:?}", harvested.imports);
    assert!(harvested.imports.iter().any(|s| s == "mod:child"), "{:?}", harvested.imports);
}

#[test]
fn python_import_statements_emit_one_specifier_per_leaf() {
    let harvested = harvest(
        "python",
        "import os, sys\nfrom pkg.sub import thing\nfrom . import relative\n",
    );
    assert!(harvested.imports.contains(&"os".to_string()), "{:?}", harvested.imports);
    assert!(harvested.imports.contains(&"sys".to_string()), "{:?}", harvested.imports);
    assert!(harvested.imports.contains(&"pkg.sub".to_string()), "{:?}", harvested.imports);
}

#[test]
fn go_import_specs_are_edges() {
    let harvested = harvest(
        "go",
        "package main\nimport (\n\"fmt\"\n\"./local/util\"\n)\nfunc main() {}\n",
    );
    assert!(harvested.imports.contains(&"fmt".to_string()), "{:?}", harvested.imports);
    assert!(harvested.imports.contains(&"./local/util".to_string()), "{:?}", harvested.imports);
}

#[test]
fn c_includes_are_edges() {
    let harvested = harvest("c", "#include \"local.h\"\n#include <stdio.h>\nint main() { return 0; }\n");
    assert!(harvested.imports.contains(&"local.h".to_string()), "{:?}", harvested.imports);
    assert!(harvested.imports.contains(&"stdio.h".to_string()), "{:?}", harvested.imports);
}

#[test]
fn java_imports_are_edges() {
    let harvested = harvest(
        "java",
        "package app;\nimport java.util.List;\nimport app.Util;\nclass Main {}\n",
    );
    assert!(harvested.imports.iter().any(|s| s.ends_with("app.Util")), "{:?}", harvested.imports);
}

#[test]
fn typescript_import_sources_only_not_imported_names() {
    let harvested = harvest(
        "typescript",
        "import { a, b } from \"./mod\";\nimport def from \"pkg\";\nexport { x } from \"./other\";\n",
    );
    assert!(harvested.imports.contains(&"./mod".to_string()), "{:?}", harvested.imports);
    assert!(harvested.imports.contains(&"pkg".to_string()), "{:?}", harvested.imports);
    assert!(harvested.imports.contains(&"./other".to_string()), "{:?}", harvested.imports);
    assert!(!harvested.imports.contains(&"a".to_string()), "{:?}", harvested.imports);
}

#[test]
fn format_lookup_covers_all_six_families() {
    for (ext, format) in [
        ("rs", "rust"),
        ("py", "python"),
        ("go", "go"),
        ("c", "c"),
        ("h", "c"),
        ("java", "java"),
        ("js", "javascript"),
        ("ts", "typescript"),
    ] {
        assert_eq!(format_for_extension(ext), Some(format), "{ext}");
        assert!(resolve_adapter(format).is_some(), "{format}");
    }
}

fn harvest_input(path: &str, format: &str, text: &str) -> HarvestInput {
    HarvestInput {
        path: path.to_string(),
        format: format.to_string(),
        text: text.to_string(),
    }
}

#[test]
fn dependents_closure_follows_import_chains() {
    // a imports b, b imports c.
    let files = vec![
        harvest_input("/repo/a.ts", "typescript", "import \"./b\";\nexport const a = 1;\n"),
        harvest_input("/repo/b.ts", "typescript", "import \"./c\";\nexport const b = 1;\n"),
        harvest_input("/repo/c.ts", "typescript", "export const c = 1;\n"),
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    assert_eq!(
        graph.dependents_closure(&["/repo/c.ts".to_string()]),
        vec!["/repo/a.ts".to_string(), "/repo/b.ts".to_string(), "/repo/c.ts".to_string()]
    );
    assert_eq!(
        graph.dependents_closure(&["/repo/a.ts".to_string()]),
        vec!["/repo/a.ts".to_string()]
    );
}

#[test]
fn call_edges_link_caller_to_callee_file() {
    let files = vec![
        harvest_input("/repo/main.py", "python", "from util import helper\ndef run():\n    helper()\n"),
        harvest_input("/repo/util.py", "python", "def helper():\n    pass\n"),
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    // Touching util.py invalidates main.py through the call edge.
    assert_eq!(
        graph.dependents_closure(&["/repo/util.py".to_string()]),
        vec!["/repo/main.py".to_string(), "/repo/util.py".to_string()]
    );
}

#[test]
fn unresolved_specifiers_are_recorded_not_dropped() {
    let files = vec![harvest_input(
        "/repo/a.ts",
        "typescript",
        "import \"npm-package\";\nexport const a = 1;\n",
    )];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    assert_eq!(graph.import_edges().len(), 1);
    assert!(graph.import_edges()[0].resolved_file.is_none());
}

#[test]
fn rerank_is_deterministic_and_boosts_graph_neighbors() {
    use crate::query::fusion::RecallCandidate;
    use crate::zg_types::{Entity, EntityMetadata, FileInfo, ZgContent, ZgRange};

    fn candidate(id: &str, file: &str, symbol: &str, score: f64) -> RecallCandidate {
        RecallCandidate {
            id: id.to_string(),
            entity: Entity {
                id: id.to_string(),
                file_id: file.to_string(),
                range: ZgRange::File,
                content: ZgContent::Text { text: String::new() },
                metadata: Some(EntityMetadata::Code {
                    symbol_type: crate::zg_types::CodeSymbolType::Function,
                    symbol_name: Some(symbol.to_string()),
                    scope: None,
                    node_type: None,
                    signature: None,
                    doc: None,
                    modifiers: Vec::new(),
                }),
            },
            file: FileInfo {
                id: file.to_string(),
                absolute_path: file.to_string(),
                ..Default::default()
            },
            sources: Vec::new(),
            recall: Vec::new(),
            evidence: Vec::new(),
            score,
            rank: 0,
            forced: false,
        }
    }

    // caller.py calls helper() defined in util.py; unrelated.py is isolated.
    let files = vec![
        harvest_input("/repo/caller.py", "python", "from util import helper\ndef run():\n    helper()\n"),
        harvest_input("/repo/util.py", "python", "def helper():\n    pass\n"),
        harvest_input("/repo/unrelated.py", "python", "def other():\n    pass\n"),
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    let signals = GraphQuerySignals {
        predicate: vec!["helper".to_string()],
        roles: Vec::new(),
    };
    // helper scores highest (anchor); run calls it; other is isolated but
    // scores above run on RRF alone — L4 must promote run above other.
    let mut first = vec![
        candidate("util.py::helper", "/repo/util.py", "helper", 0.30),
        candidate("unrelated.py::other", "/repo/unrelated.py", "other", 0.20),
        candidate("caller.py::run", "/repo/caller.py", "run", 0.10),
    ];
    graph.rerank(&mut first, &signals);
    let order: Vec<&str> = first.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(order[0], "util.py::helper");
    assert_eq!(order[1], "caller.py::run", "{order:?}");
    assert_eq!(order[2], "unrelated.py::other", "{order:?}");

    // Determinism: same inputs, same order, twice.
    let mut second = vec![
        candidate("caller.py::run", "/repo/caller.py", "run", 0.10),
        candidate("unrelated.py::other", "/repo/unrelated.py", "other", 0.20),
        candidate("util.py::helper", "/repo/util.py", "helper", 0.30),
    ];
    graph.rerank(&mut second, &signals);
    let order2: Vec<&str> = second.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(order, order2);
}

#[test]
fn role_coverage_breaks_ties_toward_query_roles() {
    use crate::query::fusion::RecallCandidate;
    use crate::zg_types::{Entity, EntityMetadata, FileInfo, ZgContent, ZgRange};

    fn candidate(id: &str, file: &str, symbol: &str, scope: Option<&str>, score: f64) -> RecallCandidate {
        RecallCandidate {
            id: id.to_string(),
            entity: Entity {
                id: id.to_string(),
                file_id: file.to_string(),
                range: ZgRange::File,
                content: ZgContent::Text { text: String::new() },
                metadata: Some(EntityMetadata::Code {
                    symbol_type: crate::zg_types::CodeSymbolType::Function,
                    symbol_name: Some(symbol.to_string()),
                    scope: scope.map(str::to_string),
                    node_type: None,
                    signature: None,
                    doc: None,
                    modifiers: Vec::new(),
                }),
            },
            file: FileInfo {
                id: file.to_string(),
                absolute_path: file.to_string(),
                ..Default::default()
            },
            sources: Vec::new(),
            recall: Vec::new(),
            evidence: Vec::new(),
            score,
            rank: 0,
            forced: false,
        }
    }

    let files = vec![
        harvest_input("/repo/a.py", "python", "def alpha():\n    shared()\n"),
        harvest_input("/repo/b.py", "python", "def beta():\n    pass\n"),
        harvest_input("/repo/shared.py", "python", "def shared():\n    pass\n"),
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    // Identical RRF scores with a graph edge touching the candidate set
    // (a.py calls shared): the candidate whose symbol/scope overlaps the
    // query roles wins the tiebreak.
    let signals = GraphQuerySignals {
        predicate: vec!["find".to_string()],
        roles: vec!["beta".to_string()],
    };
    let mut candidates = vec![
        candidate("a.py::alpha", "/repo/a.py", "alpha", None, 0.10),
        candidate("b.py::beta", "/repo/b.py", "beta", None, 0.10),
    ];
    graph.rerank(&mut candidates, &signals);
    assert_eq!(candidates[0].id, "b.py::beta");
    assert_eq!(candidates[1].id, "a.py::alpha");
}

#[test]
fn rerank_skips_rrf_order_when_no_edge_touches_candidates() {
    use crate::query::fusion::RecallCandidate;
    use crate::zg_types::{Entity, EntityMetadata, FileInfo, ZgContent, ZgRange};

    fn candidate(id: &str, file: &str, symbol: &str, score: f64) -> RecallCandidate {
        RecallCandidate {
            id: id.to_string(),
            entity: Entity {
                id: id.to_string(),
                file_id: file.to_string(),
                range: ZgRange::File,
                content: ZgContent::Text { text: String::new() },
                metadata: Some(EntityMetadata::Code {
                    symbol_type: crate::zg_types::CodeSymbolType::Function,
                    symbol_name: Some(symbol.to_string()),
                    scope: None,
                    node_type: None,
                    signature: None,
                    doc: None,
                    modifiers: Vec::new(),
                }),
            },
            file: FileInfo {
                id: file.to_string(),
                absolute_path: file.to_string(),
                ..Default::default()
            },
            sources: Vec::new(),
            recall: Vec::new(),
            evidence: Vec::new(),
            score,
            rank: 0,
            forced: false,
        }
    }

    // No imports, no calls anywhere: RRF order stands even though the
    // query roles overlap one candidate.
    let files = vec![
        harvest_input("/repo/a.py", "python", "def alpha():\n    pass\n"),
        harvest_input("/repo/b.py", "python", "def beta():\n    pass\n"),
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    assert!(graph.call_edges().is_empty());
    assert!(graph.import_edges().is_empty());
    let signals = GraphQuerySignals {
        predicate: vec!["find".to_string()],
        roles: vec!["beta".to_string()],
    };
    let mut candidates = vec![
        candidate("a.py::alpha", "/repo/a.py", "alpha", 0.10),
        candidate("b.py::beta", "/repo/b.py", "beta", 0.10),
    ];
    graph.rerank(&mut candidates, &signals);
    assert_eq!(candidates[0].id, "a.py::alpha");
    assert_eq!(candidates[1].id, "b.py::beta");
}
