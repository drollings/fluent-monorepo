//! M4 compat: explain not-found + `--help` stability through the built
//! binary. The port changes table content/scores/order (declared) but
//! never the shape, the empty state, or the flags.

#[path = "common.rs"]
mod common;

use common::{run, stdout, sync, workspace};

#[test]
fn explain_help_lists_stable_flags() {
    let dir = workspace();
    let out = run(dir.path(), &["explain", "--help"]);
    assert!(out.status.success(), "explain --help: {out:?}");
    let text = stdout(&out);
    for flag in [
        "--guidance",
        "--db",
        "--workspace",
        "--limit",
        "--no-llm",
        "--allow-degraded",
        "--filter",
        "--format",
        "--require-answer",
        "--answer-mode",
        "--trace",
        "--full-map",
    ] {
        assert!(text.contains(flag), "missing {flag}:\n{text}");
    }
}

#[test]
fn show_prompts_flag_is_accepted_and_plumbed() {
    let dir = workspace();
    let out = run(dir.path(), &["explain", "--help"]);
    assert!(out.status.success(), "explain --help: {out:?}");
    assert!(stdout(&out).contains("--show-prompts"), "{}", stdout(&out));
    // A model-free run with the flag must stay green: the flag only
    // enables prompt echo on model calls, never changes recall.
    sync(dir.path(), ".prompts.db", false);
    let root = dir.path().to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "zxqy nonexistent token zzz",
            "--workspace",
            &root,
            "--db",
            dir.path().join(".prompts.db").to_str().unwrap(),
            "--no-llm",
            "--show-prompts",
        ],
    );
    assert!(
        out.status.success(),
        "explain --show-prompts --no-llm: {out:?}"
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("=== LLM PROMPT ==="));
}

#[test]
fn explain_not_found_state_is_stable() {
    let dir = workspace();
    sync(dir.path(), ".explain.db", false);
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db");
    let db = db.to_str().unwrap().to_string();
    // Out-of-corpus query: empty on the ported path (no fallback
    // reintroduces hits, no closure invents them).
    let out = run(
        dir.path(),
        &[
            "explain",
            "zxqy nonexistent token zzz",
            "--workspace",
            &root,
            "--guidance",
            &json_dir,
            "--db",
            &db,
        ],
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.starts_with("## Explain: "), "{text}");
    assert!(text.contains("### Top matches"), "{text}");
    assert!(text.contains("### Citations"), "{text}");
    // Honest empty: titled sections with no entries, never a
    // fabricated answer and never a second human string.
    let top = text
        .split("### Top matches")
        .nth(1)
        .unwrap_or("")
        .split("### Citations")
        .next()
        .unwrap_or("");
    assert!(!top.contains("## Code"), "no findings render: {top}");
    assert!(!text.contains("### Answer"), "{text}");
}

#[test]
fn explain_missing_db_is_blocked_with_repair() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let db = dir.path().join(".missing.db");
    let db = db.to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &["explain", "alpha", "--workspace", &root, "--db", &db],
    );
    assert_eq!(out.status.code(), Some(3), "blocked explain: {out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("repair: guidance sync"), "{err}");

    // A missing index with --allow-degraded renders honest-empty
    // sections (opening creates the file; there is simply nothing in
    // it) instead of failing.
    let out = run(
        dir.path(),
        &[
            "explain",
            "alpha",
            "--workspace",
            &root,
            "--db",
            &db,
            "--allow-degraded",
        ],
    );
    assert!(out.status.success(), "degraded explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### Top matches"), "{text}");
    assert!(
        !text.contains("## Code"),
        "no findings in an empty index: {text}"
    );
}

