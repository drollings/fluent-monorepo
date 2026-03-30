---
name: coral-mcp
description: JSON-RPC 2.0 MCP server over STDIO for Coral. Exposes coral_query, coral_insert_node, and coral_explain tools to Claude Code, NullClaw, and Cursor. Each request gets an isolated arena; only the serialized response escapes to the caller.
---

# Coral MCP Server

`src/coral/mcp.zig` implements a Model Context Protocol server that wires external AI clients directly to the `QueueReactor` and `Library` without going through HTTP.

## Transport

JSON-RPC 2.0 over STDIO with Content-Length framing. Compatible with the MCP standard used by Claude Code and Cursor.

## CLI

```bash
coral mcp       # start MCP server on STDIO
```

## Exposed tools

| Tool | Description |
|------|-------------|
| `coral_query` | Route a natural-language query through the 5-tier cache (L1→L5) |
| `coral_insert_node` | Add a named ContextNode to the Library |
| `coral_explain` | BFS-expand neighbors of a named node; returns LOD-packed context |

## JSON-RPC methods handled

| Method | Action |
|--------|--------|
| `initialize` | Respond with server capabilities |
| `tools/list` | Return `TOOLS` array with JSON schemas |
| `tools/call` | Dispatch to `coral_query` / `coral_insert_node` / `coral_explain` |

## Arena strategy (Arena #5)

Each incoming request gets its own `ArenaAllocator`. All intermediate parsing and routing allocations live in this arena. Only the final serialized JSON response is duped to the caller's allocator before the arena is freed.

## Thread safety

`McpServer` holds references to a `Library` and `QueueReactor`, both of which are mutex-guarded for concurrent calls.

## Key files

- `src/coral/mcp.zig` — `McpServer`, `ToolDef`, `TOOLS`, JSON-RPC dispatch
- `src/coral/cache.zig` — `QueueReactor.route()` (called by `coral_query`)
- `src/coral/db.zig` — `Library.insertNode`, `Library.traverseFrom` (called by `coral_insert_node`, `coral_explain`)
- `src/coral/main.zig` — `coral mcp` subcommand entry point
