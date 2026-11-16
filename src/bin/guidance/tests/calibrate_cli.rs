//! `calibrate` command goldens: the corpus measures, bars gate
//! storage, passing fixtures store a reloadable entry. Written before
//! the command (red first: unknown subcommand).

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};

#[test]
fn calibrate_measures_without_storing_when_below_bars() {
    let dir = workspace();
    let corpus = dir.path().join("triage.json");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../guidance/corpora/triage_calibration.json"),
        &corpus,
    )
    .expect("stage corpus");
    let out = run(
        dir.path(),
        &[
            "calibrate",
            "--point",
            "dp.triage_residual",
            "--dataset",
            corpus.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--journal-dir",
            dir.path().join("journal").to_str().unwrap(),
        ],
    );
    let text = stdout(&out);
    assert!(
        out.status.success(),
        "measurement is not an error: {out:?}\n{text}"
    );
    assert!(text.contains("accuracy"), "{text}");
    assert!(
        text.contains("HELD") || text.contains("AUTHORIZED"),
        "verdict printed: {text}"
    );
    assert!(
        !dir.path().join("state/calibration.json").exists(),
        "static-only scoring stays below the bars: nothing stored"
    );
}

#[test]
fn calibrate_stores_passing_entries_reloadably() {
    let dir = workspace();
    let corpus = dir.path().join("passing.json");
    std::fs::write(
        &corpus,
        r#"{"cases": [
            {"diagnostic": "warning: unused import: `foo` [unused_imports]", "expected": "mechanical-removal", "control": true},
            {"diagnostic": "warning: unused import: `bar` [unused_imports]", "expected": "mechanical-removal", "control": false}
        ]}"#,
    )
    .expect("write");
    let state = dir.path().join("state");
    let out = run(
        dir.path(),
        &[
            "calibrate",
            "--point",
            "dp.triage_residual",
            "--dataset",
            corpus.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--journal-dir",
            dir.path().join("journal").to_str().unwrap(),
            "--model-fingerprint",
            "static-table",
        ],
    );
    let text = stdout(&out);
    assert!(out.status.success(), "calibration runs: {out:?}\n{text}");
    assert!(text.contains("AUTHORIZED"), "passing bars store: {text}");
    let stored = state.join("calibration.json");
    assert!(stored.exists(), "entry persisted");
    let raw = std::fs::read_to_string(&stored).expect("read");
    assert!(raw.contains("dp.triage_residual"), "{raw}");
}

#[test]
fn calibrate_rejects_unknown_points() {
    let dir = workspace();
    let out = run(
        dir.path(),
        &["calibrate", "--point", "dp.teleport", "--dataset", "x.json"],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "unknown point is usage error: {out:?}"
    );
}

#[test]
fn calibrate_capability_measures_without_storing() {
    // M15 dry-run (no endpoint): the golden corpus measures SVO-rank
    // recall@1 plus out-of-corpus controls. The command reports and
    // holds — promotion is a separate explicit human decision, so no
    // entry is ever stored here.
    let dir = workspace();
    let corpus = dir.path().join("capability.json");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../guidance/corpora/capability_seed.json"),
        &corpus,
    )
    .expect("stage corpus");
    let out = run(
        dir.path(),
        &[
            "calibrate",
            "--point",
            "dp.capability",
            "--dataset",
            corpus.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--journal-dir",
            dir.path().join("journal").to_str().unwrap(),
        ],
    );
    let text = stdout(&out);
    assert!(
        out.status.success(),
        "measurement is not an error: {out:?}\n{text}"
    );
    assert!(text.contains("recall@1"), "{text}");
    assert!(text.contains("control fires"), "{text}");
    assert!(text.contains("nothing stored"), "never auto-stores: {text}");
    assert!(
        !dir.path().join("state/calibration.json").exists(),
        "promotion needs human approval: nothing stored"
    );
}

