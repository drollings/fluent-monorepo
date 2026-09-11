//! P5 `mcp-contract` port: toolsets, background (eventual) default, input
//! bounds, elicitation-free local operation, and the freshness/coverage
//! contract — driven against the built binary over stdio.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command};

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, sync};

fn guidance_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_guidance"))
}

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn spawn(workspace: &std::path::Path, toolset: &str) -> Self {
        let db = workspace.join("mcp.db");
        let mut child = Command::new(guidance_bin())
            .args([
                "mcp",
                "--db",
                db.to_str().unwrap(),
                "--workspace",
                workspace.to_str().unwrap(),
                "--toolset",
                toolset,
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        }
    }

    fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "id": id,
            "params": params,
        });
        writeln!(self.stdin, "{}", serde_json::to_string(&request).unwrap()).expect("write");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read");
        serde_json::from_str(&line).expect("parse response")
    }

    fn tool_names(&mut self) -> Vec<String> {
        let response = self.call("tools/list", serde_json::json!({}));
        response["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string))
            .collect()
    }

    fn explain(&mut self, arguments: serde_json::Value) -> serde_json::Value {
        self.call(
            "tools/call",
            serde_json::json!({"name": "guidance_explain", "arguments": arguments}),
        )
    }

    /// Write a raw stdio line, read exactly one raw response line.
    fn raw(&mut self, line: &str) -> String {
        writeln!(self.stdin, "{line}").expect("write");
        self.stdin.flush().expect("flush");
        let mut out = String::new();
        self.stdout.read_line(&mut out).expect("read");
        out
    }
}

/// Every server stdout line is a JSON-RPC 2.0 response: versioned
/// envelope, exactly one of `result`/`error`, and never a
/// server-initiated request (no `method` member — the server cannot
/// elicit, prompt, or expect a client reply).
fn assert_response_envelope(value: &serde_json::Value, context: &str) {
    assert_eq!(value["jsonrpc"], "2.0", "{context}");
    assert!(
        value.get("method").is_none(),
        "server-initiated request: {context}"
    );
    let has_result = value.get("result").is_some();
    let has_error = value
        .get("error")
        .is_some_and(serde_json::Value::is_object);
    assert!(
        has_result ^ has_error,
        "result/error exclusivity: {context}"
    );
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), "pub fn mcp_anchor_fn() {}\n").expect("write");
    dir
}

