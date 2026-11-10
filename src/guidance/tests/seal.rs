//! Seal pins: clean seals Done, findings stick, tool errors surface,
//! and exit codes match the CLI contract.

use std::path::PathBuf;
use std::sync::Arc;

use crate::clippy::ClippyReport;
use crate::fidelity::CfgMatrix;
use crate::gates::TestReport;
use crate::seal::{budgets, discover_packages, run_seal, BuildVerdict, ExitCode, SealCtx};

fn matrix_stub(
    reports: std::collections::HashMap<String, ClippyReport>,
) -> crate::fidelity::MatrixRunner {
    Arc::new(move |_: &std::path::Path, cell: &str| {
        Ok(reports.get(cell).cloned().unwrap_or_default())
    })
}

fn build_ok() -> crate::seal::BuildRunner {
    Arc::new(|package: &std::path::Path| {
        Ok(BuildVerdict {
            package: package.to_string_lossy().into_owned(),
            clean: true,
            output: String::new(),
        })
    })
}

fn tests_ok() -> crate::seal::TestRunner {
    Arc::new(|_: &std::path::Path| {
        Ok(TestReport {
            new_failures: Vec::new(),
            output: String::new(),
        })
    })
}

fn ctx_for(root: PathBuf, clippy: crate::fidelity::MatrixRunner) -> SealCtx {
    SealCtx {
        workspace_root: root,
        cells: CfgMatrix::default_cells(),
        clippy,
        build: build_ok(),
        test: tests_ok(),
        target_dir: None,
    }
}

#[test]
fn clean_workspace_seals_done() {
    let dir = fluent_wvr_testutil::tempdir();
    let package = dir.path().join("pkg");
    std::fs::create_dir_all(&package).expect("mkdir");
    std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
    std::fs::write(package.join("a.rs"), "pub fn a() {}\n").expect("write");
    let cells = CfgMatrix::default_cells();
    let mut reports = std::collections::HashMap::new();
    for cell in &cells {
        reports.insert(cell.clone(), ClippyReport::default());
    }
    let report = run_seal(&ctx_for(dir.path().to_path_buf(), matrix_stub(reports)));
    assert_eq!(report.exit, ExitCode::Done);
    assert!(report.build_ok && report.tests_ok && report.witness_clean);
    assert_eq!(report.packages.len(), 1);
}

#[test]
fn dirty_witness_sticks_with_findings() {
    let dir = fluent_wvr_testutil::tempdir();
    let package = dir.path().join("pkg");
    std::fs::create_dir_all(&package).expect("mkdir");
    std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
    std::fs::write(package.join("a.rs"), "pub fn a() {}\n").expect("write");
    let mut reports = std::collections::HashMap::new();
    reports.insert(
        "default".to_string(),
        ClippyReport {
            diagnostics: vec![crate::clippy::ClippyDiag {
                file: Some(package.join("a.rs").to_string_lossy().into_owned()),
                code: Some("E0308".to_string()),
                message: "mismatch".to_string(),
                level: "error".to_string(),
                suggestions: Vec::new(),
            }],
        },
    );
    let report = run_seal(&ctx_for(dir.path().to_path_buf(), matrix_stub(reports)));
    assert_eq!(report.exit, ExitCode::Stuck);
    assert!(!report.witness_clean);
    assert!(!report.details.is_empty());
}

#[test]
fn tool_errors_surface_as_tool_error() {
    let dir = fluent_wvr_testutil::tempdir();
    let package = dir.path().join("pkg");
    std::fs::create_dir_all(&package).expect("mkdir");
    std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
    let failing: crate::seal::BuildRunner =
        Arc::new(|_| Err(std::io::Error::other("cargo missing")));
    let ctx = SealCtx {
        workspace_root: dir.path().to_path_buf(),
        cells: CfgMatrix::default_cells(),
        clippy: matrix_stub(std::collections::HashMap::new()),
        build: failing,
        test: tests_ok(),
        target_dir: None,
    };
    let report = run_seal(&ctx);
    assert_eq!(report.exit, ExitCode::ToolError);
}

#[test]
fn exit_codes_match_the_contract() {
    assert_eq!(
        [
            ExitCode::Done.as_i32(),
            ExitCode::WorkRemaining.as_i32(),
            ExitCode::Stuck.as_i32(),
            ExitCode::ToolError.as_i32(),
            ExitCode::Halted.as_i32(),
        ],
        [0, 1, 2, 3, 4]
    );
    assert_eq!(discover_packages(std::path::Path::new("/nonexistent-root-xyz")), Vec::<PathBuf>::new());
    const { assert!(budgets::DEF_P50_MS < budgets::DEF_P99_MS) }
}
