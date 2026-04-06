---
name: coral-mcp
description: JSON-RPC 2.0 MCP server over STDIO for Coral. Exposes coral_query, coral_insert_node, and coral_explain tools to Claude Code, NullClaw, and Cursor. Each request gets an isolated arena; only the serialized response escapes to the caller.
anchors:
  - McpServer
  - ToolDef
  - coral_query
  - coral_insert_node
  - coral_explain
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

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (42 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/coral/mcp.zig` | 1.0 | defines_anchor |
| `src/coral/main.zig` | 0.9 | used_by |
| `src/guidance/query_engine.zig` | 0.9 | used_by |
| `src/coral/frozen_snapshot.zig` | 0.4 | path_heuristic |
| `src/coral/triage.zig` | 0.4 | path_heuristic |
| `src/coral/agent_loop.zig` | 0.4 | path_heuristic |
| `src/coral/cli.zig` | 0.4 | path_heuristic |
| `src/coral/http_transport.zig` | 0.4 | path_heuristic |
| `src/coral/frontier_tool_compiler.zig` | 0.4 | path_heuristic |
| `src/coral/targets.zig` | 0.4 | path_heuristic |
| `src/coral/db.zig` | 0.4 | path_heuristic |
| `src/coral/token_budget.zig` | 0.4 | path_heuristic |
| `src/coral/config.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/pagerank.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/louvain.zig` | 0.4 | path_heuristic |
| `src/coral/frontier.zig` | 0.4 | path_heuristic |
| `src/coral/global_search.zig` | 0.4 | path_heuristic |
| `src/coral/tool_registry.zig` | 0.4 | path_heuristic |
| `src/coral/batch.zig` | 0.4 | path_heuristic |
| `src/coral/executor.zig` | 0.4 | path_heuristic |
| `src/coral/delegation.zig` | 0.4 | path_heuristic |
| `src/coral/cache.zig` | 0.4 | path_heuristic |
| `src/coral/fixtures.zig` | 0.4 | path_heuristic |
| `src/coral/quantized_embedding.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/union_find.zig` | 0.4 | path_heuristic |
| `src/coral/anonymize.zig` | 0.4 | path_heuristic |
| `src/coral/cache_test.zig` | 0.4 | path_heuristic |
| `src/coral/pattern.zig` | 0.4 | path_heuristic |
| `src/coral/schema.zig` | 0.4 | path_heuristic |
| `src/coral/benchmark.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/shortest_path.zig` | 0.4 | path_heuristic |
| `src/coral/type_inference.zig` | 0.4 | path_heuristic |
| `src/coral/yago_ingest.zig` | 0.4 | path_heuristic |
| `src/coral/csr_graph.zig` | 0.4 | path_heuristic |
| `src/coral/algorithm_runner.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/degree_centrality.zig` | 0.4 | path_heuristic |
| `src/coral/context_node_schema.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/edge_weights.zig` | 0.4 | path_heuristic |
| `src/coral/metrics.zig` | 0.4 | path_heuristic |
| `src/coral/session.zig` | 0.4 | path_heuristic |
| `src/coral/http_transport_test.zig` | 0.4 | path_heuristic |
| `src/coral/verify.zig` | 0.4 | path_heuristic |