fn content_text(response: &serde_json::Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn full_toolset_lists_explain_and_status() {
    let dir = workspace();
    let mut client = McpClient::spawn(dir.path(), "full");
    let names = client.tool_names();
    assert!(names.contains(&"guidance_explain".to_string()), "{names:?}");
    assert!(names.contains(&"guidance_status".to_string()), "{names:?}");
}

#[test]
fn agent_toolset_is_search_only() {
    let dir = workspace();
    let mut client = McpClient::spawn(dir.path(), "agent");
    let names = client.tool_names();
    assert!(names.contains(&"guidance_explain".to_string()), "{names:?}");
    assert!(!names.contains(&"guidance_status".to_string()), "{names:?}");
    // Status calls are rejected under the agent toolset.
    let response = client.call(
        "tools/call",
        serde_json::json!({"name": "guidance_status", "arguments": {}}),
    );
    assert!(response["error"].is_object(), "{response}");
}

#[test]
fn explain_reports_freshness_and_coverage_with_empty_results() {
    let dir = workspace();
    let mut client = McpClient::spawn(dir.path(), "full");
    let response = client.explain(serde_json::json!({"query": "mcp_anchor_fn"}));
    let text = content_text(&response);
    assert!(text.contains("freshness: "), "{text}");
    assert!(text.contains("coverage: ranked_sample"), "{text}");
    assert!(text.contains("No results found."), "{text}");
}

#[test]
fn explain_enforces_input_bounds() {
    let dir = workspace();
    let mut client = McpClient::spawn(dir.path(), "full");
    // Limit clamps (no error); oversize query errors.
    let response = client.explain(serde_json::json!({"query": "a", "limit": 500}));
    assert!(response["result"].is_object(), "{response}");
    let long = "x".repeat(4001);
    let response = client.explain(serde_json::json!({"query": long}));
    assert_eq!(response["error"]["code"], -32602, "{response}");
    // Conflicting routes error; bad freshness errors.
    let response = client.explain(serde_json::json!({"query": "a", "fts": true, "vector": true}));
    assert!(response["error"].is_object(), "{response}");
    let response = client.explain(serde_json::json!({"query": "a", "freshness": "soon"}));
    assert!(response["error"].is_object(), "{response}");
    // Missing query errors.
    let response = client.explain(serde_json::json!({}));
    assert_eq!(response["error"]["code"], -32602, "{response}");
}

#[test]
fn explain_hybrid_routes_accepted() {
    let dir = workspace();
    let mut client = McpClient::spawn(dir.path(), "full");
    for arguments in [
        serde_json::json!({"query": "mcp_anchor_fn", "fuse": true}),
        serde_json::json!({"query": "mcp_anchor_fn", "fts": true}),
        serde_json::json!({"query": "mcp_anchor_fn", "vector": true}),
        serde_json::json!({"queries": ["mcp_anchor_fn", "other"], "limit": 5}),
        serde_json::json!({"query": "mcp_anchor_fn", "freshness": "wait_for_fresh"}),
        serde_json::json!({"query": "mcp_anchor_fn", "globs": ["src/**"], "preferSymbol": true}),
    ] {
        let response = client.explain(arguments);
        assert!(response["result"].is_object(), "{response}");
        assert!(
            content_text(&response).contains("freshness: "),
            "{response}"
        );
    }
}

/// Two-file import fixture: `a_main` depends on `b_mod`, so an
/// explain for `foo` carries exactly one uncovered context line.
fn graph_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("b_mod.rs"),
        "pub fn b_helper() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("a_main.rs"),
        "mod b_mod;\nuse b_mod::b_helper;\npub fn foo() -> u64 {\n    b_helper()\n}\n",
    )
    .expect("write");
    dir
}

/// Context section of the CLI `explain` output: lines after
/// `### Context` starting with `- ` (workspace-relative, verbatim).
fn cli_context_lines(dir: &std::path::Path) -> Vec<String> {
    let root = dir.to_str().unwrap().to_string();
    let json_dir = dir.join(".guidance").to_str().unwrap().to_string();
    let db = dir.join("mcp.db").to_str().unwrap().to_string();
    let out = run(
        dir,
        &[
            "explain",
            "where is foo defined",
            "--workspace",
            &root,
            "--guidance",
            &json_dir,
            "--db",
            &db,
            "--limit",
            "10",
        ],
    );
    assert!(out.status.success(), "cli explain failed: {out:?}");
    let text = stdout(&out);
    let mut lines = Vec::new();
    let mut in_context = false;
    for line in text.lines() {
        if line.strip_prefix("### Context").is_some() {
            in_context = true;
            continue;
        }
        if in_context && line.starts_with("- ") {
            lines.push(line.to_string());
        }
    }
    lines
}

#[test]
fn explain_context_flag_off_is_byte_identical_and_on_matches_cli() {
    let dir = graph_workspace();
    sync(dir.path(), "mcp.db", false);
    let cli_lines = cli_context_lines(dir.path());
    assert!(!cli_lines.is_empty(), "fixture must carry context");
    let mut client = McpClient::spawn(dir.path(), "full");
    let unset = client.explain(serde_json::json!({"query": "where is foo defined"}));
    let off = client.explain(
        serde_json::json!({"query": "where is foo defined", "context": false}),
    );
    // Flag-off is byte-identical to today: same result object, no
    // `context` key either way.
    assert_eq!(unset["result"], off["result"], "{unset} vs {off}");
    assert!(unset["result"].get("context").is_none(), "{unset}");
    let on = client.explain(
        serde_json::json!({"query": "where is foo defined", "context": true}),
    );
    // Hits text untouched by the flag; context arrives as a separate
    // array beside — never inside — the hits payload.
    assert_eq!(
        on["result"]["content"], unset["result"]["content"],
        "{on} vs {unset}"
    );
    let context: Vec<String> = on["result"]["context"]
        .as_array()
        .expect("context array")
        .iter()
        .map(|value| value.as_str().expect("line").to_string())
        .collect();
    assert_eq!(context, cli_lines, "shared-builder proof");
}

