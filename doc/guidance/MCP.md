# guidance MCP Server

The `guidance serve` command starts a Model Context Protocol (MCP) server over STDIO, allowing AI agents and IDE extensions to use guidance tools via JSON-RPC 2.0.

## Transport

- **Protocol**: JSON-RPC 2.0
- **Transport**: STDIO (stdin → request, stdout → response)
- **Framing**: newline-delimited JSON

## Starting the Server

```bash
guidance serve
```

Configure in Claude Desktop (`~/.config/claude-desktop/config.json`):

```json
{
  "mcpServers": {
    "guidance": {
      "command": "guidance",
      "args": ["serve"]
    }
  }
}
```

## Tools

### `explain`

AST-guided code search. Returns structural info about the query.

**Input schema**:
```json
{
  "type": "object",
  "required": ["query"],
  "properties": {
    "query":  { "type": "string" },
    "limit":  { "type": "integer", "default": 10 },
    "no_llm": { "type": "boolean", "default": false }
  }
}
```

### `gen`

Regenerate guidance JSON and `.guidance.db` for stale source files.

**Input schema**:
```json
{
  "type": "object",
  "properties": {
    "force": { "type": "boolean", "default": false },
    "file":  { "type": "string" }
  }
}
```

### `check`

Run the full RALPH loop: test → lint → fmt → guidance gen.

**Input schema**: `{}`

### `status`

Report generation status: synced, stale, missing files.

**Input schema**: `{}`

## Protocol Lifecycle

```
Client → {"jsonrpc":"2.0","id":1,"method":"initialize","params":{...}}
Server → {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05",...}}
Client → {"jsonrpc":"2.0","method":"notifications/initialized"}
Client → {"jsonrpc":"2.0","id":2,"method":"tools/list"}
Server → {"jsonrpc":"2.0","id":2,"result":{"tools":[...]}}
Client → {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"explain","arguments":{"query":"hash function"}}}
Server → {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"..."}]}}
```

## Error Codes

| Code   | Meaning              |
|--------|----------------------|
| -32700 | Parse error          |
| -32600 | Invalid request      |
| -32601 | Method not found     |
| -32602 | Invalid params       |
| -32000 | Tool execution error |
