//! P5 `mcp-contract` port: toolsets, background (eventual) default, input
//! bounds, elicitation-free local operation, and the freshness/coverage
//! contract — driven against the built binary over stdio.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command};

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
