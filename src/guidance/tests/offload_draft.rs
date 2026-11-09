use super::DEFAULT_MAX_ATTEMPTS;
use super::{apply_draft, is_test_path, ApplyTool, ApplyVerdict, TriageClass};
use fluent_wvr_testutil::tempdir;

// Generative drafts land only through policy gates: an auto-applicable
// class, a non-test target, and the narrow gate green. The gate certifies;
// the draft never does. Test targets and abstentions are refused before
// anything is staged, and nothing here can commit.

const DRIFTED: &str = "pub fn messy( x : u32 ) -> u32 {\n    x   +   1\n}\n";
const FORMATTED: &str = "pub fn messy(x: u32) -> u32 {\n    x + 1\n}\n";

#[test]
fn draft_with_green_gate_is_kept() {
    let dir = tempdir();
    let file = dir.path().join("src.rs");
    std::fs::write(&file, DRIFTED).expect("write");
    let report = apply_draft(
        &file,
        TriageClass::Formatting,
        FORMATTED,
        &["true".to_string()],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect("draft applies");
    assert_eq!(report.verdict, ApplyVerdict::Kept);
    assert!(report.changed);
    assert_eq!(std::fs::read_to_string(&file).expect("read"), FORMATTED);
}

#[test]
fn draft_with_red_gate_is_reverted() {
    let dir = tempdir();
    let file = dir.path().join("src.rs");
    std::fs::write(&file, DRIFTED).expect("write");
    let report = apply_draft(
        &file,
        TriageClass::ClippyFix,
        FORMATTED,
        &["false".to_string()],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect("draft reports");
    assert_eq!(report.verdict, ApplyVerdict::Reverted);
    assert_eq!(report.attempts_used, DEFAULT_MAX_ATTEMPTS);
    assert_eq!(std::fs::read_to_string(&file).expect("read"), DRIFTED);
}

#[test]
fn test_target_draft_is_refused_untouched() {
    let dir = tempdir();
    let tests = dir.path().join("tests");
    std::fs::create_dir(&tests).expect("mkdir");
    let file = tests.join("target.rs");
    std::fs::write(&file, DRIFTED).expect("write");
    let err = apply_draft(
        &file,
        TriageClass::Formatting,
        FORMATTED,
        &["true".to_string()],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect_err("test targets are refused");
    assert!(err.to_string().contains("test"), "{err}");
    assert_eq!(std::fs::read_to_string(&file).expect("read"), DRIFTED);
}

#[test]
fn abstained_class_draft_is_refused_untouched() {
    let dir = tempdir();
    let file = dir.path().join("src.rs");
    std::fs::write(&file, DRIFTED).expect("write");
    let _err = apply_draft(
        &file,
        TriageClass::Manual,
        FORMATTED,
        &["true".to_string()],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect_err("abstentions never apply");
    assert_eq!(std::fs::read_to_string(&file).expect("read"), DRIFTED);
}

#[test]
fn identical_draft_is_unchanged() {
    let dir = tempdir();
    let file = dir.path().join("src.rs");
    std::fs::write(&file, FORMATTED).expect("write");
    let report = apply_draft(
        &file,
        TriageClass::Formatting,
        FORMATTED,
        &["true".to_string()],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect("draft reports");
    assert_eq!(report.verdict, ApplyVerdict::Unchanged);
    assert!(!report.changed);
}

#[test]
fn test_path_heuristic_names_guarded_shapes() {
    use std::path::Path;
    assert!(is_test_path(Path::new("src/tests/foo.rs")));
    assert!(is_test_path(Path::new("src/testing/foo.rs")));
    assert!(is_test_path(Path::new("src/test_foo.rs")));
    assert!(is_test_path(Path::new("src/foo_test.rs")));
    assert!(is_test_path(Path::new("src/foo_tests.rs")));
    assert!(!is_test_path(Path::new("src/foo.rs")));
    assert!(!is_test_path(Path::new("src/latest.rs")));
    assert!(!is_test_path(Path::new("src/contest.rs")));
}

#[test]
fn tool_names_match_allowlist() {
    assert_eq!(ApplyTool::Rustfmt.name(), "rustfmt");
}
