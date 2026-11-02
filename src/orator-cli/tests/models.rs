//! `models verify` reporting: named unverifiable fails, all-models
//! fails only on present-and-pinned mismatch — all hermetic.

use orator_models::ModelStatus;

use crate::models::verify_report;

fn row(name: &str, present: bool, pinned: bool) -> ModelStatus {
    ModelStatus {
        name: name.to_string(),
        kind: "whisper".to_string(),
        pinned,
        present,
        verified: None,
    }
}

fn check(name: &str) -> Result<bool, String> {
    match name {
        "good" => Ok(true),
        "bad" => Ok(false),
        _ => Err("boom".to_string()),
    }
}

fn rows() -> Vec<ModelStatus> {
    vec![
        row("good", true, true),
        row("bad", true, true),
        row("unpinned", true, false),
        row("absent", false, false),
    ]
}

#[test]
fn named_match_reports_ok() {
    let report = verify_report(&rows(), "good", check);
    assert_eq!(report.lines, vec!["good: ok"]);
    assert!(!report.failed);
    assert!(!report.unknown);
}

#[test]
fn named_mismatch_fails() {
    let report = verify_report(&rows(), "bad", check);
    assert_eq!(report.lines, vec!["bad: MISMATCH"]);
    assert!(report.failed);
}

#[test]
fn named_check_error_fails_with_detail() {
    let report = verify_report(&rows(), "good", |_| Err::<bool, _>("disk gone".to_string()));
    assert_eq!(report.lines, vec!["good: disk gone"]);
    assert!(report.failed);
}

#[test]
fn named_unpinned_is_unverified_and_fails() {
    let report = verify_report(&rows(), "unpinned", check);
    assert_eq!(report.lines, vec!["unpinned: unverified (unpinned)"]);
    assert!(report.failed);
}

#[test]
fn named_absent_is_missing_and_fails() {
    let report = verify_report(&rows(), "absent", check);
    assert_eq!(report.lines, vec!["absent: missing"]);
    assert!(report.failed);
}

#[test]
fn unknown_name_is_usage() {
    let report = verify_report(&rows(), "nope", check);
    assert!(report.unknown);
    assert!(report.failed);
}

#[test]
fn all_models_reports_skips_and_fails_on_mismatch() {
    let report = verify_report(&rows(), "", check);
    assert_eq!(
        report.lines,
        vec![
            "good: ok",
            "bad: MISMATCH",
            "unpinned: unverified (unpinned)",
            "absent: missing",
        ]
    );
    assert!(report.failed);
}

#[test]
fn all_models_succeeds_when_only_optionals_are_missing() {
    let rows = vec![
        row("unpinned", true, false),
        row("absent", false, true),
    ];
    let report = verify_report(&rows, "", check);
    assert_eq!(
        report.lines,
        vec!["unpinned: unverified (unpinned)", "absent: missing",]
    );
    assert!(!report.failed);
}
