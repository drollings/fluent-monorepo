//! P3 `graph_index` tests: per-language import harvesting, specifier
//! resolution, dependents closures over imports + calls, and deterministic
//! L4 re-rank with role-coverage tiebreak.

use crate::extractor::adapter::{format_for_extension, resolve_adapter};
use crate::extractor::code::harvest_file_entities;
use crate::extractor::ExtractSource;
use crate::graph_index::{
    ContextDirection, ContextFamily, GraphIndex, GraphQuerySignals, HarvestInput,
};
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

fn persist_harvest(
    db: &search_vector::db::GuidanceDb,
    parser: &mut AstParser,
    path: &str,
    format: &str,
    text: &str,
) {
    // Exactly what the ingest path persists: one harvest, no second parse.
    let source = ExtractSource {
        file_id: "test".to_string(),
        text: text.to_string(),
        format: format.to_string(),
        kind: FileKind::Code,
    };
    let harvested = harvest_file_entities(parser, &source).expect("harvest");
    let symbols: Vec<(String, Vec<String>)> = harvested
        .symbols
        .iter()
        .map(|s| (s.name.clone().unwrap_or_default(), s.calls.clone()))
        .collect();
    db.replace_file_graph(path, &harvested.imports, &symbols).expect("persist");
}

fn sorted_imports(graph: &GraphIndex) -> Vec<(String, String, Option<String>)> {
    let mut edges: Vec<_> = graph
        .import_edges()
        .iter()
        .map(|e| (e.importer.clone(), e.specifier.clone(), e.resolved_file.clone()))
        .collect();
    edges.sort();
    edges
}

fn sorted_calls(graph: &GraphIndex) -> Vec<(String, Option<String>, String, Vec<String>)> {
    let mut edges: Vec<_> = graph
        .call_edges()
        .iter()
        .map(|e| {
            let mut targets = e.callee_files.clone();
            targets.sort();
            (e.caller_file.clone(), e.caller_symbol.clone(), e.callee.clone(), targets)
        })
        .collect();
    edges.sort();
    edges
}

fn sorted_defs(graph: &GraphIndex, symbols: &[&str]) -> Vec<(String, Vec<String>)> {
    symbols
        .iter()
        .map(|s| {
            let mut files: Vec<String> =
                graph.definitions_of(s).iter().map(|d| d.file.clone()).collect();
            files.sort();
            (s.to_string(), files)
        })
        .collect()
}

#[test]
fn hydrated_graph_equals_harvested_graph() {
    let roots = vec!["/repo".to_string()];
    let corpus = vec![
        ("/repo/parent.rs", "rust", "mod child;\nfn alpha() {\n    helper();\n}\n"),
        ("/repo/child.rs", "rust", "pub fn helper() {}\n"),
        ("/repo/main.py", "python", "import os\ndef gamma():\n    pass\n"),
    ];
    let inputs: Vec<HarvestInput> = corpus
        .iter()
        .map(|(p, f, t)| harvest_input(p, f, t))
        .collect();
    let harvested = GraphIndex::build(&inputs, &roots).expect("harvested build");
    assert!(!harvested.import_edges().is_empty());
    assert!(!harvested.call_edges().is_empty());

    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let mut parser = AstParser::new();
    for (path, format, text) in &corpus {
        persist_harvest(&db, &mut parser, path, format, text);
    }
    let edges: Vec<(String, String)> = db
        .graph_edge_rows()
        .expect("edges")
        .into_iter()
        .map(|r| (r.importer, r.specifier))
        .collect();
    let symbols: Vec<(String, String, Vec<String>)> = db
        .graph_symbol_rows()
        .expect("symbols")
        .into_iter()
        .map(|r| (r.name, r.file, r.calls))
        .collect();
    let known: Vec<String> = corpus.iter().map(|(p, _, _)| p.to_string()).collect();
    let hydrated = GraphIndex::build_from_rows(&known, &edges, &symbols, &roots).expect("hydrated");

    assert_eq!(sorted_imports(&harvested), sorted_imports(&hydrated));
    assert_eq!(sorted_calls(&harvested), sorted_calls(&hydrated));
    assert_eq!(
        sorted_defs(&harvested, &["alpha", "helper", "gamma"]),
        sorted_defs(&hydrated, &["alpha", "helper", "gamma"])
    );
    for path in &known {
        assert_eq!(harvested.file_dependencies(path), hydrated.file_dependencies(path), "{path}");
        assert_eq!(
            harvested.dependents_closure(&[path.clone()]),
            hydrated.dependents_closure(&[path.clone()]),
            "{path}"
        );
    }
}