fn explain_fixture() -> (tempfile::TempDir, String, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("lib.rs"),
        "/// Adds one.\npub fn alpha() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("b.rs"),
        "/// Adds two.\npub fn beta() -> u64 {\n    2\n}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("c.rs"),
        "/// Adds three.\npub fn gamma() -> u64 {\n    3\n}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("d.rs"),
        "/// Adds four.\npub fn delta() -> u64 {\n    4\n}\n",
    )
    .expect("write");
    std::fs::create_dir_all(root.join("doc")).expect("mkdir doc");
    std::fs::write(
        root.join("doc").join("notes.md"),
        "# Notes\n\nThe alpha helper adds one to every total.\n\nThis alpha aside adds nothing of value, ignore it.\n",
    )
    .expect("write");
    // Unreachable local models: hermetic (no dialing). The judge and
    // answer calls fail fast, exercising the omission and unavailable
    // paths deterministically.
    std::fs::create_dir_all(root.join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        root.join(".guidance").join("guidance-config.json"),
        r#"{"generative": {"enabled": true, "base_url": "http://127.0.0.1:1", "model": "code", "timeout_ms": 1000}, "decision": {"enabled": true, "base_url": "http://127.0.0.1:1", "model": "decision", "timeout_ms": 1000}}"#,
    )
    .expect("write");
    let db = root.join(".explain.db").to_str().unwrap().to_string();
    let json_dir = root.join(".guidance").to_str().unwrap().to_string();
    let ws = root.to_str().unwrap().to_string();
    (dir, ws, json_dir, db)
}

fn explain_args<'a>(
    ws: &'a str,
    json_dir: &'a str,
    db: &'a str,
    query: &'a str,
    extra: &[&'a str],
) -> Vec<&'a str> {
    let mut args = vec![
        "explain",
        query,
        "--workspace",
        ws,
        "--guidance",
        json_dir,
        "--db",
        db,
    ];
    args.extend(extra.iter().cloned());
    args
}

/// Hermetic local-model stub: answers one chat-completions POST per
/// connection with a canned `choices[0].message.content` body. The
/// response is chosen by request content: judge prompts (score
/// requests) get the score JSON, everything else gets the answer
/// prose. Ephemeral port, connection cap, close-after-reply — no real
/// model is dialed.

#[test]
fn explain_citations_follow_live_edits() {
    // Stored ranges point at lines 2-4; prepending two lines moves the
    // symbol without re-syncing. Citations must carry the live
    // positions.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    std::fs::write(
        dir.join("lib.rs"),
        "// header\n// more\n/// Adds one.\npub fn alpha() -> u64 {\n    1\n}\n",
    )
    .expect("edit without re-sync");
    let out = run(
        dir,
        &explain_args(&ws, &json_dir, &db, "adds", &["--no-llm"]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    let cites = text
        .split("### Citations")
        .nth(1)
        .expect("citations render");
    assert!(
        cites.contains("lib.rs:4-6"),
        "live lines for moved symbol: {cites}"
    );
    assert!(
        !cites.contains("lib.rs:2-4"),
        "stale stored range must not surface in citations: {cites}"
    );
}

/// `src/`-layout fixture: sources live under `src/` with a config naming
/// it, so stored row sources are src-root-relative (`live.rs`).
fn src_layout_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src").join("live.rs"),
        "/// Live.\npub fn live_fn() -> u32 {\n    41\n}\n",
    )
    .expect("write");
    std::fs::create_dir(dir.path().join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        dir.path().join(".guidance").join("guidance-config.json"),
        r#"{"src_dirs": ["src"]}"#,
    )
    .expect("config");
    dir
}

fn root_docs_fixture() -> tempfile::TempDir {
    // Workspace with a root README overlapping the query, plus code
    // hits so recall proceeds.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("README.md"),
        "# Flux Capacitor\n\nThe flux capacitor regulates widget flow.\n",
    )
    .expect("write readme");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src").join("flux.rs"),
        "/// Regulates flow.\npub fn flux_capacitor() -> u64 {\n    1\n}\n",
    )
    .expect("write flux");
    std::fs::write(
        dir.path().join("src").join("plain.rs"),
        "pub fn plain_thing() -> u64 {\n    2\n}\n",
    )
    .expect("write plain");
    std::fs::create_dir(dir.path().join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        dir.path().join(".guidance").join("guidance-config.json"),
        r#"{"src_dirs": ["src"], "docs_dir": "doc"}"#,
    )
    .expect("write config placeholder");
    dir
}

