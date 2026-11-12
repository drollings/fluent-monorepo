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
