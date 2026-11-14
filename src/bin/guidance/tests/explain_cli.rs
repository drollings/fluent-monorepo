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
        "--filter",
    ] {
        assert!(text.contains(flag), "missing {flag}:\n{text}");
    }
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
    assert!(text.contains("No results found."), "{text}");
    assert!(
        !text.contains('|'),
        "no table rows for empty results: {text}"
    );
}

#[test]
fn explain_missing_db_is_empty_not_error() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "alpha",
            "--workspace",
            &root,
            "--db",
            dir.path().join(".missing.db").to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "explain without db: {out:?}");
    assert!(stdout(&out).contains("No results found."));
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
fn stub_model(judge_scores: &str, answer: &str, max_conns: usize) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().expect("port").port();
    let judge = judge_scores.to_string();
    let answer = answer.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(max_conns) {
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(_) => break,
            };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
            let mut head = vec![0u8; 65536];
            let n = match stream.read(&mut head) {
                Ok(n) => n,
                Err(_) => break,
            };
            let text = String::from_utf8_lossy(&head[..n]).into_owned();
            let length = text
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length:")
                        .or_else(|| line.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            while body.len() < length {
                let mut rest = vec![0u8; 65536];
                match stream.read(&mut rest) {
                    Ok(0) | Err(_) => break,
                    Ok(m) => body.push_str(&String::from_utf8_lossy(&rest[..m])),
                }
            }
            let content = if body.contains("Rate how relevant") {
                judge.clone()
            } else {
                answer.clone()
            };
            let payload =
                serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn stub_config(base_url: &str) -> String {
    format!(
        r#"{{"generative": {{"enabled": true, "base_url": "{base_url}", "model": "code", "timeout_ms": 5000}}, "decision": {{"enabled": true, "base_url": "{base_url}", "model": "decision", "timeout_ms": 5000}}}}"#
    )
}

#[test]
fn explain_renders_ordered_sections_with_gated_docs_and_answer() {
    // Full contract against the stub model: judged docs plus a concise
    // answer render in reference order — Answer, From the docs, Top
    // matches, Citations, footer — with no table and no Context section.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    let stub = stub_model(r#"{"1": 95, "2": 5}"#, "Alpha adds one to its input.", 8);
    std::fs::write(
        dir.join(".guidance").join("guidance-config.json"),
        stub_config(&stub),
    )
    .expect("write stub config");
    sync(dir, &db, false);
    let out = run(dir, &explain_args(&ws, &json_dir, &db, "alpha", &[]));
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    let positions = [
        "### Answer",
        "### From the docs",
        "### Top matches",
        "### Citations",
        "If you need details, try:",
    ];
    let mut cursor = 0usize;
    for section in positions {
        let found = text[cursor..].find(section).expect(section);
        cursor += found + section.len();
    }
    assert!(text.contains("Alpha adds one"), "{text}");
    assert!(text.contains("notes.md"), "{text}");
    assert!(text.contains("adds one to every total"), "{text}");
    let docs_section = text
        .split("### From the docs")
        .nth(1)
        .expect("docs section renders")
        .split("### Top matches")
        .next()
        .unwrap_or("");
    assert!(
        !docs_section.contains("nothing of value"),
        "judge rejects the junk paragraph even though it shares a query token: {docs_section}"
    );
    assert!(!text.contains("### Context"), "{text}");
    assert!(!text.contains("| Name |"), "{text}");
    assert!(text.contains("guidance explain \"callers of"), "{text}");
    assert!(text.contains("```rust"), "{text}");
}

#[test]
fn explain_omits_docs_when_judge_unreachable() {
    // The fixture points the decision role at a dead port: the judge
    // call fails, so the docs section is omitted rather than cited
    // unjudged. The answer degrades to unavailable instead of failing.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let out = run(dir, &explain_args(&ws, &json_dir, &db, "adds", &[]));
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(!text.contains("### From the docs"), "{text}");
    assert!(text.contains("### Answer"), "{text}");
    assert!(text.contains("(unavailable"), "{text}");
    assert!(text.contains("### Citations"), "{text}");
}

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
    let abs = dir.join("lib.rs").to_string_lossy().into_owned();
    assert!(
        cites.contains(&format!("{abs} : lines 4-6")),
        "live lines for moved symbol: {cites}"
    );
    assert!(
        !cites.contains(&format!("{abs} : lines 2-4")),
        "stale stored range must not surface in citations: {cites}"
    );
}