fn callers_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src").join("callee.rs"),
        "pub fn target_sym() -> u64 {\n    7\n}\n",
    )
    .expect("write callee");
    std::fs::write(
        dir.path().join("src").join("caller.rs"),
        "use crate::callee::target_sym;\npub fn caller_fn() -> u64 {\n    target_sym()\n}\n",
    )
    .expect("write caller");
    std::fs::create_dir(dir.path().join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        dir.path().join(".guidance").join("guidance-config.json"),
        r#"{"src_dirs": ["src"]}"#,
    )
    .expect("config");
    dir
}

#[test]
fn explain_snippet_serves_live_lines_after_unsynced_edit() {
    // Stored ranges must never surface as current: inserting comment
    // lines above the symbol without re-syncing moves the live span to
    // 5-7 while the stored span stays 2-4. Regression: the snippet
    // resolver joined the src-root-relative row source to the workspace
    // root, missed the file, and served the stale stored slice (lines
    // 2-4 of the live bytes) under the live 5-7 header.
    let dir = src_layout_fixture();
    sync(dir.path(), ".explain.db", false);
    std::fs::write(
        dir.path().join("src").join("live.rs"),
        "/// Live.\n// a\n// b\n// c\npub fn live_fn() -> u32 {\n    41\n}\n",
    )
    .expect("edit");
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db");
    let db = db.to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "what does live_fn do",
            "--workspace",
            &root,
            "--guidance",
            &json_dir,
            "--db",
            &db,
            "--no-llm",
            "--allow-degraded",
        ],
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("live.rs:5-7"), "live header: {text}");
    assert!(
        text.contains("pub fn live_fn() -> u32 {"),
        "live_fn snippet serves live lines, not the stored slice: {text}"
    );
    assert!(
        !text.contains("/// Live.\n// a\n"),
        "stored slice must not render under a live header: {text}"
    );
}

#[test]
fn explain_renders_root_readme_with_root_relative_cite() {
    // Root README content surfaces through the project map and code
    // recall cites workspace-relative paths (never docs-dir-relative).
    let dir = root_docs_fixture();
    let ws = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance").to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db").to_str().unwrap().to_string();
    sync(dir.path(), &db, false);
    let out = run(
        dir.path(),
        &explain_args(&ws, &json_dir, &db, "flux capacitor", &["--no-llm"]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### Top matches"), "{text}");
    assert!(text.contains("flux_capacitor"), "{text}");
    assert!(text.contains("### From the project map"), "{text}");
    assert!(
        !text.contains("doc/README.md"),
        "cite is workspace-relative, not docs-dir-relative: {text}"
    );
}

#[test]
fn explain_callers_renders_caller_file() {
    let dir = callers_fixture();
    sync(dir.path(), ".explain.db", false);
    let ws = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance").to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db").to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &explain_args(&ws, &json_dir, &db, "callers of target_sym", &["--no-llm"]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("caller.rs"), "caller file renders: {text}");
    assert!(text.contains("### Top matches"), "{text}");
}

#[test]
fn explain_callers_unknown_stays_empty() {
    let dir = callers_fixture();
    sync(dir.path(), ".explain.db", false);
    let ws = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance").to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db").to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &explain_args(&ws, &json_dir, &db, "callers of absent_sym", &["--no-llm"]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### Top matches"), "{text}");
    // The graph plane stays empty for unknown symbols: no caller edge
    // may render. Lexical partials can still rank (rank-only, never
    // filtered pre-calibration); under Auto they answer extractively.
    assert!(
        !text.contains("calls absent_sym"),
        "no caller edges: {text}"
    );
    for section in ["### Answer", "### Top matches", "### Call graph"] {
        let body = text
            .split(section)
            .nth(1)
            .unwrap_or("")
            .split("### ")
            .next()
            .unwrap_or("");
        assert!(
            !body.contains("calls absent_sym"),
            "no caller edges in {section}: {text}"
        );
    }
}