#[test]
fn calibrate_subject_anchor_reports_deterministically() {
    // No endpoint, no registry, never stored: the anchor report is a
    // pure function of the corpus. Hermetic and repeatable.
    let dir = workspace();
    let corpus = dir.path().join("doc_admission.jsonl");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../doc/guidance/eval/doc_admission.jsonl"),
        &corpus,
    )
    .expect("stage corpus");
    let first = stdout(&run(
        dir.path(),
        &[
            "calibrate",
            "--point",
            "dp.subject_anchor",
            "--dataset",
            corpus.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--journal-dir",
            dir.path().join("journal").to_str().unwrap(),
        ],
    ));
    let second = stdout(&run(
        dir.path(),
        &[
            "calibrate",
            "--point",
            "dp.subject_anchor",
            "--dataset",
            corpus.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--journal-dir",
            dir.path().join("journal").to_str().unwrap(),
        ],
    ));
    assert_eq!(
        first, second,
        "deterministic report repeats byte-identically"
    );
    assert!(first.contains("mrr:"), "{first}");
    assert!(first.contains("retention: 1.000 (8/8)"), "{first}");
    assert!(first.contains("excluded: 0.875 (7/8)"), "{first}");
    assert!(
        first.contains("verdict: PASS (bars met; anchors are measured, never stored)"),
        "{first}"
    );
    assert!(
        !dir.path().join("state/calibration.json").exists(),
        "the anchor never stores"
    );
}

#[test]
fn calibrate_doc_admission_held_without_endpoint() {
    // Fail-closed: an unreachable decision endpoint judges nothing
    // Strong, so recall collapses and the point holds with nothing
    // stored. Measurement is not an error.
    let dir = workspace();
    let corpus = dir.path().join("doc_admission.jsonl");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../doc/guidance/eval/doc_admission.jsonl"),
        &corpus,
    )
    .expect("stage corpus");
    let out = run(
        dir.path(),
        &[
            "calibrate",
            "--point",
            "dp.doc_admission",
            "--dataset",
            corpus.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--journal-dir",
            dir.path().join("journal").to_str().unwrap(),
            "--model-fingerprint",
            "unprobed",
        ],
    );
    let text = stdout(&out);
    assert!(
        out.status.success(),
        "measurement is not an error: {out:?}\n{text}"
    );
    assert!(text.contains("precision:"), "{text}");
    assert!(text.contains("control fires"), "{text}");
    assert!(text.contains("HELD"), "unreachable judge holds: {text}");
    assert!(
        !dir.path().join("state/calibration.json").exists(),
        "fail-closed stores nothing"
    );
}

#[test]
fn calibrate_unknown_point_names_admission_points() {
    let dir = workspace();
    let out = run(
        dir.path(),
        &["calibrate", "--point", "dp.teleport", "--dataset", "x.json"],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("dp.doc_admission"), "{err}");
    assert!(err.contains("dp.subject_anchor"), "{err}");
}

#[test]
fn calibrate_sufficiency_reports_without_storing() {
    // Deterministic abstain rule: must-answer routes Answer,
    // must-refuse (and controls) route Unanswerable. Measured, never
    // stored — the verdict needs its own calibration first.
    let dir = workspace();
    let corpus = dir.path().join("sufficiency.jsonl");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../doc/guidance/eval/sufficiency.jsonl"),
        &corpus,
    )
    .expect("stage corpus");
    let out = run(
        dir.path(),
        &[
            "calibrate",
            "--point",
            "sufficiency",
            "--dataset",
            corpus.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--journal-dir",
            dir.path().join("journal").to_str().unwrap(),
        ],
    );
    let text = stdout(&out);
    assert!(
        out.status.success(),
        "measurement is not an error: {out:?}\n{text}"
    );
    assert!(text.contains("support: 8"), "{text}");
    assert!(text.contains("correct: 8/8"), "{text}");
    assert!(text.contains("controls refused: 2/2"), "{text}");
    assert!(text.contains("verdict: PASS"), "{text}");
    assert!(
        !dir.path().join("state/calibration.json").exists(),
        "the abstain rule is measured, never stored"
    );
}
