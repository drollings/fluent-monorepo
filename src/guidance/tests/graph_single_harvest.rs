//! Single-harvest graph equivalence: one parse feeds persisted rows and
//! the in-memory index identically; homonyms stay conservative.

use crate::extractor::code::CodeExtractor;
use crate::extractor::{ChunkOptions, ExtractSource};
use crate::graph_index::{GraphIndex, HarvestInput, ResolutionMethod};
use crate::search_types::FileKind;

fn harvest_once(
    text: &str,
) -> (
    Vec<crate::extractor::PreparedFragment>,
    crate::extractor::code::HarvestedFile,
) {
    let mut parser = crate::ast_parser::AstParser::new();
    let source = ExtractSource {
        file_id: "f".to_string(),
        text: text.to_string(),
        format: "rust".to_string(),
        kind: FileKind::Code,
    };
    let options = ChunkOptions::resolve(None, None).expect("options");
    let (fragments, harvest, _) = CodeExtractor
        .extract_with_harvest(&mut parser, &source, options)
        .expect("harvest");
    (fragments, harvest)
}

#[test]
fn harvest_persisted_and_memory_agree() {
    let text_a = "fn foo() { bar(); }\nfn bar() {}\n";
    let text_b = "fn foo() {}\n";
    let (_fragments_a, harvest_a) = harvest_once(text_a);
    let (_fragments_b, harvest_b) = harvest_once(text_b);

    let db = search_vector::db::GuidanceDb::open_in_memory().expect("memory db");
    let symbols_a: Vec<(String, Vec<String>)> = harvest_a
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.name.clone().unwrap_or_default(),
                symbol.calls.clone(),
            )
        })
        .collect();
    let symbols_b: Vec<(String, Vec<String>)> = harvest_b
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.name.clone().unwrap_or_default(),
                symbol.calls.clone(),
            )
        })
        .collect();
    db.replace_file_graph("/repo/a.rs", &harvest_a.imports, &symbols_a)
        .expect("persist a");
    db.replace_file_graph("/repo/b.rs", &harvest_b.imports, &symbols_b)
        .expect("persist b");

    let edge_rows = db.graph_edge_rows().expect("edges");
    let symbol_rows = db.graph_symbol_rows().expect("symbols");
    let edges: Vec<(String, String)> = edge_rows
        .iter()
        .map(|row| (row.importer.clone(), row.specifier.clone()))
        .collect();
    let symbols: Vec<(String, String, Vec<String>)> = symbol_rows
        .iter()
        .map(|row| (row.name.clone(), row.file.clone(), row.calls.clone()))
        .collect();
    let known = vec!["/repo/a.rs".to_string(), "/repo/b.rs".to_string()];
    let roots = vec!["/repo".to_string()];
    let from_rows =
        GraphIndex::build_from_rows(&known, &edges, &symbols, &roots).expect("from rows");
    let from_harvest = GraphIndex::build(
        &[
            HarvestInput {
                path: "/repo/a.rs".to_string(),
                format: "rust".to_string(),
                text: text_a.to_string(),
            },
            HarvestInput {
                path: "/repo/b.rs".to_string(),
                format: "rust".to_string(),
                text: text_b.to_string(),
            },
        ],
        &roots,
    )
    .expect("from harvest");

    assert_eq!(
        from_rows.definitions_of("foo").len(),
        from_harvest.definitions_of("foo").len()
    );
    assert_eq!(
        from_rows.definitions_of("bar").len(),
        from_harvest.definitions_of("bar").len()
    );
    assert_eq!(
        from_rows.call_edges().len(),
        from_harvest.call_edges().len()
    );
}

#[test]
fn homonym_stays_conservative() {
    let text = "fn foo() {}\n";
    let from_harvest = GraphIndex::build(
        &[
            HarvestInput {
                path: "/repo/a.rs".to_string(),
                format: "rust".to_string(),
                text: text.to_string(),
            },
            HarvestInput {
                path: "/repo/b.rs".to_string(),
                format: "rust".to_string(),
                text: text.to_string(),
            },
        ],
        &["/repo".to_string()],
    )
    .expect("build");
    assert_eq!(from_harvest.definitions_of("foo").len(), 2);
    let edge = from_harvest
        .call_edges()
        .iter()
        .find(|edge| edge.callee == "foo");
    if let Some(edge) = edge {
        assert!(!edge.resolution.is_unique());
        assert!(matches!(
            edge.resolution,
            ResolutionMethod::Homonym { .. } | ResolutionMethod::Unresolved
        ));
    }
}