#[test]
fn stdio_transport_frames_one_response_per_line_with_id_echo() {
    let dir = workspace();
    let mut client = McpClient::spawn(dir.path(), "full");
    // Distinct id shapes echo back on their own single lines.
    let line = client.raw(r#"{"jsonrpc":"2.0","method":"initialize","id":7,"params":{}}"#);
    assert_eq!(line.trim().lines().count(), 1, "one line");
    let first: serde_json::Value = serde_json::from_str(line.trim()).expect("valid JSON");
    assert_response_envelope(&first, "initialize");
    assert_eq!(first["id"], 7, "numeric id echoes");
    let line = client.raw(r#"{"jsonrpc":"2.0","method":"tools/list","id":"abc","params":{}}"#);
    let second: serde_json::Value = serde_json::from_str(line.trim()).expect("valid JSON");
    assert_response_envelope(&second, "tools/list");
    assert_eq!(second["id"], "abc", "string id echoes");
    // Blank lines earn silence: blank, then two valid requests, must
    // read back exactly those two responses in order (a phantom blank
    // response would shift every subsequent read).
    writeln!(client.stdin).expect("blank line");
    client.stdin.flush().expect("flush");
    let line = client.raw(
        r#"{"jsonrpc":"2.0","method":"tools/call","id":21,"params":{"name":"guidance_status","arguments":{}}}"#,
    );
    let third: serde_json::Value = serde_json::from_str(line.trim()).expect("valid JSON");
    assert_response_envelope(&third, "post-blank status");
    assert_eq!(third["id"], 21);
    let line = client.raw(
        r#"{"jsonrpc":"2.0","method":"tools/call","id":22,"params":{"name":"guidance_status","arguments":{}}}"#,
    );
    let fourth: serde_json::Value = serde_json::from_str(line.trim()).expect("valid JSON");
    assert_eq!(fourth["id"], 22);
    // Malformed JSON earns a structured error with null id — still
    // exactly one framed line, still an envelope.
    let line = client.raw("{not json");
    assert_eq!(line.trim().lines().count(), 1, "one line");
    let bad: serde_json::Value = serde_json::from_str(line.trim()).expect("valid JSON");
    assert_response_envelope(&bad, "malformed");
    assert!(bad["id"].is_null(), "unparseable id is null: {bad}");
    assert!(bad["error"].is_object(), "{bad}");
}

#[test]
fn server_never_initiates_requests_or_elicitations() {
    // A session across every response shape — valid result, tool
    // argument error, unknown method, malformed frame. No server line
    // may carry `method`: the server answers, never asks (no
    // elicitation, no prompt, no server-to-client notification).
    let dir = workspace();
    let mut client = McpClient::spawn(dir.path(), "full");
    let frames = [
        r#"{"jsonrpc":"2.0","method":"initialize","id":31,"params":{}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"tools/call","id":32,"params":{"name":"guidance_explain","arguments":{"query":"mcp_anchor_fn"}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"tools/call","id":33,"params":{"name":"guidance_explain","arguments":{}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"no_such_tool","id":34}"#.to_string(),
        "{broken".to_string(),
    ];
    for frame in &frames {
        let line = client.raw(frame);
        let value: serde_json::Value =
            serde_json::from_str(line.trim()).expect("framed JSON");
        assert_response_envelope(&value, frame);
    }
}

#[test]
fn status_reports_counts() {
    let dir = workspace();
    let mut client = McpClient::spawn(dir.path(), "full");
    let response = client.call(
        "tools/call",
        serde_json::json!({"name": "guidance_status", "arguments": {}}),
    );
    let text = content_text(&response);
    assert!(text.contains("node_count"), "{text}");
}
