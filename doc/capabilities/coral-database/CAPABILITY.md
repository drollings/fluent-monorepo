---
name: coral-database
description: SQLite-backed graph database with KNN search, node insertion, and recursive graph traversal
anchors:
  - Library
  - ContextNode
  - knn_search
  - insert_node
  - traverse_from
  - NodeId
  - KnnHit
---

# Coral Database

Provides a persistent graph database built on SQLite. The `Library` struct manages `context_nodes` (with optional `f32` embedding vectors), directed `edges`, `wasm_tools`, and an `embedding_cache`. Supports KNN search via brute-force cosine distance over stored embeddings, recursive graph traversal via CTEs, and node/edge CRUD.

## Key files

- `coral/src/db.rs` — `Library`, `LibraryError`, KNN search, recursive traversal, schema init
- `types/src/lib.rs` — `ContextNode`, `NodeId`, `KnnHit`, `GraphNode`, `WasmTool` (crate: `guidance-types`)

## Semantic Deviations

- **rusqlite** replaces raw `sqlite3` C bindings — safe Rust wrapper with prepared statements, `params![]` macro, `FromRow` callback pattern
- **Arc (std::sync::Arc)** for shared ownership of `Library` across reactor/router/MCP instead of Zig's slice+arena pattern for text
- **Mutex<rusqlite::Connection>** for interior mutability — Zig uses async or explicit `*` pointers
- **Blob encoding** for embeddings — `f32` vector serialized as little-endian bytes (`vec_to_blob` / `blob_to_vec`)
- **Cosine distance** computed in Rust — brute-force scan over all stored embeddings; no FTS or index acceleration
- **CTE recursion** for graph traversal — `WITH RECURSIVE` SQL instead of Zig's iterative BFS/DFS
- **No arena allocator** — `String` / `Vec` allocations are scoped and dropped normally

## Example

```rust
use guidance_types::{ContextNode, NodeId};
use guidance_coral::db::Library;

let lib = Library::open_in_memory().expect("db");

let node = ContextNode {
    id: None,
    name: "hello_world".into(),
    source: "pub fn hello() void {}".into(),
    lod: vec!["full".into()],
    embedding: Some(vec![0.1, 0.2, 0.3]),
};
let node_id = lib.insert_node(&node).expect("insert");

let hits = lib.knn_search(&[0.1, 0.2, 0.3], 5).expect("search");
assert!(!hits.is_empty());
```

## Zig reference

See `../src/coral/db.zig` in the Zig coral source tree for the original `Library` / `ContextNode` / `knn_search` implementation.