#[test]
fn explain_renders_both_same_basename_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src").join("sub1")).expect("mkdir sub1");
    std::fs::create_dir_all(dir.path().join("src").join("sub2")).expect("mkdir sub2");
    std::fs::write(
        dir.path().join("src").join("sub1").join("same.rs"),
        "pub fn item_one() -> u64 {\n    1\n}\n",
    )
    .expect("write one");
    std::fs::write(
        dir.path().join("src").join("sub2").join("same.rs"),
        "pub fn item_two() -> u64 {\n    2\n}\n",
    )
    .expect("write two");
    std::fs::create_dir(dir.path().join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        dir.path().join(".guidance").join("guidance-config.json"),
        r#"{"src_dirs": ["src"]}"#,
    )
    .expect("config");
    sync(dir.path(), ".explain.db", false);
    let ws = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance").to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db").to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &explain_args(&ws, &json_dir, &db, "item", &["--no-llm"]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### Top matches"), "{text}");
    // Factual answers are extractive: both same-basename files render
    // as cited Answer rows, consumed from the later sections.
    let response = text
        .split("### Answer")
        .nth(1)
        .expect("extractive answer renders")
        .split("### Gaps")
        .next()
        .unwrap_or("")
        .split("### Envelope")
        .next()
        .unwrap_or("");
    assert!(
        response.contains("sub1/same.rs"),
        "first file renders: {response}"
    );
    assert!(
        response.contains("sub2/same.rs"),
        "second file renders: {response}"
    );
    let top = text
        .split("### Top matches")
        .nth(1)
        .unwrap_or("")
        .split("### Citations")
        .next()
        .unwrap_or("");
    assert!(
        !top.contains("sub1/same.rs") && !top.contains("sub2/same.rs"),
        "consumed rows leave Top matches: {top}"
    );
}

/// Seed one ready capability row directly: the generator is gone, so
/// tests plant the external-corpus row the import would store.

#[test]
fn explain_renders_sections_and_envelope_v2() {
    // Shipping golden: the ReadEngine assembly renders Coverage, cards,
    // docs, capabilities, top matches, citations, and gaps in order,
    // plus a schema-2 envelope carrying every v1 key.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("doc/capabilities")).expect("mkdir corpus");
    std::fs::write(
        dir.join("doc/capabilities/corpus.json"),
        r#"{"briefs": [{"id": "doc/notes.md#notes", "title": "Notes", "purpose": "Alpha helper notes.", "crate": "c", "domain": "d", "source_path": "doc/notes.md", "anchor": "notes"}]}"#,
    )
    .expect("write corpus");
    sync(dir, &db, false);
    let out = run(
        dir,
        &explain_args(&ws, &json_dir, &db, "alpha helper", &["--no-llm"]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    for section in [
        "### Coverage",
        "### From the project map",
        "### From the docs",
        "### From the capabilities",
        "### Top matches",
        "### Citations",
    ] {
        assert!(text.contains(section), "missing {section}:\n{text}");
    }
    let positions: Vec<usize> = [
        "### Coverage",
        "### From the project map",
        "### Top matches",
    ]
    .iter()
    .map(|marker| text.find(marker).expect(marker))
    .collect();
    assert!(
        positions[0] < positions[1] && positions[1] < positions[2],
        "{text}"
    );
    assert!(text.contains("\"schema_version\": 2"), "{text}");
    for key in [
        "\"coverage\"",
        "\"claims\"",
        "\"tokens\"",
        "\"latency_ms\"",
        "\"degraded\"",
    ] {
        assert!(text.contains(key), "missing {key}:\n{text}");
    }
    // v1 keys are preserved alongside the v2 additions.
    for key in ["\"intent\"", "\"route\"", "\"gaps\"", "\"citations\""] {
        assert!(text.contains(key), "missing v1 {key}:\n{text}");
    }
}

#[test]
fn explain_markdown_shortens_envelope_digests_json_keeps_them() {
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let digests = |text: &str| -> Vec<String> {
        let envelope: serde_json::Value = serde_json::from_str(text).expect("envelope parses");
        envelope["citations"]
            .as_array()
            .expect("citations")
            .iter()
            .filter_map(|cite| cite["digest"].as_str().map(str::to_string))
            .collect()
    };
    let json_out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "alpha helper",
            &["--no-llm", "--format", "json"],
        ),
    );
    assert!(json_out.status.success(), "json: {json_out:?}");
    let full = digests(&stdout(&json_out));
    assert!(!full.is_empty(), "fixture answers with citations");
    assert!(full.iter().all(|digest| digest.len() == 64), "{full:?}");

    let md_out = run(
        dir,
        &explain_args(&ws, &json_dir, &db, "alpha helper", &["--no-llm"]),
    );
    assert!(md_out.status.success(), "md: {md_out:?}");
    let text = stdout(&md_out);
    let block = text
        .split("```json")
        .nth(1)
        .and_then(|tail| tail.split("```").next())
        .expect("markdown envelope block");
    let shortened = digests(block);
    assert_eq!(shortened.len(), full.len(), "same citations, shorter ids");
    assert!(
        shortened.iter().all(|digest| digest.len() <= 8),
        "markdown digests shorten: {shortened:?}"
    );
}

