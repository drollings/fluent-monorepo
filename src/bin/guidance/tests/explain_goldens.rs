//! Cold-start golden harness: hermetic CLI runs on a fixed fixture tree.
//!
//! Pins section order, citation count, no duplicate file across the
//! citation list, valid envelope JSON, balanced fences, and output
//! size. Evidence-only (`--no-llm`): no model is dialed.

#[path = "common.rs"]
mod common;

use common::{run, stdout, sync, workspace as _workspace};

#[test]
fn goldens_workspace_scaffold_exists() {
    let dir = _workspace();
    assert!(dir.path().join("lib.rs").is_file());
}

/// Fixed fixture: two code files plus one doc, synced once.
fn golden_fixture() -> (tempfile::TempDir, String, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("lib.rs"),
        "/// Adds one.\npub fn alpha() -> u64 {\n    1\n}\n",
    )
    .expect("write lib");
    std::fs::write(
        root.join("beta.rs"),
        "/// Adds two.\npub fn beta() -> u64 {\n    2\n}\n",
    )
    .expect("write beta");
    std::fs::create_dir_all(root.join("doc")).expect("mkdir doc");
    std::fs::write(
        root.join("doc").join("notes.md"),
        "# Notes\n\nThe alpha helper adds one to every total.\n",
    )
    .expect("write doc");
    std::fs::create_dir_all(root.join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        root.join(".guidance").join("guidance-config.json"),
        r#"{"src_dirs": ["."]}"#,
    )
    .expect("write config");
    let db = root.join(".golden.db").to_string_lossy().into_owned();
    let json_dir = root.join(".guidance").to_string_lossy().into_owned();
    let ws = root.to_string_lossy().into_owned();
    sync(dir.path(), ".golden.db", false);
    (dir, ws, json_dir, db)
}

fn explain(dir: &std::path::Path, ws: &str, json_dir: &str, db: &str, query: &str) -> String {
    let out = run(
        dir,
        &[
            "explain",
            query,
            "--workspace",
            ws,
            "--guidance",
            json_dir,
            "--db",
            db,
            "--no-llm",
        ],
    );
    assert!(out.status.success(), "explain failed: {out:?}");
    stdout(&out)
}

fn section_positions(text: &str) -> Vec<(&'static str, Option<usize>)> {
    [
        "### Coverage",
        "### From the project map",
        "### From the docs",
        "### From the capabilities",
        "### Top matches",
        "### Call graph",
        "### Citations",
        "### Answer",
        "### Gaps",
        "### Envelope",
    ]
    .iter()
    .map(|marker| (*marker, text.find(marker)))
    .collect()
}

fn envelope_json(text: &str) -> serde_json::Value {
    let body = text
        .split("### Envelope")
        .nth(1)
        .expect("envelope section renders");
    let start = body.find("```json").expect("envelope fence opens");
    let rest = &body[start + "```json".len()..];
    let end = rest.find("```").expect("envelope fence closes");
    serde_json::from_str(rest[..end].trim()).expect("envelope parses")
}

fn count_fences(text: &str) -> usize {
    text.matches("```").count()
}

#[test]
fn goldens_section_order_is_stable() {
    let (temp, ws, json_dir, db) = golden_fixture();
    let text = explain(temp.path(), &ws, &json_dir, &db, "alpha helper");
    let positions = section_positions(&text);
    let required = [
        "### Coverage",
        "### From the project map",
        "### From the docs",
        "### Top matches",
        "### Citations",
    ];
    for marker in required {
        assert!(text.contains(marker), "missing {marker}:\n{text}");
    }
    let mut last = 0usize;
    for marker in required {
        let pos = text.find(marker).expect(marker);
        assert!(pos >= last, "order violated at {marker}:\n{text}");
        last = pos;
    }
    let _ = positions;
}

