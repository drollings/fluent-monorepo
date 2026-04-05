---
name: coral-database
description: SQLite-backed knowledge graph storing ContextNodes with a 6-level LOD text pyramid, float embeddings as BLOBs, and graph edges. Supports KNN semantic search, recursive CTE graph traversal, duck-typing capability queries, and thread-safe concurrent access.
anchors:
  - Library
  - ContextNode
  - NodeId
  - knnSearch
  - insertNode
  - traverseFrom
---

# Coral Database

`Library` in `src/coral/db.zig` is the central SQLite handle for the Coral knowledge base. Every entity is a `ContextNode` with six LOD (level-of-detail) text slots, float embeddings, and graph edges.

## ContextNode LOD pyramid

| Level | Name | Purpose |
|-------|------|---------|
| lod[0] | full | Maximum detail — complete description |
| lod[1] | summary | Condensed but comprehensive |
| lod[2] | brief | Concise key points |
| lod[3] | tiny | Single sentence |
| lod[4] | name | Entity name / identifier |
| lod[5] | minimal | Abbreviation or alias |

Embeddings are stored as raw `f32` BLOB (native byte order). KNN search fetches all candidate BLOBs, decodes to `[]f32`, and sorts by cosine similarity in Zig — correct for ≤100K nodes.

## Key Library operations

| Function | Description |
|----------|-------------|
| `insertNode` | Upsert a ContextNode; mutex-guarded |
| `fetchNode` | Load by `NodeId` |
| `knnSearch` | Top-K by cosine similarity |
| `traverseFrom` | BFS via recursive CTE up to `max_depth` |
| `findNodeByName` | Lookup by lod[4] name |
| `isA(child_id, parent_name)` | Duck-typing capability check using recursive CTE |
| `insertNeighborOf` | Add directed graph edge; mutex-guarded |
| `insertWasmTool` | Register WASM tool by name/provides bitset; mutex-guarded |

## Thread safety

`Library` holds a `std.Thread.Mutex` on all write operations. `StringInterner` uses `std.Thread.RwLock` with double-checked locking in `intern()`.

## EdgeType

Graph edges between nodes are typed via `EdgeType`:

| Value | Meaning |
|-------|---------|
| `depends_on` | Dependency relationship (own SQL table) |
| `provides_capability` | Capability provision |
| `neighbor_of` | General adjacency |
| `semantic_similarity` | Proximity from KNN |
| `temporal_sequence` | Time-ordered sequence |

## Persistence implementation note (P1.4 deferred)

`insertNode()` and `fetchNode()` currently use manual `sqlite3_bind_text` / `sqlite3_column_text` calls for each LOD field. The `context_node_schema.zig` accessor table covers the same fields and is the intended replacement (making schema the single source of truth), but the P1.4 refactor was deferred: routing through `DynamicEditable` adds overhead at a non-boundary call site. The accessor table is used for binary IPC only; persistence remains manual binds.

## HydrationPipeline

`HydrationPipeline` embeds a node on insert (via `EmbeddingProvider`), stores the vector, then runs KNN to discover and persist neighbour edges automatically.

## ContextPacker

`ContextPacker` selects the appropriate LOD level for a node based on its BFS graph distance from the query root — close nodes get lod[0], distant nodes get lod[3] or lod[4].

## Key files

- `src/coral/db.zig` — `Library`, `ContextNode`, `HydrationPipeline`, `ContextPacker`, `NodeId`
- `src/coral/schema.zig` — SQL DDL, `LOD_COUNT = 6`, schema constants
- `src/coral/context_node_schema.zig` — Binary IPC schema, `BINARY_SCHEMA_VERSION`, `BinaryHeader`, `PayloadType`

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (67 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/common/types.zig` | 1.0 | defines_anchor |
| `src/coral/db.zig` | 1.0 | defines_anchor |
| `src/common/root.zig` | 0.9 | used_by |
| `src/guidance/ast_parser.zig` | 0.9 | used_by |
| `src/guidance/comment_checker.zig` | 0.9 | used_by |
| `src/guidance/comment_inserter.zig` | 0.9 | used_by |
| `src/guidance/comment_sync.zig` | 0.9 | used_by |
| `src/guidance/document_indexer.zig` | 0.9 | used_by |
| `src/guidance/hash.zig` | 0.9 | used_by |
| `src/guidance/header_generator.zig` | 0.9 | used_by |
| `src/guidance/json_store.zig` | 0.9 | used_by |
| `src/guidance/line_verify.zig` | 0.9 | used_by |
| `src/guidance/llm_filter.zig` | 0.9 | used_by |
| `src/guidance/llm_filter_batch.zig` | 0.9 | used_by |
| `src/guidance/main.zig` | 0.9 | used_by |
| `src/guidance/pattern.zig` | 0.9 | used_by |
| `src/guidance/plugin.zig` | 0.9 | used_by |
| `src/guidance/query_engine.zig` | 0.9 | used_by |
| `src/guidance/query_strategy.zig` | 0.9 | used_by |
| `src/guidance/ralph.zig` | 0.9 | used_by |
| `src/guidance/scanner.zig` | 0.9 | used_by |
| `src/guidance/schema_validator.zig` | 0.9 | used_by |
| `src/guidance/stage_builder.zig` | 0.9 | used_by |
| `src/guidance/staged.zig` | 0.9 | used_by |
| `src/guidance/sync.zig` | 0.9 | used_by |
| `src/guidance/sync_engine.zig` | 0.9 | used_by |
| `src/guidance/synthesize.zig` | 0.9 | used_by |
| `src/coral/frozen_snapshot.zig` | 0.4 | path_heuristic |
| `src/coral/triage.zig` | 0.4 | path_heuristic |
| `src/coral/agent_loop.zig` | 0.4 | path_heuristic |
| `src/coral/cli.zig` | 0.4 | path_heuristic |
| `src/coral/http_transport.zig` | 0.4 | path_heuristic |
| `src/coral/frontier_tool_compiler.zig` | 0.4 | path_heuristic |
| `src/coral/targets.zig` | 0.4 | path_heuristic |
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
| `src/coral/mcp.zig` | 0.4 | path_heuristic |
| `src/coral/csr_graph.zig` | 0.4 | path_heuristic |
| `src/coral/algorithm_runner.zig` | 0.4 | path_heuristic |
| `src/coral/scrub.zig` | 0.4 | path_heuristic |
| `src/coral/main.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/degree_centrality.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/edge_weights.zig` | 0.4 | path_heuristic |
| `src/coral/metrics.zig` | 0.4 | path_heuristic |
| `src/coral/session.zig` | 0.4 | path_heuristic |
| `src/coral/http_transport_test.zig` | 0.4 | path_heuristic |
| `src/coral/verify.zig` | 0.4 | path_heuristic |