#[test]
fn explain_no_llm_omits_answer_and_docs() {
    // --no-llm means no model calls at all: no answer section and no
    // decision-gated docs section, while snippets and citations stay.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    sync(dir, &db, false);
    let out = run(
        dir,
        &explain_args(&ws, &json_dir, &db, "adds", &["--no-llm"]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(!text.contains("### Answer"), "{text}");
    assert!(!text.contains("### From the docs"), "{text}");
    assert!(text.contains("### Top matches"), "{text}");
    assert!(text.contains("### Citations"), "{text}");
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
        ],
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("live.rs:5-7"), "live header: {text}");
    assert!(
        text.contains("```rust\npub fn live_fn() -> u32 {\n    41\n}\n```"),
        "live_fn snippet serves live lines, not the stored slice: {text}"
    );
}

#[test]
fn explain_drops_prose_only_single_token_hits() {
    // Relevance gate: a hit matching exactly one query token in prose —
    // no symbol or file-stem match — must not render. Today it fills Top
    // matches and Citations, and the answer confabulates from it.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src").join("real.rs"),
        "pub fn major_unit() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("src").join("decoy.rs"),
        "/// major overhaul planned\npub fn overhaul() -> u64 {\n    2\n}\n",
    )
    .expect("write");
    std::fs::create_dir(dir.path().join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        dir.path().join(".guidance").join("guidance-config.json"),
        r#"{"src_dirs": ["src"]}"#,
    )
    .expect("config");
    sync(dir.path(), ".explain.db", false);
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db");
    let db = db.to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "major components overview",
            "--workspace",
            &root,
            "--guidance",
            &json_dir,
            "--db",
            &db,
            "--no-llm",
        ],
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("major_unit"), "structural hit stays: {text}");
    assert!(
        !text.contains("overhaul"),
        "prose-only single-token hit must not render: {text}"
    );
    assert!(!text.contains("decoy.rs"), "no decoy citations: {text}");
}

#[test]
fn explain_reports_no_relevant_results_when_all_gated() {
    // Gated-empty is the honest empty state: hits existed, none was
    // relevant — no snippets, no citations, no model-backed answer.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src").join("a.rs"),
        "/// major cleanup\npub fn cleanup() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("src").join("b.rs"),
        "// refactoring notes\npub fn notes() -> u64 {\n    2\n}\n",
    )
    .expect("write");
    std::fs::create_dir(dir.path().join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        dir.path().join(".guidance").join("guidance-config.json"),
        r#"{"src_dirs": ["src"]}"#,
    )
    .expect("config");
    sync(dir.path(), ".explain.db", false);
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db");
    let db = db.to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "major refactoring effort",
            "--workspace",
            &root,
            "--guidance",
            &json_dir,
            "--db",
            &db,
            "--no-llm",
        ],
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(
        text.contains("No relevant results found."),
        "gated-empty state: {text}"
    );
    assert!(!text.contains("### Top matches"), "{text}");
    assert!(!text.contains("### Citations"), "{text}");
}

#[test]
fn explain_renders_docs_for_think_wrapped_judge_scores() {
    // Reasoning models wrap the verdict in think tags; the scores
    // inside still gate docs. Fails before the shared-parser
    // migration (whole-body parse rejects the body, section missing).
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    let stub = stub_model(
        "<think>scoring each paragraph</think>{\"1\": 95, \"2\": 5}",
        "Alpha adds one to its input.",
        8,
    );
    std::fs::write(
        dir.join(".guidance").join("guidance-config.json"),
        stub_config(&stub),
    )
    .expect("write stub config");
    sync(dir, &db, false);
    let out = run(dir, &explain_args(&ws, &json_dir, &db, "alpha", &[]));
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### From the docs"), "{text}");
    assert!(text.contains("adds one to every total"), "{text}");
}

#[test]
fn explain_renders_docs_for_prose_wrapped_judge_scores() {
    // Trailing prose around the verdict parses; the scores inside
    // still gate docs. Fails before the shared-parser migration.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    let stub = stub_model(
        "Here are my scores:\n{\"1\": 95, \"2\": 5}\nHope this helps.",
        "Alpha adds one to its input.",
        8,
    );
    std::fs::write(
        dir.join(".guidance").join("guidance-config.json"),
        stub_config(&stub),
    )
    .expect("write stub config");
    sync(dir, &db, false);
    let out = run(dir, &explain_args(&ws, &json_dir, &db, "alpha", &[]));
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### From the docs"), "{text}");
    assert!(text.contains("adds one to every total"), "{text}");
}

#[test]
fn explain_omits_docs_when_query_shares_no_doc_terms() {
    // M2-cal control group: code hits exist (gamma/delta), but no doc
    // paragraph shares a query token, so ranking yields nothing and
    // the section stays absent — without any judge call deciding it.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    let stub = stub_model(r#"{"1": 95, "2": 5}"#, "Gamma and delta.", 8);
    std::fs::write(
        dir.join(".guidance").join("guidance-config.json"),
        stub_config(&stub),
    )
    .expect("write stub config");
    sync(dir, &db, false);
    let out = run(dir, &explain_args(&ws, &json_dir, &db, "gamma delta", &[]));
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### Top matches"), "{text}");
    assert!(
        !text.contains("### From the docs"),
        "zero overlap means no docs section: {text}"
    );
}

