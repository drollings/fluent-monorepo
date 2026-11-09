use super::{GraphIndex, HarvestInput, ResolutionMethod};

// Resolution labels name the producer's uncertainty about identity
// resolution (producer-confidence axis). They are never a correctness
// verdict: only the M5 fixture measures the task-value axis.

fn rust_input(path: &str, text: &str) -> HarvestInput {
    HarvestInput {
        path: path.to_string(),
        format: "rust".to_string(),
        text: text.to_string(),
    }
}

fn build(files: &[(&str, &str)]) -> GraphIndex {
    let inputs: Vec<HarvestInput> = files
        .iter()
        .map(|(path, text)| rust_input(path, text))
        .collect();
    GraphIndex::build(&inputs, &["/repo".to_string()]).expect("build")
}

fn edge_resolution(graph: &GraphIndex, callee: &str) -> (ResolutionMethod, Vec<String>) {
    let edges: Vec<_> = graph
        .call_edges()
        .iter()
        .filter(|edge| edge.callee == callee)
        .collect();
    assert_eq!(edges.len(), 1, "one edge per call site: {callee}");
    (edges[0].resolution, edges[0].callee_files.clone())
}

#[test]
fn unique_callee_labels_unique() {
    let graph = build(&[
        ("/repo/caller.rs", "fn outer() {\n    inner();\n}\n"),
        ("/repo/callee.rs", "pub fn inner() {}\n"),
    ]);
    let (resolution, files) = edge_resolution(&graph, "inner");
    assert_eq!(resolution, ResolutionMethod::Unique);
    assert!(resolution.is_unique());
    assert_eq!(files, vec!["/repo/callee.rs".to_string()]);
}

#[test]
fn homonym_callee_labels_count_and_fans_out() {
    let graph = build(&[
        ("/repo/caller.rs", "fn outer() {\n    dup();\n}\n"),
        ("/repo/a.rs", "pub fn dup() {}\n"),
        ("/repo/b.rs", "pub fn dup() {}\n"),
        ("/repo/c.rs", "pub fn dup() {}\n"),
    ]);
    let (resolution, files) = edge_resolution(&graph, "dup");
    assert_eq!(resolution, ResolutionMethod::Homonym { count: 3 });
    assert!(!resolution.is_unique());
    // Expand, never narrow: every candidate file stays reachable.
    assert_eq!(
        files,
        vec![
            "/repo/a.rs".to_string(),
            "/repo/b.rs".to_string(),
            "/repo/c.rs".to_string(),
        ]
    );
    for candidate in &files {
        assert!(
            graph
                .file_dependencies("/repo/caller.rs")
                .contains(candidate),
            "homonym target stays a dependent: {candidate}"
        );
    }
}

#[test]
fn undefined_callee_labels_unresolved() {
    let graph = build(&[("/repo/caller.rs", "fn outer() {\n    ghost();\n}\n")]);
    let (resolution, files) = edge_resolution(&graph, "ghost");
    assert_eq!(resolution, ResolutionMethod::Unresolved);
    assert!(!resolution.is_unique());
    assert!(files.is_empty());
}

#[test]
fn self_only_callee_labels_unique() {
    // One definition site exists (the caller itself): resolution is
    // certain even though no file edge is contributed, exactly as before.
    let graph = build(&[("/repo/solo.rs", "fn s() {\n    s();\n}\n")]);
    let (resolution, files) = edge_resolution(&graph, "s");
    assert_eq!(resolution, ResolutionMethod::Unique);
    assert!(files.is_empty());
}

#[test]
fn expansion_fan_out_unchanged_labels_ride_along() {
    let graph = build(&[
        ("/repo/caller.rs", "fn outer() {\n    inner();\n}\n"),
        ("/repo/callee.rs", "pub fn inner() {}\n"),
    ]);
    let edges = graph.expand_shallow(&["/repo/callee.rs".to_string()]);
    assert_eq!(edges.len(), 1);
    let edge = &edges[0];
    assert_eq!(edge.file, "/repo/caller.rs");
    assert_eq!(edge.anchor, "/repo/callee.rs");
    assert_eq!(edge.via, "inner");
    assert_eq!(edge.resolution, ResolutionMethod::Unique);
    assert!(edge.resolution.render_note().is_none());
}

#[test]
fn render_note_names_ambiguity_only() {
    assert!(ResolutionMethod::Unique.render_note().is_none());
    assert_eq!(
        ResolutionMethod::Homonym { count: 3 }.render_note(),
        Some("ambiguous: 3 candidates".to_string())
    );
    assert_eq!(
        ResolutionMethod::Unresolved.render_note(),
        Some("unresolved".to_string())
    );
}
