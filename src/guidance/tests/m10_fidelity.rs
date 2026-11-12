//! Fidelity pins: cfg matrix verification, edge tiers, cfg-gated and
//! detached findings, conservative deletion, and blind-spot gaps.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::clippy::{ClippyDiag, ClippyReport};
use crate::fidelity::{
    blind_spot_gaps, run_clippy_matrix, safe_to_delete, scan_fidelity, CfgMatrix, DeleteVerdict,
    FidelityKind, MatrixRunner,
};

fn runner_for(
    reports: HashMap<PathBuf, HashMap<String, ClippyReport>>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
) -> MatrixRunner {
    use std::sync::atomic::Ordering;
    Arc::new(move |package: &std::path::Path, cell: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(reports
            .get(package)
            .and_then(|cells| cells.get(cell))
            .cloned()
            .unwrap_or_default())
    })
}

fn cell_reports(report: &ClippyReport) -> HashMap<String, ClippyReport> {
    CfgMatrix::default_cells()
        .into_iter()
        .map(|cell| (cell, report.clone()))
        .collect()
}

fn clean_report() -> ClippyReport {
    ClippyReport::default()
}

fn dirty_report(file: &str) -> ClippyReport {
    ClippyReport {
        diagnostics: vec![ClippyDiag {
            file: Some(file.to_string()),
            code: Some("E0308".to_string()),
            message: "mismatch".to_string(),
            level: "error".to_string(),
            suggestions: Vec::new(),
        }],
    }
}

#[test]
fn matrix_verifies_per_file_per_cell() {
    let dir = fluent_wvr_testutil::tempdir();
    let package = dir.path().join("pkg");
    std::fs::create_dir_all(&package).expect("mkdir");
    std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
    let cells = CfgMatrix::default_cells();
    assert_eq!(
        cells,
        vec!["default", "all-features", "no-default-features"]
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let file = package.join("src/a.rs").to_string_lossy().into_owned();
    let files = vec![file.clone()];
    let reports = [(package.clone(), cell_reports(&clean_report()))]
        .into_iter()
        .collect();
    let matrix = run_clippy_matrix(
        &package,
        &files,
        &cells,
        &runner_for(reports, Arc::clone(&calls)),
    );
    assert_eq!(matrix.cells.len(), 3);
    assert!(matrix.cells.iter().all(|cell| cell.clean));
    assert_eq!(
        matrix.verified.get(&file).map(Vec::len),
        Some(3),
        "clean files verify in every cell"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "one invocation per matrix cell"
    );
    // A dirty cell narrows verification instead of claiming clean.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut dirty_cells = HashMap::new();
    dirty_cells.insert("default".to_string(), dirty_report("src/a.rs"));
    let reports = [(package.clone(), dirty_cells)].into_iter().collect();
    let file = package.join("src/a.rs").to_string_lossy().into_owned();
    let files = vec![file.clone()];
    let matrix = run_clippy_matrix(
        &package,
        &files,
        &["default".to_string()],
        &runner_for(reports, calls),
    );
    assert!(!matrix.cells[0].clean);
    assert!(matrix.cells[0]
        .dirty_files
        .contains(&"src/a.rs".to_string()));
    assert!(!matrix.verified.contains_key(&file));
}

#[test]
fn edge_tiers_label_every_edge() {
    use crate::graph_index::{EdgeTier, GraphIndex, HarvestInput};
    let inputs = vec![
        HarvestInput {
            path: "a.rs".to_string(),
            format: "rust".to_string(),
            text: "pub fn foo() -> u64 {\n    1\n}\n".to_string(),
        },
        HarvestInput {
            path: "b.rs".to_string(),
            format: "rust".to_string(),
            text: "use crate::a::foo;\npub fn bar() -> u64 {\n    foo()\n}\n".to_string(),
        },
    ];
    let graph = GraphIndex::build(&inputs, &[".".to_string()]).expect("build");
    assert!(!graph.call_edges().is_empty() || !graph.import_edges().is_empty());
    for edge in graph.call_edges() {
        assert_eq!(edge.tier, EdgeTier::Syntactic);
    }
    for edge in graph.import_edges() {
        assert_eq!(edge.tier, EdgeTier::Syntactic);
    }
    let report = crate::query::context_pack::impact_report(&graph, &["a.rs".to_string()]);
    assert!(report.tiers.contains(&"syntactic".to_string()));
}

#[test]
fn cfg_gated_and_detached_files_are_findings_not_clean() {
    let dir = fluent_wvr_testutil::tempdir();
    let root = dir.path().join("pkg");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
    let gated = root.join("gated.rs");
    std::fs::write(&gated, "#[cfg(feature = \"nightly\")]\npub fn f() {}\n").expect("write");
    let plain = root.join("plain.rs");
    std::fs::write(&plain, "pub fn g() {}\n").expect("write");
    let stray = dir.path().join("stray.rs");
    std::fs::write(&stray, "pub fn s() {}\n").expect("write");
    let files = vec![
        (
            gated.to_string_lossy().into_owned(),
            std::fs::read_to_string(&gated).expect("read"),
        ),
        (
            plain.to_string_lossy().into_owned(),
            std::fs::read_to_string(&plain).expect("read"),
        ),
        (
            stray.to_string_lossy().into_owned(),
            std::fs::read_to_string(&stray).expect("read"),
        ),
    ];
    let findings = scan_fidelity(&files);
    assert!(
        findings
            .iter()
            .any(|f| f.kind == FidelityKind::UnverifiedCfg && f.path.ends_with("gated.rs")),
        "cfg-gated items surface: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f.kind == FidelityKind::Detached && f.path.ends_with("stray.rs")),
        "package-less files surface: {findings:?}"
    );
    assert!(
        findings.iter().all(|f| !f.path.ends_with("plain.rs")),
        "clean files stay silent: {findings:?}"
    );
}

#[test]
fn deletion_answers_are_never_yes() {
    assert_eq!(
        safe_to_delete(false, &["a.rs".to_string()], true),
        DeleteVerdict::No
    );
    assert_eq!(
        safe_to_delete(true, &[], true),
        DeleteVerdict::UnreferencedWithCaveats
    );
    assert_eq!(safe_to_delete(true, &[], false), DeleteVerdict::Unknown);
    // Public surface is never deletable by index evidence alone.
    assert_eq!(safe_to_delete(false, &[], true), DeleteVerdict::Unknown);
}

#[test]
fn blind_spots_report_as_gaps_at_full_recall() {
    let queries = [
        ("who calls the macro?", vec!["macro"]),
        ("what does build.rs generate?", vec!["build"]),
        ("is this dyn dispatch?", vec!["dyn"]),
        ("is this cfg-gated?", vec!["cfg"]),
        ("where is foo defined?", vec![]),
    ];
    for (query, markers) in queries {
        let gaps = blind_spot_gaps(query);
        let empty = markers.is_empty();
        for marker in markers {
            assert!(
                gaps.iter().any(|gap| gap.contains(marker)),
                "blind spot must surface for {query:?}: {gaps:?}"
            );
        }
        if empty {
            assert!(gaps.is_empty(), "plain queries stay gap-free: {gaps:?}");
        }
    }
}

#[test]
fn tier_labels_survive_rendering() {
    use crate::graph_index::EdgeTier;
    assert_eq!(EdgeTier::Syntactic.as_str(), "syntactic");
    assert_eq!(EdgeTier::Resolved.as_str(), "resolved");
}