#[test]
fn explain_docs_rank_cache_keeps_output_byte_identical() {
    // M3 recall-neutrality: the rank cache must not change what renders.
    // First run populates `.guidance/doc_rank_cache.json`; the second
    // run serves from it. Identical stdout proves the cache is a pure
    // memo, not a behavior change. Uses the stub model so the judged
    // docs section actually renders.
    let (temp, ws, json_dir, db) = explain_fixture();
    let dir = temp.path();
    let stub = stub_model(r#"{"1": 95, "2": 5}"#, "Alpha adds one.", 8);
    std::fs::write(
        dir.join(".guidance").join("guidance-config.json"),
        stub_config(&stub),
    )
    .expect("write stub config");
    sync(dir, &db, false);
    let args = explain_args(&ws, &json_dir, &db, "alpha", &[]);
    let first = stdout(&run(dir, &args));
    assert!(first.contains("### From the docs"), "{first}");
    let cache = dir.join(".guidance").join("doc_rank_cache.json");
    assert!(
        cache.is_file(),
        "first run records the ranking: {}",
        cache.display()
    );
    let second = stdout(&run(dir, &args));
    assert_eq!(
        second, first,
        "cached rank renders byte-identically to the fresh rank"
    );
}

fn root_docs_fixture() -> tempfile::TempDir {
    // Workspace with a root README overlapping the query, plus code
    // hits so the relevance gate proceeds to docs.
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

#[test]
fn explain_renders_root_readme_with_root_relative_cite() {
    // M4 golden: a root README overlapping the query renders under
    // From the docs with a root-relative cite. Fails before root
    // scope (root files were never walked).
    let dir = root_docs_fixture();
    let stub = stub_model(r#"{"1": 95}"#, "Flux regulates flow.", 8);
    std::fs::write(
        dir.path().join(".guidance").join("guidance-config.json"),
        stub_config(&stub),
    )
    .expect("write stub config");
    let ws = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance").to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db").to_str().unwrap().to_string();
    sync(dir.path(), &db, false);
    let out = run(
        dir.path(),
        &explain_args(&ws, &json_dir, &db, "flux capacitor", &[]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### From the docs"), "{text}");
    assert!(text.contains("regulates widget flow"), "{text}");
    assert!(text.contains("README.md#"), "root-relative cite: {text}");
    assert!(
        !text.contains("doc/README.md"),
        "cite is workspace-relative, not docs-dir-relative: {text}"
    );
}

#[test]
fn explain_omits_docs_when_query_matches_neither_tree_nor_roots() {
    // M4-cal control: code hits exist (relevance gate proceeds) but no
    // doc or root paragraph shares a token, so no docs section.
    let dir = root_docs_fixture();
    let stub = stub_model(r#"{"1": 95}"#, "Plain thing.", 8);
    std::fs::write(
        dir.path().join(".guidance").join("guidance-config.json"),
        stub_config(&stub),
    )
    .expect("write stub config");
    let ws = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance").to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db").to_str().unwrap().to_string();
    sync(dir.path(), &db, false);
    let out = run(
        dir.path(),
        &explain_args(&ws, &json_dir, &db, "plain thing", &[]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### Top matches"), "{text}");
    assert!(
        !text.contains("### From the docs"),
        "zero overlap means no docs section: {text}"
    );
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
    let top = text
        .split("### Top matches")
        .nth(1)
        .unwrap_or("")
        .split("### Citations")
        .next()
        .unwrap_or("");
    assert!(
        !top.contains("callee.rs"),
        "definition is not a caller: {top}"
    );
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
    assert!(
        text.contains("No results found.") || text.contains("No relevant results found."),
        "honest empty: {text}"
    );
    assert!(!text.contains("### Top matches"), "{text}");
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
    let top = text
        .split("### Top matches")
        .nth(1)
        .unwrap_or("")
        .split("### Citations")
        .next()
        .unwrap_or("");
    assert!(top.contains("sub1/same.rs"), "first file renders: {top}");
    assert!(top.contains("sub2/same.rs"), "second file renders: {top}");
}

fn capability_fixture() -> (tempfile::TempDir, String, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src").join("router")).expect("mkdir src");
    std::fs::create_dir_all(root.join("doc").join("router")).expect("mkdir doc");
    std::fs::write(
        root.join("src").join("router").join("dispatch.rs"),
        "/// Routes requests onward through the table.\npub fn dispatch_routes() {}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("src").join("router").join("drive.rs"),
        "/// Drives the flux regulator.\npub fn regulate_drive() {}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("src").join("router").join("flux.rs"),
        "/// Calibrates flux output levels.\npub fn calibrate_output() {}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("doc").join("router").join("guide.md"),
        "# Guide\n\nRoutes requests onward.\n",
    )
    .expect("write");
    std::fs::create_dir_all(root.join(".guidance")).expect("mkdir guidance");
    let db = root.join(".explain.db").to_str().unwrap().to_string();
    let json_dir = root.join(".guidance").to_str().unwrap().to_string();
    let ws = root.to_str().unwrap().to_string();
    (dir, ws, json_dir, db)
}

fn narrow_fixture() -> (tempfile::TempDir, String, String, String) {
    // Overlapping vocabulary: one narrow query matches all three
    // files on multiple tokens, so the branch stays shut by count
    // and score even with a matching brief available.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("a.rs"),
        "/// Alpha beta gamma core.\npub fn alpha_core() {}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("b.rs"),
        "/// Alpha beta helpers.\npub fn beta_help() {}\n",
    )
    .expect("write");
    std::fs::write(
        root.join("c.rs"),
        "/// Alpha gamma tools.\npub fn gamma_tools() {}\n",
    )
    .expect("write");
    std::fs::create_dir_all(root.join("doc")).expect("mkdir doc");
    std::fs::write(
        root.join("doc").join("guide.md"),
        "# Guide\n\nAlpha beta gamma overview.\n",
    )
    .expect("write");
    std::fs::create_dir_all(root.join(".guidance")).expect("mkdir guidance");
    let db = root.join(".explain.db").to_str().unwrap().to_string();
    let json_dir = root.join(".guidance").to_str().unwrap().to_string();
    let ws = root.to_str().unwrap().to_string();
    (dir, ws, json_dir, db)
}

#[test]
fn explain_capability_branch_renders_brief_and_tagged_pull() {
    // Trigger-ON golden (M13-cal): an NL orientation query is
    // code-thin, so the capability section renders and the rank-1
    // brief opens one scoped pull tagged with its crate.
    let (temp, ws, json_dir, db) = capability_fixture();
    let dir = temp.path();
    let stub = stub_model(
        r#"{"doc/router/guide.md#guide": "Routes requests onward."}"#,
        r#"{"doc/router/guide.md#guide": "Routes requests onward."}"#,
        4,
    );
    std::fs::write(
        dir.join(".guidance").join("guidance-config.json"),
        stub_config(&stub),
    )
    .expect("write stub config");
    sync(dir, ".explain.db", false);
    let out = run(
        dir,
        &[
            "summarize",
            "--capabilities",
            "--workspace",
            &ws,
            "--json-dir",
            &json_dir,
            "--db",
            &db,
        ],
    );
    assert!(out.status.success(), "summarize --capabilities: {out:?}");
    let out = run(
        dir,
        &explain_args(
            &ws,
            &json_dir,
            &db,
            "how are requests routed onward",
            &["--no-llm"],
        ),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("### From the capabilities"), "{text}");
    // No model under --no-llm: briefs render unjudged (`considered`)
    // in deterministic SVO order, never filtered.
    assert!(text.contains("[considered] fluent-router/router"), "{text}");
    assert!(text.contains("Routes requests onward."), "{text}");
    assert!(text.contains("via capability: fluent-router"), "{text}");
    assert!(text.contains("dispatch.rs"), "{text}");
}

#[test]
fn explain_capability_branch_stays_shut_on_strong_code_recall() {
    // Trigger-OFF golden: a narrow multi-token query with agreeing
    // code recall renders byte-identically to the pre-branch shape —
    // no capabilities section, no branch tags — even with a matching
    // brief available (the trigger, not the brief index, decides).
    let (temp, ws, json_dir, db) = narrow_fixture();
    let dir = temp.path();
    let stub = stub_model(
        r#"{"doc/guide.md#guide": "Alpha beta gamma overview."}"#,
        r#"{"doc/guide.md#guide": "Alpha beta gamma overview."}"#,
        4,
    );
    std::fs::write(
        dir.join(".guidance").join("guidance-config.json"),
        stub_config(&stub),
    )
    .expect("write stub config");
    sync(dir, ".explain.db", false);
    let out = run(
        dir,
        &[
            "summarize",
            "--capabilities",
            "--workspace",
            &ws,
            "--json-dir",
            &json_dir,
            "--db",
            &db,
        ],
    );
    assert!(out.status.success(), "summarize --capabilities: {out:?}");
    let out = run(
        dir,
        &explain_args(&ws, &json_dir, &db, "alpha beta gamma", &["--no-llm"]),
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(!text.contains("From the capabilities"), "{text}");
    assert!(!text.contains("via capability"), "{text}");
    assert!(text.contains("### Top matches"), "{text}");
}
