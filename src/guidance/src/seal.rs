//! Workspace seal: the human-or-CI gate the loop never runs itself.
//! One matrix clippy per package, one full build per package (past
//! check-level, catching monomorphization and linker errors), full
//! tests against the baseline, and a whole-tree witness pass. Anything
//! less than clean reports instead of fixing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::fidelity::{run_clippy_matrix, MatrixRunner};

/// Process exit codes for the maintenance surface: done, work
/// remaining, stuck with escalations, tool error, or breaker halt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Fixpoint reached; nothing queued, nothing blocked.
    Done,
    /// Interrupted, preempted, or watching: work remains.
    WorkRemaining,
    /// Escalations exist; a human or orchestrator owns them.
    Stuck,
    /// Toolchain or environment failure.
    ToolError,
    /// Circuit breaker halted the run; resumable.
    Halted,
}

impl ExitCode {
    /// Process exit number for the CLI contract.
    #[must_use]
    pub fn as_i32(self) -> i32 {
        match self {
            Self::Done => 0,
            Self::WorkRemaining => 1,
            Self::Stuck => 2,
            Self::ToolError => 3,
            Self::Halted => 4,
        }
    }
}

/// Performance budgets (targets; hardware-dependent). The bench
/// asserts these against the stored baseline with release-blocking
/// regressions through the scorecard ratchet.
pub mod budgets {
    /// Cold definition/reference/outline lookup, ~1M LOC.
    pub const DEF_P50_MS: u64 = 3;
    /// Same, p99.
    pub const DEF_P99_MS: u64 = 10;
    /// Trigram grep.
    pub const GREP_MS: u64 = 20;
    /// Whole-repo status as a stat sweep.
    pub const STATUS_MS: u64 = 50;
    /// Touched-but-identical revalidation (one hash plus one stamp).
    pub const REVALIDATE_MS: u64 = 5;
    /// Context pack assembly.
    pub const PACK_MS: u64 = 50;
    /// Optional semantic recall query.
    pub const RECALL_MS: u64 = 100;
}

/// Full-build verdict for one package (past check-level).
#[derive(Debug, Clone)]
pub struct BuildVerdict {
    /// Package directory.
    pub package: String,
    /// Build clean.
    pub clean: bool,
    /// Captured output for the report.
    pub output: String,
}

/// Full-build runner: one invocation per package. Production shells to
/// cargo; tests inject a stub.
pub type BuildRunner = Arc<dyn Fn(&Path) -> std::io::Result<BuildVerdict> + Send + Sync>;

/// Full-test runner: one invocation per package against the recorded
/// baseline. Production shells to cargo; tests inject a stub.
pub type TestRunner =
    Arc<dyn Fn(&Path) -> std::io::Result<crate::gates::TestReport> + Send + Sync>;

/// Seal inputs: runners plus the matrix to verify.
pub struct SealCtx {
    /// Workspace root to discover packages under.
    pub workspace_root: PathBuf,
    /// Matrix cells for the clippy leg.
    pub cells: Vec<String>,
    /// Cell-aware clippy runner.
    pub clippy: MatrixRunner,
    /// Full-build runner per package.
    pub build: BuildRunner,
    /// Full-test runner per package.
    pub test: TestRunner,
    /// Dedicated cargo target dir for the seal's invocations.
    pub target_dir: Option<PathBuf>,
}

/// Seal outcome for the run report.
#[derive(Debug, Clone)]
pub struct SealReport {
    /// Packages sealed, sorted.
    pub packages: Vec<String>,
    /// Matrix cells verified.
    pub cells: Vec<String>,
    /// Every package built clean.
    pub build_ok: bool,
    /// No new test failures anywhere.
    pub tests_ok: bool,
    /// The witness pass found zero diagnostics.
    pub witness_clean: bool,
    /// Dirty files named by the witness, when any.
    pub witness_dirty: Vec<String>,
    /// Exit code for the CLI contract.
    pub exit: ExitCode,
    /// Human-readable details for the report file.
    pub details: Vec<String>,
}

/// Discover cargo packages under a root: directories holding
/// `Cargo.toml`, sorted. Pure I/O walk, no build graph involved.
#[must_use]
pub fn discover_packages(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    crate::walk::walk_files(root, &["toml"], |path| {
        if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            if let Some(parent) = path.parent() {
                out.push(parent.to_path_buf());
            }
        }
    });
    out.sort();
    out.dedup();
    out
}

/// Rust sources under a package directory for the verified map.
fn package_sources(package: &Path) -> Vec<String> {
    let mut out = Vec::new();
    crate::walk::walk_files(package, &["rs"], |path| {
        out.push(path.to_string_lossy().into_owned());
    });
    out.sort();
    out
}

/// Seal the workspace: matrix clippy per package, full build per
/// package, full tests per package, then a whole-tree witness over the
/// matrix. Reports; never fixes, never loops.
pub fn run_seal(ctx: &SealCtx) -> SealReport {
    let packages = discover_packages(&ctx.workspace_root);
    let mut details = Vec::new();
    let mut build_ok = true;
    let mut tests_ok = true;
    let mut tool_error = false;
    let mut witness_dirty = Vec::new();

    for package in &packages {
        let files = package_sources(package);
        let matrix = run_clippy_matrix(package, &files, &ctx.cells, &ctx.clippy);
        for cell in &matrix.cells {
            if !cell.clean {
                details.push(format!(
                    "{} {}: {}",
                    package.display(),
                    cell.cell,
                    cell.dirty_files.join(", ")
                ));
            }
        }
        match (ctx.build)(package) {
            Ok(verdict) if verdict.clean => {}
            Ok(verdict) => {
                build_ok = false;
                details.push(format!("build {}: {}", package.display(), verdict.output));
            }
            Err(error) => {
                build_ok = false;
                tool_error = true;
                details.push(format!("build tool error {}: {error}", package.display()));
            }
        }
        match (ctx.test)(package) {
            Ok(report) if report.new_failures.is_empty() => {}
            Ok(report) => {
                tests_ok = false;
                details.push(format!(
                    "tests {}: new failures {}",
                    package.display(),
                    report.new_failures.join(", ")
                ));
            }
            Err(error) => {
                tests_ok = false;
                tool_error = true;
                details.push(format!("test tool error {}: {error}", package.display()));
            }
        }
    }

    // Witness: a second full-matrix pass must find zero diagnostics.
    // Done is never declared from the last edit's success alone.
    let mut witness_clean = true;
    for package in &packages {
        let files = package_sources(package);
        let matrix = run_clippy_matrix(package, &files, &ctx.cells, &ctx.clippy);
        for cell in &matrix.cells {
            if !cell.clean {
                witness_clean = false;
                for file in &cell.dirty_files {
                    if !witness_dirty.contains(file) {
                        witness_dirty.push(file.clone());
                    }
                }
            }
        }
    }
    witness_dirty.sort();

    let exit = if tool_error {
        ExitCode::ToolError
    } else if !tests_ok || !witness_clean || !build_ok {
        ExitCode::Stuck
    } else {
        ExitCode::Done
    };
    SealReport {
        packages: packages.iter().map(|path| path.to_string_lossy().into_owned()).collect(),
        cells: ctx.cells.clone(),
        build_ok,
        tests_ok,
        witness_clean,
        witness_dirty,
        exit,
        details,
    }
}

#[cfg(test)]
#[path = "../tests/seal.rs"]
mod seal_tests;
