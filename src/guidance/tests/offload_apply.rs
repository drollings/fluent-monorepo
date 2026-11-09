use super::apply_verified;
use super::{gate_argv, is_whitespace_only, ApplyTool, ApplyVerdict, DEFAULT_MAX_ATTEMPTS};
use fluent_wvr_testutil::tempdir;

// The deterministic applier: stage an isolated copy, run the tool, check
// the diff is mechanical-class, write back, verify with the narrow gate,
// restore byte-identically on red. No model anywhere in this loop.

const DRIFTED: &str = "pub fn messy( x : u32 ) -> u32 {\n    x   +   1\n}\n";
const FORMATTED: &str = "pub fn messy(x: u32) -> u32 {\n    x + 1\n}\n";

fn drifted_file() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir();
    let file = dir.path().join("drift.rs");
    std::fs::write(&file, DRIFTED).expect("write");
    (dir, file)
}

#[test]
fn rustfmt_repairs_drift_with_zero_model_calls() {
    let (_dir, file) = drifted_file();
    let report = apply_verified(
        &file,
        ApplyTool::Rustfmt,
        &["echo".to_string(), "gate-ok".to_string()],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect("applies");
    assert!(report.changed);
    assert_eq!(report.verdict, ApplyVerdict::Kept);
    assert_eq!(report.attempts_used, 1);
    assert_eq!(std::fs::read_to_string(&file).expect("read"), FORMATTED);
    assert!(report.gate_output.contains("gate-ok"));
    assert_eq!(report.tool, "rustfmt");
    assert_ne!(report.before_hash, report.after_hash);
}

#[test]
fn red_gate_restores_bytes_identically() {
    let (_dir, file) = drifted_file();
    let before = std::fs::read(&file).expect("read");
    let report = apply_verified(
        &file,
        ApplyTool::Rustfmt,
        &["false".to_string()],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect("reports");
    assert!(report.changed);
    assert_eq!(report.verdict, ApplyVerdict::Reverted);
    assert_eq!(report.attempts_used, DEFAULT_MAX_ATTEMPTS);
    assert_eq!(
        std::fs::read(&file).expect("read"),
        before,
        "red gate must restore the exact original bytes"
    );
    assert_eq!(report.after_hash, report.before_hash);
}

#[test]
fn already_clean_is_unchanged() {
    let dir = tempdir();
    let file = dir.path().join("clean.rs");
    std::fs::write(&file, FORMATTED).expect("write");
    let report = apply_verified(
        &file,
        ApplyTool::Rustfmt,
        &["true".to_string()],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect("reports");
    assert!(!report.changed);
    assert_eq!(report.verdict, ApplyVerdict::Unchanged);
    assert_eq!(report.attempts_used, 1);
    assert_eq!(std::fs::read_to_string(&file).expect("read"), FORMATTED);
}

#[test]
fn whitespace_only_check_separates_mechanical_diffs() {
    assert!(is_whitespace_only("fn  a( ){}", "fn a() {}"));
    assert!(is_whitespace_only("x", "x"));
    assert!(!is_whitespace_only("fn a() {}", "fn b() {}"));
    assert!(!is_whitespace_only("x + 1", "x + 2"));
}

#[test]
fn gate_argv_maps_matrix_and_package_gates() {
    assert_eq!(gate_argv("guidance-smoke"), vec!["make", "guidance-smoke"]);
    assert_eq!(
        gate_argv("cargo test -p fluent-llm"),
        vec!["cargo", "test", "-p", "fluent-llm"]
    );
    assert_eq!(gate_argv("make smoke"), vec!["make", "smoke"]);
}

#[test]
fn empty_verify_command_errors_without_touching_source() {
    let (_dir, file) = drifted_file();
    let before = std::fs::read(&file).expect("read");
    assert!(apply_verified(&file, ApplyTool::Rustfmt, &[], DEFAULT_MAX_ATTEMPTS).is_err());
    assert_eq!(std::fs::read(&file).expect("read"), before);
}
