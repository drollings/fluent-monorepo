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
    let out = run(dir, &explain_args(&ws, &json_dir, &db, "adds", &["--no-llm"]));
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
    let out = run(dir, &explain_args(&ws, &json_dir, &db, "adds", &["--no-llm"]));
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(!text.contains("### Answer"), "{text}");
    assert!(!text.contains("### From the docs"), "{text}");
    assert!(text.contains("### Top matches"), "{text}");
    assert!(text.contains("### Citations"), "{text}");
}