#[test]
fn explain_no_llm_is_model_free_and_capped_by_budget() {
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "alpha helper",
            &["--no-llm", "--budget", "200"],
        ),
    );
    assert!(out.status.success(), "explain --budget: {out:?}");
    let text = stdout(&out);
    assert!(!text.contains("### Answer"), "{text}");
    assert!(
        text.contains("### Gaps") || text.contains("### Top matches"),
        "{text}"
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("=== LLM PROMPT ==="));
}

#[test]
fn explain_format_json_prints_only_parseable_envelope() {
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "alpha helper",
            &["--no-llm", "--format", "json"],
        ),
    );
    assert!(out.status.success(), "explain --format json: {out:?}");
    let text = stdout(&out);
    assert!(
        !text.contains("## Explain:"),
        "no markdown in json mode: {text}"
    );
    assert!(!text.contains("```"), "no fence in json mode: {text}");
    let envelope: serde_json::Value = serde_json::from_str(text.trim()).expect("json parses");
    assert_eq!(envelope.get("schema_version"), Some(&serde_json::json!(2)));
    assert!(
        envelope.get("models").is_some(),
        "models present: {envelope}"
    );
}

#[test]
fn explain_envelope_carries_resolution_subject() {
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    // Anchored query: the free-term subject resolves beside the route.
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "beta gamma",
            &["--no-llm", "--format", "json"],
        ),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout(&out).trim()).expect("json parses");
    let resolution = envelope.get("resolution").expect("resolution object");
    assert!(resolution.get("route").is_some(), "{resolution}");
    let subject = resolution.get("subject").expect("subject");
    assert!(
        subject
            .as_array()
            .is_some_and(|terms| terms.iter().any(|term| term == "beta" || term == "gamma")),
        "{resolution}"
    );
    assert_eq!(
        resolution.get("source"),
        Some(&serde_json::json!("identifier")),
        "{resolution}"
    );
    // Anchorless query: subject null, output shape unchanged.
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "which crate",
            &["--no-llm", "--format", "json"],
        ),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout(&out).trim()).expect("json parses");
    assert_eq!(
        envelope
            .get("resolution")
            .and_then(|resolution| resolution.get("subject")),
        Some(&serde_json::Value::Null),
        "{envelope}"
    );
}
#[test]
fn explain_format_md_stays_default() {
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "alpha helper",
            &["--no-llm", "--format", "md"],
        ),
    );
    assert!(out.status.success(), "explain --format md: {out:?}");
    let text = stdout(&out);
    assert!(text.starts_with("## Explain: "), "{text}");
}

#[test]
fn explain_require_answer_exits_3_when_nothing_answers() {
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "zxqy nonexistent token zzz",
            &["--no-llm", "--require-answer"],
        ),
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "require-answer exits 3: {out:?}"
    );
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "alpha helper",
            &["--no-llm", "--require-answer"],
        ),
    );
    assert!(
        out.status.success(),
        "answerable query stays green: {out:?}"
    );
}

#[test]
fn explain_transport_failure_names_generative_unavailable() {
    // Unreachable local model: the legacy prose path degrades to a
    // gap and marks the generative role unavailable, never a verdict.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "alpha helper",
            &["--format", "json", "--answer-mode", "synthesis"],
        ),
    );
    assert!(out.status.success(), "explain with dead model: {out:?}");
    let text = stdout(&out);
    let envelope: serde_json::Value = serde_json::from_str(text.trim()).expect("json parses");
    let models = envelope.get("models").expect("models present");
    assert_eq!(
        models.get("generative"),
        Some(&serde_json::json!("unavailable")),
        "dead endpoint is unavailable, not a verdict: {envelope}"
    );
    assert_eq!(
        envelope.get("degraded"),
        Some(&serde_json::json!(true)),
        "{envelope}"
    );
    assert!(
        envelope
            .get("gaps")
            .and_then(|gaps| gaps.as_array())
            .is_some_and(|gaps| gaps.iter().any(|gap| gap
                .as_str()
                .is_some_and(|gap| gap.contains("synthesis unavailable")))),
        "gap names the failure: {envelope}"
    );
    assert!(
        !envelope
            .get("claims")
            .and_then(|claims| claims.as_array())
            .is_some_and(|claims| !claims.is_empty()),
        "no claims without synthesis: {envelope}"
    );
}

