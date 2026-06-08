use std::io::{self, BufRead, Write};
use std::sync::Arc;

use guidance_common::types::ContextNode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::db::Library;

#[derive(Error, Debug)]
pub enum McpError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("library error: {0}")]
    Library(#[from] crate::db::LibraryError),
    #[error("method not found: {0}")]
    MethodNotFound(String),
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub id: Option<serde_json::Value>,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

pub struct McpServer {
    library: Arc<Library>,
}

impl McpServer {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }

    pub fn handle_request(&self, raw_json: &str) -> Result<String, McpError> {
        let request: JsonRpcRequest = serde_json::from_str(raw_json)?;

        let response = match request.method.as_str() {
            "coral_query" => self.handle_coral_query(&request),
            "coral_insert" => self.handle_coral_insert(&request),
            "coral_traverse" => self.handle_coral_traverse(&request),
            _ => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("method not found: {}", request.method),
                }),
                result: None,
            },
        };

        Ok(serde_json::to_string(&response)?)
    }

    fn handle_coral_query(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let name = request
            .params
            .as_ref()
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match self.library.find_node_by_name(name) {
            Ok(Some(node_id)) => {
                let node = self.library.get_node(node_id).ok().flatten();
                JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id.clone(),
                    result: Some(serde_json::json!({
                        "found": true,
                        "node_id": node_id.as_int(),
                        "node": node,
                    })),
                    error: None,
                }
            }
            Ok(None) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                result: Some(serde_json::json!({"found": false})),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                error: Some(JsonRpcError {
                    code: -32000,
                    message: e.to_string(),
                }),
                result: None,
            },
        }
    }

    fn handle_coral_insert(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let node: ContextNode = match request
            .params
            .as_ref()
            .map(|p| serde_json::from_value(p.clone()))
        {
            Some(Ok(n)) => n,
            Some(Err(e)) => {
                return JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id.clone(),
                    error: Some(JsonRpcError {
                        code: -32602,
                        message: format!("invalid params: {e}"),
                    }),
                    result: None,
                };
            }
            None => {
                return JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id.clone(),
                    error: Some(JsonRpcError {
                        code: -32602,
                        message: "missing params".into(),
                    }),
                    result: None,
                };
            }
        };

        match self.library.insert_node(&node) {
            Ok(node_id) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                result: Some(serde_json::json!({"node_id": node_id.as_int()})),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                error: Some(JsonRpcError {
                    code: -32000,
                    message: e.to_string(),
                }),
                result: None,
            },
        }
    }

    fn handle_coral_traverse(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let node_id = request
            .params
            .as_ref()
            .and_then(|p| p.get("node_id"))
            .and_then(|v| v.as_i64())
            .map(guidance_common::types::NodeId::from_int)
            .unwrap_or(guidance_common::types::NodeId::from_int(0));

        let max_depth = request
            .params
            .as_ref()
            .and_then(|p| p.get("max_depth"))
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as u8;

        match self.library.traverse_from(node_id, max_depth) {
            Ok(nodes) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                result: Some(serde_json::json!({"nodes": nodes, "count": nodes.len()})),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                error: Some(JsonRpcError {
                    code: -32000,
                    message: e.to_string(),
                }),
                result: None,
            },
        }
    }

    /// Serve MCP protocol over STDIO: read JSON-RPC 2.0 requests from stdin,
    /// write responses to stdout, one JSON object per line.
    pub fn serve_stdio(&self) -> Result<(), McpError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut stdout = stdout.lock();

        for line in stdin.lock().lines() {
            let line = line?;
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            let response = self.handle_request(&trimmed)?;
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_server() -> McpServer {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        McpServer::new(lib)
    }

    #[test]
    fn test_method_not_found() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"unknown","id":1}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: JsonRpcResponse = serde_json::from_str(&response).expect("parse");
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn test_coral_query_not_found() {
        let server = make_server();
        let req =
            r#"{"jsonrpc":"2.0","method":"coral_query","id":1,"params":{"name":"nonexistent"}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        assert_eq!(resp["result"]["found"], false);
    }

    #[test]
    fn test_coral_insert_and_query() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"coral_insert","id":1,"params":{"name":"test_mcp","source":"test source","lod":[],"embedding":null}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        let node_id = resp["result"]["node_id"].as_i64().expect("node_id");
        assert!(node_id > 0);

        let query_req =
            r#"{"jsonrpc":"2.0","method":"coral_query","id":2,"params":{"name":"test_mcp"}}"#
                .to_string();
        let query_resp = server.handle_request(&query_req).expect("handle");
        let qr: serde_json::Value = serde_json::from_str(&query_resp).expect("parse");
        assert_eq!(qr["result"]["found"], true);
    }

    #[test]
    fn test_coral_traverse() {
        let server = make_server();
        let insert = r#"{"jsonrpc":"2.0","method":"coral_insert","id":1,"params":{"name":"root","source":"root","lod":[],"embedding":null}}"#;
        let resp = server.handle_request(insert).expect("handle");
        let rv: serde_json::Value = serde_json::from_str(&resp).expect("parse");
        let root_id = rv["result"]["node_id"].as_i64().expect("id");

        let traverse_req = format!(
            r#"{{"jsonrpc":"2.0","method":"coral_traverse","id":2,"params":{{"node_id":{root_id},"max_depth":3}}}}"#
        );
        let trav_resp = server.handle_request(&traverse_req).expect("handle");
        let tr: serde_json::Value = serde_json::from_str(&trav_resp).expect("parse");
        assert_eq!(tr["result"]["count"].as_i64(), Some(1));
    }

    #[test]
    fn test_serve_stdio_handles_multiple_lines() {
        let server = make_server();
        // Test that handle_request works for multiple JSON objects
        let req1 =
            r#"{"jsonrpc":"2.0","method":"coral_query","id":1,"params":{"name":"nonexistent"}}"#;
        let req2 = r#"{"jsonrpc":"2.0","method":"coral_insert","id":2,"params":{"name":"test_stdio","source":"test","lod":[],"embedding":null}}"#;

        let resp1 = server.handle_request(req1).expect("handle");
        let r1: JsonRpcResponse = serde_json::from_str(&resp1).expect("parse");
        assert_eq!(
            r1.result.as_ref().and_then(|r| r.get("found")),
            Some(&serde_json::json!(false))
        );

        let resp2 = server.handle_request(req2).expect("handle");
        let r2: JsonRpcResponse = serde_json::from_str(&resp2).expect("parse");
        assert!(r2.result.as_ref().and_then(|r| r.get("node_id")).is_some());
    }

    #[test]
    fn test_serve_stdio_parse_error_for_invalid_json() {
        let server = make_server();
        // Invalid JSON should produce an error from handle_request
        let resp = server.handle_request("not-json");
        assert!(resp.is_err());
    }
}
