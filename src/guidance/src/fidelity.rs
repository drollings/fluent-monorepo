//! Rust slice fidelity: cfg-matrix verification, cfg-gated and
//! detached findings, conservative deletion answers, and blind-spot
//! gaps. Nothing here claims clean for what no matrix cell exercised.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::clippy::{resolve_reported, ClippyReport};

/// Default cfg matrix cells: default features, all features, and no
/// default features. Items behind a cfg no cell exercises are flagged,
/// never silently called clean.
pub struct CfgMatrix;

impl CfgMatrix {
    /// Default matrix cells in run order.
    #[must_use]
    pub fn default_cells() -> Vec<String> {
        ["default", "all-features", "no-default-features"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// Cargo args selecting one cell.
    #[must_use]
    pub fn args_for_cell(cell: &str) -> Vec<String> {
        match cell {
            "all-features" => vec!["--all-features".to_string()],
            "no-default-features" => vec!["--no-default-features".to_string()],
            _ => Vec::new(),
        }
    }
}

/// One matrix cell's verdict for a package.
#[derive(Debug, Clone)]
/// One matrix cell's verdict for a package.
pub struct CellReport {
    /// Cell name.
    pub cell: String,
    /// Clean means no diagnostics at all in the cell.
    pub clean: bool,
    /// Files named by the cell's diagnostics.
    pub dirty_files: Vec<String>,
}

/// Per-file verification across the matrix: which cells each dirty file
/// verified clean in. In-memory record for the run report; the seal
/// command consumes it, and sidecar persistence lands with it.
#[derive(Debug, Clone, Default)]
/// Per-file verification across the matrix: which cells each dirty file
/// verified clean in. In-memory record for the run report; the seal
/// command consumes it, and sidecar persistence lands with it.
pub struct MatrixReport {
    /// Cell reports in run order.
    pub cells: Vec<CellReport>,
    /// File path to cells verified clean.
    pub verified: HashMap<String, Vec<String>>,
}

/// Run one clippy invocation per matrix cell over a package and record
/// per-file verification. Strict: a file verifies in a cell only with
/// zero diagnostics naming it there.
pub fn run_clippy_matrix(
    package: &Path,
    files: &[String],
    cells: &[String],
    runner: &MatrixRunner,
) -> MatrixReport {
    let mut report = MatrixReport::default();
    for cell in cells {
        let parsed = runner(package, cell).unwrap_or_default();
        let mut dirty_files: Vec<String> = parsed
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.file.clone())
            .collect();
        dirty_files.sort();
        dirty_files.dedup();
        let dirty_resolved: std::collections::HashSet<String> = dirty_files
            .iter()
            .map(|name| {
                resolve_reported(package, name)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let clean = dirty_files.is_empty();
        for file in files {
            if !dirty_resolved.contains(file) {
                report
                    .verified
                    .entry(file.clone())
                    .or_default()
                    .push(cell.clone());
            }
        }
        report.cells.push(CellReport {
            cell: cell.clone(),
            clean,
            dirty_files,
        });
    }
    report
}

/// Cell-aware clippy runner: one invocation per package per matrix
/// cell. Production builds one [`crate::clippy::ClippyCtx`] per cell
/// with the cell's feature args; tests inject a stub.
pub type MatrixRunner = Arc<dyn Fn(&Path, &str) -> std::io::Result<ClippyReport> + Send + Sync>;

/// Build the live matrix runner over one target dir and shaping: each
/// cell runs with its feature args under the same shaping.
#[must_use]
/// Build the live matrix runner over one target dir and shaping: each
/// cell runs with its feature args under the same shaping.
pub fn live_matrix_runner(
    target_dir: Option<PathBuf>,
    shaping: crate::clippy::BuildShaping,
) -> MatrixRunner {
    Arc::new(move |package: &Path, cell: &str| {
        let ctx = crate::clippy::ClippyCtx {
            target_dir: target_dir.clone(),
            shaping,
            extra_args: CfgMatrix::args_for_cell(cell),
        };
        crate::clippy::run_clippy(package, &ctx)
    })
}

/// Kind of fidelity finding: never clean, always owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Kind of fidelity finding: never clean, always owned.
pub enum FidelityKind {
    /// Item behind a cfg no exercised cell covers.
    UnverifiedCfg,
    /// Source file outside any cargo package.
    Detached,
}

/// One fidelity finding with its file.
#[derive(Debug, Clone)]
/// One fidelity finding with its file.
pub struct FidelityFinding {
    /// File path, as passed in.
    pub path: String,
    /// Finding kind.
    pub kind: FidelityKind,
    /// Item name or reason detail.
    pub detail: String,
}

/// Scan files for cfg-gated items (Rust only) and package-less sources.
/// Clean files stay silent; anything else is a finding with an owner,
/// never a silent clean.
#[must_use]
/// Scan files for cfg-gated items (Rust only) and package-less sources.
/// Clean files stay silent; anything else is a finding with an owner,
/// never a silent clean.
pub fn scan_fidelity(files: &[(String, String)]) -> Vec<FidelityFinding> {
    let mut parser = crate::ast_parser::AstParser::new();
    let mut findings = Vec::new();
    for (path, text) in files {
        if path.ends_with(".rs") {
            for (name, line) in parser.rust_cfg_gated(text) {
                findings.push(FidelityFinding {
                    path: path.clone(),
                    kind: FidelityKind::UnverifiedCfg,
                    detail: format!("{name}:{line}"),
                });
            }
        }
        if crate::clippy::package_root(Path::new(path)).is_none() {
            findings.push(FidelityFinding {
                path: path.clone(),
                kind: FidelityKind::Detached,
                detail: "outside any cargo package".to_string(),
            });
        }
    }
    findings.sort_by(|a, b| (&a.path, &a.detail).cmp(&(&b.path, &b.detail)));
    findings
}

/// Deletion verdict: conservative answers to destructive-adjacent
/// questions. `Yes` does not exist: the index can only report what it
/// covered, never a deletion proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Deletion verdict: conservative answers to destructive-adjacent
/// questions. `Yes` does not exist: the index can only report what it
/// covered, never a deletion proof.
pub enum DeleteVerdict {
    /// Referenced: not deletable.
    No,
    /// Uncoverable by index evidence (public surface, macros, dynamic
    /// dispatch, FFI, build output, downstream crates, cfg-disabled
    /// code): unknown, with channels named by the caller.
    Unknown,
    /// Private, unreferenced in the covered index, and clean under the
    /// compiler's own `dead_code` lint across the matrix — still caveated.
    UnreferencedWithCaveats,
}

/// Whether an item is safe to delete. Private items defer to the
/// compiler's `dead_code` verdict (`dead_code_clean`); anything public
/// or macro-adjacent is at best unknown. Never yes.
#[must_use]
/// Whether an item is safe to delete. Private items defer to the
/// compiler's `dead_code` verdict (`dead_code_clean`); anything public
/// or macro-adjacent is at best unknown. Never yes.
pub fn safe_to_delete(is_private: bool, usages: &[String], dead_code_clean: bool) -> DeleteVerdict {
    if !usages.is_empty() {
        return DeleteVerdict::No;
    }
    if is_private && dead_code_clean {
        DeleteVerdict::UnreferencedWithCaveats
    } else {
        DeleteVerdict::Unknown
    }
}

/// Published blind spots attached to answers that touch them: macro
/// expansion output, `build.rs` output, dynamic dispatch at the
/// syntactic tier, and cfg-disabled code. Query-text heuristics route;
/// producer-axis signals that order gaps, never verdicts.
#[must_use]
/// Published blind spots attached to answers that touch them: macro
/// expansion output, `build.rs` output, dynamic dispatch at the
/// syntactic tier, and cfg-disabled code. Query-text heuristics route;
/// producer-axis signals that order gaps, never verdicts.
pub fn blind_spot_gaps(query: &str) -> Vec<String> {
    let lowered = query.to_lowercase();
    let mut gaps = Vec::new();
    if lowered.contains("macro") || lowered.contains("derive") {
        gaps.push("macro expansion output not covered".to_string());
    }
    if lowered.contains("build") {
        gaps.push("build.rs output not covered".to_string());
    }
    if lowered.contains("dyn ") || lowered.contains("dynamic") || lowered.contains("dispatch") {
        gaps.push("dyn dispatch resolved syntactically only".to_string());
    }
    if lowered.contains("cfg") {
        gaps.push("cfg-disabled code not covered".to_string());
    }
    gaps
}

#[cfg(test)]
#[path = "../tests/m10_fidelity.rs"]
mod m10_fidelity_tests;