#[test]
fn explain_strict_refuses_renamed_row_naming_the_file() {
    // Rename probe through the CLI: the live range renders with a
    // stale label, and --strict refuses while naming the file.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("src").join("ren.rs"),
        "/// Renames.\npub fn old_name() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    std::fs::create_dir(root.join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        root.join(".guidance").join("guidance-config.json"),
        r#"{"src_dirs": ["src"]}"#,
    )
    .expect("config");
    sync(dir.path(), ".strict.db", false);
    std::fs::write(
        root.join("src").join("ren.rs"),
        "/// Renames.\npub fn new_name() -> u64 {\n    1\n}\n",
    )
    .expect("rename without re-sync");
    let ws = root.to_str().unwrap().to_string();
    let json_dir = root.join(".guidance").to_str().unwrap().to_string();
    let db = root.join(".strict.db").to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "where is old_name defined",
            &["--no-llm", "--allow-degraded"],
        ),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(
        text.contains("pub fn new_name"),
        "live bytes render: {text}"
    );
    assert!(
        !text.contains("pub fn old_name()"),
        "stored excerpt hidden: {text}"
    );
    assert!(text.contains("(stale)"), "row labelled stale: {text}");
    let out = run(
        dir.path(),
        &[
            "explain",
            "where is old_name defined",
            "--workspace",
            &ws,
            "--guidance",
            &json_dir,
            "--db",
            &db,
            "--no-llm",
            "--strict",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "strict refuses: {out:?}");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("ren.rs"), "strict names the file: {err}");
}

#[test]
fn explain_trace_prints_per_candidate_outcomes() {
    // Deterministic, model-free: every candidate row names its anchor
    // decision, judge label, and admission/budget outcome. Default
    // output stays trace-free (byte-identical contract).
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "alpha helper",
            &["--no-llm", "--trace"],
        ),
    );
    assert!(out.status.success(), "explain --trace: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### Trace"), "trace section renders: {text}");
    assert!(text.contains("anchor:"), "anchor decision per row: {text}");
    assert!(text.contains("judge:"), "judge label per row: {text}");
    assert!(
        text.contains("lib.rs") || text.contains("code-"),
        "code candidates trace: {text}"
    );
    let plain = run(
        dir,
        &explain_args(&ws, &json_dir, &db, "alpha helper", &["--no-llm"]),
    );
    assert!(plain.status.success(), "explain: {plain:?}");
    assert!(
        !stdout(&plain).contains("### Trace"),
        "default output stays trace-free"
    );
}

#[test]
fn explain_trace_json_carries_trace_array() {
    // `--format json --trace` carries the same rows inside the
    // envelope; without the flag the key stays absent.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "alpha helper",
            &["--no-llm", "--format", "json", "--trace"],
        ),
    );
    assert!(out.status.success(), "explain json --trace: {out:?}");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout(&out).trim()).expect("envelope parses");
    let trace = envelope.get("trace").expect("trace key with flag");
    let rows = trace.as_array().expect("trace is an array");
    assert!(!rows.is_empty(), "trace covers candidates: {envelope}");
    for row in rows {
        for key in ["plane", "candidate", "anchor", "judge", "outcome"] {
            assert!(row.get(key).is_some(), "missing {key}: {row}");
        }
    }
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "alpha helper",
            &["--no-llm", "--format", "json"],
        ),
    );
    assert!(out.status.success(), "explain json: {out:?}");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout(&out).trim()).expect("envelope parses");
    assert!(
        envelope.get("trace").is_none(),
        "default envelope stays trace-free: {envelope}"
    );
}
