---
name: coral-database
description: SQLite-backed knowledge graph storing ContextNodes with a 6-level LOD text pyramid, float embeddings as BLOBs, and graph edges. Supports KNN semantic search, recursive CTE graph traversal, duck-typing capability queries, and thread-safe concurrent access.
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
