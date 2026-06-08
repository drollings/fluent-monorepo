---
name: coral-mcp
description: JSON-RPC 2.0 MCP server over tokio async STDIO for querying, inserting, and traversing the coral graph
anchors:
  - McpServer
  - handle_request
  - coral_query
  - coral_insert
  - coral_traverse
  - JsonRpcRequest
  - JsonRpcResponse
---

# Coral MCP

Provides a JSON-RPC 2.0 MCP (Model Context Protocol) server that reads requests from STDIO and returns JSON responses. Supports three methods: `coral_query` (lookup node by name), `coral_insert` (create a new `ContextNode`), and `coral_traverse` (recursively traverse edges from a node). The `McpServer` holds an `Arc<Library>` and dispatches requests synchronously.

## Key files

- `coral/src/mcp.rs` — `McpServer`, `JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcError`, `McpError`

## Semantic Deviations

- **tokio async STDIO** — the server is designed to run in a tokio async runtime, reading lines from stdin and writing JSON responses to stdout (production wiring uses `tokio::io::BufReader`/`tokio::io::AsyncWriteExt`)
- **serde_json** for JSON-RPC parsing instead of `std.json` — `#[derive(Deserialize)]` on `JsonRpcRequest` for zero-copy-ish deserialization
- **No arena per request** — all request-scoped allocations (`String`, `Vec`) use the global allocator and drop naturally
- **Synchronous dispatch** — `handle_request()` is not async; the caller wraps it in `tokio::task::spawn_blocking` if needed
- **Method routing** via `match request.method.as_str()` rather than Zig's `inline else` comptime dispatch
- **Error codes** follow JSON-RPC 2.0 conventions (`-32601` method not found, `-32602` invalid params, `-32000` server error)

## Example

```rust
use std::sync::Arc;
use guidance_coral::db::Library;
use guidance_coral::mcp::McpServer;

let lib = Arc::new(Library::open_in_memory().expect("db"));
let server = McpServer::new(lib);

// Insert a node
let req = r#"{"jsonrpc":"2.0","method":"coral_insert","id":1,"params":{"name":"my_node","source":"content","lod":[],"embedding":null}}"#;
let resp = server.handle_request(req).expect("handle");

// Query it back
let query = r#"{"jsonrpc":"2.0","method":"coral_query","id":2,"params":{"name":"my_node"}}"#;
let qresp = server.handle_request(query).expect("handle");
```

## Zig reference

See `../src/coral/mcp.zig` in the Zig coral source tree for the original `McpServer` with arena-per-request allocation and `std.json` parsing.