#[test]
fn hydrated_build_costs_less_than_reparse_per_file() {
    // M2 calibration probe: row-read hydration vs full re-parse, per file.
    let roots = vec!["/repo".to_string()];
    let mut corpus: Vec<(String, String, String)> = Vec::new();
    for i in 0..60 {
        let path = format!("/repo/m{i:02}.rs");
        let text = format!(
            "use crate::m{i:02}dep;\nfn f{i:02}() {{\n    g{i:02}();\n}}\nfn g{i:02}() {{}}\n"
        );
        corpus.push((path, "rust".to_string(), text));
    }
    let inputs: Vec<HarvestInput> = corpus
        .iter()
        .map(|(p, f, t)| harvest_input(p, f, t))
        .collect();

    let start = std::time::Instant::now();
    let harvested = GraphIndex::build(&inputs, &roots).expect("harvested build");
    let reparse_ns = start.elapsed().as_nanos();

    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let mut parser = AstParser::new();
    for (path, format, text) in &corpus {
        persist_harvest(&db, &mut parser, path, format, text);
    }
    let edges: Vec<(String, String)> = db
        .graph_edge_rows()
        .expect("edges")
        .into_iter()
        .map(|r| (r.importer, r.specifier))
        .collect();
    let symbols: Vec<(String, String, Vec<String>)> = db
        .graph_symbol_rows()
        .expect("symbols")
        .into_iter()
        .map(|r| (r.name, r.file, r.calls))
        .collect();
    let known: Vec<String> = corpus.iter().map(|(p, _, _)| p.to_string()).collect();

    let start = std::time::Instant::now();
    let hydrated = GraphIndex::build_from_rows(&known, &edges, &symbols, &roots).expect("hydrated");
    let hydrated_ns = start.elapsed().as_nanos();

    assert_eq!(sorted_imports(&harvested), sorted_imports(&hydrated));
    eprintln!(
        "hydration probe: re-parse {} ns/file, hydrated {} ns/file (n={})",
        reparse_ns / 60,
        hydrated_ns / 60,
        60
    );
    assert!(
        hydrated_ns < reparse_ns,
        "hydration must be strictly cheaper than re-parse: {hydrated_ns} vs {reparse_ns}"
    );
}

#[test]
fn plain_exports_emit_no_import_edges() {
    // `export function/class` declarations are not imports: string
    // literals in their bodies (here "needle") must never become edges.
    let harvested = harvest(
        "typescript",
        "export function findNeedle1(haystack: string[]): string[] {\n  return haystack.filter((line) => line.includes(\"needle\"));\n}\nexport class SearchService1 {\n  constructor(private name: string) {}\n}\n",
    );
    assert!(harvested.imports.is_empty(), "{:?}", harvested.imports);
}

#[test]
fn shallow_expansion_imports_only_fires_with_provenance() {
    // No calls anywhere: the dependent arrives via the import edge alone,
    // carrying its specifier as provenance.
    let files = vec![
        harvest_input("/repo/user.rs", "rust", "mod base;\nfn use_it() {}\n"),
        harvest_input("/repo/base.rs", "rust", "pub fn foundation() {}\n"),
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    assert!(graph.call_edges().is_empty());
    let edges = graph.expand_shallow(&["/repo/base.rs".to_string()]);
    assert_eq!(edges.len(), 1);
    let edge = &edges[0];
    assert_eq!(edge.file, "/repo/user.rs");
    assert_eq!(edge.anchor, "/repo/base.rs");
    assert_eq!(edge.direction, ContextDirection::Dependent);
    assert_eq!(edge.family, ContextFamily::Import);
    assert_eq!(edge.via, "mod:base");
}

#[test]
fn shallow_expansion_calls_only_fires_with_provenance() {
    // No imports anywhere: the caller arrives via the call edge alone.
    let files = vec![
        harvest_input("/repo/caller.rs", "rust", "fn outer() {\n    inner();\n}\n"),
        harvest_input("/repo/callee.rs", "rust", "pub fn inner() {}\n"),
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    assert!(graph.import_edges().is_empty());
    let edges = graph.expand_shallow(&["/repo/callee.rs".to_string()]);
    assert_eq!(edges.len(), 1);
    let edge = &edges[0];
    assert_eq!(edge.file, "/repo/caller.rs");
    assert_eq!(edge.anchor, "/repo/callee.rs");
    assert_eq!(edge.direction, ContextDirection::Dependent);
    assert_eq!(edge.family, ContextFamily::Call);
    assert_eq!(edge.via, "inner");
}

#[test]
fn shallow_expansion_covers_dependencies_and_stays_depth_one() {
    // a imports b and calls into it; b calls ext. From anchor a: b is a
    // direct dependency (import + call edges); ext is depth-2 and absent.
    // From anchor b: a is a direct dependent; ext a direct dependency.
    let files = vec![
        harvest_input("/repo/a.rs", "rust", "mod b;\nfn fa() {\n    fb();\n}\n"),
        harvest_input("/repo/b.rs", "rust", "pub fn fb() {\n    ext();\n}\n"),
        harvest_input("/repo/ext.rs", "rust", "pub fn ext() {}\n"),
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    let from_a = graph.expand_shallow(&["/repo/a.rs".to_string()]);
    assert!(from_a.iter().any(|e| e.file == "/repo/b.rs"
        && e.direction == ContextDirection::Dependency
        && e.family == ContextFamily::Import));
    assert!(from_a.iter().any(|e| e.file == "/repo/b.rs"
        && e.direction == ContextDirection::Dependency
        && e.family == ContextFamily::Call
        && e.via == "fb"));
    assert!(!from_a.iter().any(|e| e.file == "/repo/ext.rs"), "{from_a:?}");
    let from_b = graph.expand_shallow(&["/repo/b.rs".to_string()]);
    assert!(from_b.iter().any(|e| e.file == "/repo/a.rs"
        && e.direction == ContextDirection::Dependent));
    assert!(from_b.iter().any(|e| e.file == "/repo/ext.rs"
        && e.direction == ContextDirection::Dependency));
}

#[test]
fn shallow_expansion_without_edges_is_empty() {
    let files = vec![harvest_input("/repo/solo.rs", "rust", "pub fn alone() {}\n")];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    assert!(graph.expand_shallow(&["/repo/solo.rs".to_string()]).is_empty());
    assert!(graph.expand_shallow(&[]).is_empty());
    // Unresolved specifiers name no file and contribute nothing.
    let files = vec![harvest_input("/repo/u.rs", "rust", "use nowhere::thing;\nfn f() {}\n")];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    assert!(!graph.import_edges().is_empty());
    assert!(graph.expand_shallow(&["/repo/u.rs".to_string()]).is_empty());
}
