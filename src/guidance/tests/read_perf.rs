//! Read-plane performance pins: no pipeline build on the
//! extractive path, glob matchers compiled once per selection (not
//! per file), and section budgets that cap rendered output.
//! Included via `#[path]` forwarder, per repo convention. Hermetic:
//! no model, no endpoint; the pipeline-build skip is structural.

use crate::query::ingest::LazyNlp;

#[test]
fn extractive_path_builds_no_pipeline() {
    // The skip branch never touches the constructor: a model-free
    // query pays zero pipeline cost by construction, not by cache
    // luck. (The build branch is exercised by the ingest tests.)
    let holder = LazyNlp::new();
    assert!(holder.get_unless(true).is_none());
}

#[test]
fn glob_matchers_compile_once_per_selection() {
    use crate::selection::FileSelection;
    // Warm up the shared default matchers so the counter below
    // observes only this selection's own globs.
    let warm = tempfile::tempdir().expect("tempdir");
    std::fs::write(warm.path().join("w.rs"), "fn w() {}\n").expect("write");
    let warm_selection = FileSelection {
        workspace_root: warm.path().to_path_buf(),
        include_fixtures: false,
        roots: vec![warm.path().to_path_buf()],
        include_globs: vec![],
        exclude_globs: vec![],
        extra_skip_dirs: Vec::new(),
        honor_gitignore: false,
        max_bytes_override: None,
    };
    let mut warm_diag = crate::selection::ScanDiagnostics::default();
    crate::selection::select_files(&warm_selection, &mut warm_diag).expect("warm");
    crate::query::glob::reset_compile_count();
    let build = |count: usize| {
        let dir = tempfile::tempdir().expect("tempdir");
        for index in 0..count {
            std::fs::write(dir.path().join(format!("f{index}.rs")), "fn f() {}\n").expect("write");
        }
        let selection = FileSelection {
            workspace_root: dir.path().to_path_buf(),
            include_fixtures: false,
            roots: vec![dir.path().to_path_buf()],
            include_globs: vec!["*.rs".to_string(), "f*.rs".to_string()],
            exclude_globs: vec!["*.md".to_string()],
            extra_skip_dirs: Vec::new(),
            honor_gitignore: false,
            max_bytes_override: None,
        };
        let mut diag = crate::selection::ScanDiagnostics::default();
        let selected = crate::selection::select_files(&selection, &mut diag).expect("select");
        (selected.len(), dir)
    };
    // Same globs, wildly different file counts: the compile count is
    // identical (matchers build once per selection, never per file).
    // Keep the tempdirs alive until after both selections.
    crate::query::glob::reset_compile_count();
    let (one, _keep_one) = build(1);
    let count_one = crate::query::glob::compile_count();
    assert_eq!(one, 1);
    crate::query::glob::reset_compile_count();
    let (many, _keep_many) = build(20);
    let count_many = crate::query::glob::compile_count();
    assert_eq!(many, 20);
    assert_eq!(count_one, count_many, "per-file construction would scale");
    assert_eq!(count_many, 3, "two includes plus one exclude, built once");
}

#[test]
fn section_budgets_cap_rendered_output() {
    use crate::read::engine::{cap_findings_by_budget, Finding, FindingKind, FindingProducer};
    // Whole findings only: the first always renders, later ones only
    // while they fit. A tiny code budget keeps the head finding and
    // drops the tail; a large one keeps everything.
    let finding = |id: &str, words: usize| Finding {
        id: id.to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: crate::source_ref::SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new("src/a.rs"),
        ),
        start_line: Some(1),
        end_line: Some(2),
        text: "word ".repeat(words),
        producer_score: 1.0,
        provenance: "code".to_string(),
    };
    let findings = vec![finding("a", 400), finding("b", 400), finding("c", 400)];
    let full = cap_findings_by_budget(&findings, 10000);
    assert_eq!(full.len(), 3);
    let capped = cap_findings_by_budget(&findings, 100);
    assert_eq!(capped.len(), 1, "only the head finding fits: {capped:?}");
    assert!(capped[0].text.len() < full.iter().map(|f| f.text.len()).sum::<usize>());
    // Empty input and zero budget keep the contract: nothing to show,
    // but never a mid-fence cut.
    assert!(cap_findings_by_budget(&[], 100).is_empty());
    let zero = cap_findings_by_budget(&findings, 0);
    assert_eq!(zero.len(), 1, "the first finding always renders");
}
