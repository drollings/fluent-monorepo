use super::*;

// Persisted dependency-graph inputs: `graph_edges` + `graph_symbols`
// round-trip through `replace_file_graph`, stale rows clear on rewrite,
// and `delete_file` / `drop_index` remove graph rows with the file.

fn graph_db() -> GuidanceDb {
    GuidanceDb::open_in_memory().expect("db")
}

#[test]
fn edge_and_symbol_rows_round_trip() {
    let db = graph_db();
    db.replace_file_graph(
        "/repo/src/a.ts",
        &["./b".to_string(), "pkg.sub".to_string()],
        &[("alpha".to_string(), vec!["beta".to_string()])],
    )
    .expect("write");

    let mut edges = db.graph_edge_rows().expect("read edges");
    edges.sort_by(|a, b| a.specifier.cmp(&b.specifier));
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0].importer, "/repo/src/a.ts");
    assert_eq!(edges[0].specifier, "./b");
    assert_eq!(edges[0].resolved, None);
    assert_eq!(edges[1].specifier, "pkg.sub");

    let symbols = db.graph_symbol_rows().expect("read symbols");
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "alpha");
    assert_eq!(symbols[0].file, "/repo/src/a.ts");
    assert_eq!(symbols[0].calls, vec!["beta".to_string()]);
}

#[test]
fn rewrite_clears_stale_graph_rows() {
    let db = graph_db();
    db.replace_file_graph(
        "/repo/src/a.ts",
        &["./b".to_string(), "./stale".to_string()],
        &[
            ("alpha".to_string(), vec!["beta".to_string()]),
            ("gone".to_string(), Vec::new()),
        ],
    )
    .expect("write");
    db.replace_file_graph("/repo/src/a.ts", &["./b".to_string()], &[])
        .expect("rewrite");

    let edges = db.graph_edge_rows().expect("read edges");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].specifier, "./b");
    assert!(db.graph_symbol_rows().expect("read symbols").is_empty());
}

#[test]
fn duplicate_symbol_names_union_calls() {
    // Overloads share one (name, file) row; every def's calls survive.
    let db = graph_db();
    db.replace_file_graph(
        "/repo/src/a.rs",
        &[],
        &[
            ("run".to_string(), vec!["one".to_string()]),
            ("run".to_string(), vec!["two".to_string()]),
        ],
    )
    .expect("write");

    let symbols = db.graph_symbol_rows().expect("read symbols");
    assert_eq!(symbols.len(), 1);
    let mut calls = symbols[0].calls.clone();
    calls.sort();
    assert_eq!(calls, vec!["one".to_string(), "two".to_string()]);
}

#[test]
fn delete_file_removes_graph_rows() {
    let db = graph_db();
    let file = ZgFileRecord {
        id: "file-a".to_string(),
        absolute_path: "/repo/src/a.ts".to_string(),
        relative_path: "src/a.ts".to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 10,
        last_modified_time: 10,
        kind: Some("code".to_string()),
        format: "typescript".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    db.replace_file(&file, &[], &[]).expect("fragments");
    db.replace_file_graph("/repo/src/a.ts", &["./b".to_string()], &[(
        "alpha".to_string(),
        Vec::new(),
    )])
    .expect("graph");

    db.delete_file("file-a").expect("delete");
    assert!(db.graph_edge_rows().expect("edges").is_empty());
    assert!(db.graph_symbol_rows().expect("symbols").is_empty());
}

#[test]
fn drop_index_clears_graph_tables() {
    let db = graph_db();
    db.replace_file_graph("/repo/src/a.ts", &["./b".to_string()], &[(
        "alpha".to_string(),
        Vec::new(),
    )])
    .expect("graph");

    db.drop_index().expect("drop");
    assert!(db.graph_edge_rows().expect("edges").is_empty());
    assert!(db.graph_symbol_rows().expect("symbols").is_empty());
}