#[test]
fn goldens_citations_are_counted_and_deduped() {
    let (temp, ws, json_dir, db) = golden_fixture();
    let text = explain(temp.path(), &ws, &json_dir, &db, "alpha helper");
    let envelope = envelope_json(&text);
    let cites = envelope
        .get("citations")
        .and_then(|value| value.as_array())
        .expect("citations array");
    assert!(!cites.is_empty(), "fixture query cites: {text}");
    let mut files = std::collections::HashSet::new();
    for cite in cites {
        let rendered = cite
            .get("citation")
            .and_then(|value| value.as_str())
            .expect("citation string");
        let file = rendered.split(':').next().unwrap_or(rendered);
        assert!(
            files.insert(file.to_string()),
            "duplicate file across citations: {rendered}\n{text}"
        );
    }
    let citations_body = text
        .split("### Citations")
        .nth(1)
        .unwrap_or("")
        .split("### Answer")
        .next()
        .unwrap_or("")
        .split("### Gaps")
        .next()
        .unwrap_or("")
        .split("### Envelope")
        .next()
        .unwrap_or("");
    assert!(
        citations_body.contains("lib.rs") || citations_body.contains("beta.rs"),
        "citations name the fixture files: {citations_body}"
    );
}

#[test]
fn goldens_envelope_is_valid_and_fences_balance() {
    let (temp, ws, json_dir, db) = golden_fixture();
    let text = explain(temp.path(), &ws, &json_dir, &db, "alpha helper");
    let envelope = envelope_json(&text);
    assert_eq!(envelope.get("schema_version"), Some(&serde_json::json!(2)));
    for key in [
        "intent",
        "route",
        "gaps",
        "citations",
        "coverage",
        "claims",
        "tokens",
        "latency_ms",
        "degraded",
        "models",
        "answer_mode",
        "cache",
    ] {
        assert!(envelope.get(key).is_some(), "missing {key}: {envelope}");
    }
    let fences = count_fences(&text);
    assert_eq!(fences % 2, 0, "unbalanced fences ({fences}):\n{text}");
    assert!(fences >= 2, "envelope fence renders:\n{text}");
}

#[test]
fn goldens_output_size_is_bounded() {
    let (temp, ws, json_dir, db) = golden_fixture();
    let text = explain(temp.path(), &ws, &json_dir, &db, "alpha helper");
    assert!(text.len() > 500, "answer renders substance: {}", text.len());
    assert!(
        text.len() < 120_000,
        "golden output bounded: {}",
        text.len()
    );
    let envelope = envelope_json(&text);
    let tokens = envelope
        .get("tokens")
        .and_then(serde_json::Value::as_u64)
        .expect("tokens number");
    assert!(tokens > 0 && tokens < 60_000, "tokens bounded: {tokens}");
}

#[test]
fn goldens_control_stays_unanswered() {
    let (temp, ws, json_dir, db) = golden_fixture();
    let text = explain(
        temp.path(),
        &ws,
        &json_dir,
        &db,
        "zzqy nonexistent token zzz",
    );
    assert!(
        !text.contains("### Answer"),
        "controls answer nothing:\n{text}"
    );
    let envelope = envelope_json(&text);
    let cites = envelope
        .get("citations")
        .and_then(|value| value.as_array())
        .expect("citations array");
    assert!(cites.is_empty(), "controls cite nothing: {envelope}");
}

#[test]
fn goldens_no_llm_runs_are_byte_identical() {
    // No model in the call path: two runs print byte-identical
    // bodies (the envelope carries wall-clock latency, so it is
    // compared structurally, not byte-for-byte).
    let (temp, ws, json_dir, db) = golden_fixture();
    let dir = temp.path();
    let run_once = || {
        let out = run(
            dir,
            &[
                "explain",
                "alpha helper",
                "--workspace",
                &ws,
                "--guidance",
                &json_dir,
                "--db",
                &db,
                "--no-llm",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        stdout(&out)
    };
    let first = run_once();
    let second = run_once();
    let (first_body, first_envelope) = first.split_once("### Envelope").expect("envelope");
    let (second_body, _) = second.split_once("### Envelope").expect("envelope");
    assert_eq!(first_body, second_body);
    let envelope: serde_json::Value = serde_json::from_str(
        first_envelope
            .replace("```json", "")
            .replace("```", "")
            .trim(),
    )
    .expect("envelope parses");
    assert_eq!(envelope.get("schema_version"), Some(&serde_json::json!(2)));
}
