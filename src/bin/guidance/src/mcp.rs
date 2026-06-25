//! MCP (Model Context Protocol) server for guidance.
//!
//! JSON-RPC 2.0 over STDIO, exposing guidance's search capabilities
//! as MCP tools for AI coding assistants.

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use guidance_core::memory::MemoryBridge;
use guidance_search_vector::GuidanceDb;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum McpError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database error: {0}")]
    Db(String),
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
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
    db: Arc<GuidanceDb>,
    memory: Option<MemoryBridge>,
}

impl McpServer {
    pub fn new(db: Arc<GuidanceDb>) -> Self {
        Self { db, memory: None }
    }

    pub fn with_memory(db: Arc<GuidanceDb>, memory: MemoryBridge) -> Self {
        Self {
            db,
            memory: Some(memory),
        }
    }

    pub fn handle_request(&self, raw_json: &str) -> Result<String, McpError> {
        let request: JsonRpcRequest = serde_json::from_str(raw_json)?;

        let response = match request.method.as_str() {
            "initialize" => self.handle_initialize(&request),
            "tools/list" => self.handle_tools_list(&request),
            "tools/call" => self.handle_tools_call(&request),
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

    fn handle_initialize(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id.clone(),
            result: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "guidance",
                    "version": "0.1.0"
                }
            })),
            error: None,
        }
    }

    fn handle_tools_list(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let mut tools: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "name": "guidance_explain",
                "description": "Search the codebase knowledge graph for identifiers, functions, modules, and patterns. Returns ranked results with source locations.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query (identifier, keyword, or natural language question)"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results (default: 10)"
                        }
                    },
                    "required": ["query"]
                }
            }),
            serde_json::json!({
                "name": "guidance_status",
                "description": "Get the status of the guidance database (node count, embedding count).",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            }),
        ];

        if let Some(ref memory) = self.memory {
            let schemas = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(memory.tool_schemas())
            });
            for schema in schemas {
                tools.push(serde_json::json!({
                    "name": schema.name,
                    "description": schema.description,
                    "inputSchema": schema.parameters,
                }));
            }
        }

        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id.clone(),
            result: Some(serde_json::json!({ "tools": tools })),
            error: None,
        }
    }

    fn handle_tools_call(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let tool_name = request
            .params
            .as_ref()
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let arguments = request
            .params
            .as_ref()
            .and_then(|p| p.get("arguments"))
            .cloned()
            .unwrap_or(serde_json::json!({}));

        match tool_name {
            "guidance_explain" => self.handle_guidance_explain(request, &arguments),
            "guidance_status" => self.handle_guidance_status(request),
            other if self.memory.is_some() => {
                let memory = self.memory.as_ref().unwrap();
                match tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(memory.handle_tool_call(other, &arguments))
                }) {
                    Ok(result) => JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: request.id.clone(),
                        result: Some(serde_json::json!({
                            "content": [{"type": "text", "text": result}]
                        })),
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
            _ => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                error: Some(JsonRpcError {
                    code: -32602,
                    message: format!("unknown tool: {tool_name}"),
                }),
                result: None,
            },
        }
    }

    fn handle_guidance_explain(
        &self,
        request: &JsonRpcRequest,
        arguments: &serde_json::Value,
    ) -> JsonRpcResponse {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let limit = arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        if query.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "query is required".into(),
                }),
                result: None,
            };
        }

        match self.db.hybrid_search(query, None, limit) {
            Ok(results) => {
                let items: Vec<serde_json::Value> = results
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "name": r.name,
                            "source": r.source,
                            "signature": r.signature,
                            "score": r.similarity,
                        })
                    })
                    .collect();

                JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id.clone(),
                    result: Some(serde_json::json!({
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string_pretty(&items).unwrap_or_default()
                        }]
                    })),
                    error: None,
                }
            }
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

    fn handle_guidance_status(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let node_count = self.db.get_node_count().unwrap_or(0);
        let embedding_count = self.db.get_embedding_count().unwrap_or(0);

        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id.clone(),
            result: Some(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::json!({
                        "node_count": node_count,
                        "embedding_count": embedding_count,
                        "hnsw_active": self.db.has_hnsw(),
                        "hnsw_points": self.db.hnsw_len(),
                    }).to_string()
                }]
            })),
            error: None,
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

/// Open a GuidanceDb from a path (or create in-memory if not found) and serve MCP over STDIO.
pub fn serve_stdio_from_path(db_path: &Path) -> Result<(), McpError> {
    let db = if db_path.exists() {
        GuidanceDb::open(db_path).map_err(|e| McpError::Db(e.to_string()))?
    } else {
        GuidanceDb::open_in_memory().map_err(|e| McpError::Db(e.to_string()))?
    };

    let memory = guidance_core::memory::init_memory_bridge();
    let server = match memory {
        Some(m) => McpServer::with_memory(Arc::new(db), m),
        None => McpServer::new(Arc::new(db)),
    };
    server.serve_stdio()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_server() -> McpServer {
        let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
        McpServer::new(db)
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
    fn test_initialize() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        assert_eq!(resp["result"]["serverInfo"]["name"], "guidance");
    }

    #[test]
    fn test_tools_list() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"tools/list","id":1,"params":{}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert!(tools.len() >= 2);
    }

    #[test]
    fn test_guidance_explain_empty_db() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"tools/call","id":1,"params":{"name":"guidance_explain","arguments":{"query":"hello"}}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        // Should return empty results, not an error
        assert!(resp["result"].is_object());
    }

    #[test]
    fn test_guidance_status() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"tools/call","id":1,"params":{"name":"guidance_status","arguments":{}}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        assert!(resp["result"].is_object());
    }
}
