---
name: coral-ingestion
description: Batch ingestion of ContextNodes into the Library with triple mapping for RDF-style data
anchors:
  - BatchIngestor
  - IngestStats
  - TripleMapper
  - ingest_file
---

# Coral Ingestion

Batch-ingests `ContextNode` values into the `Library` database. `BatchIngestor` accumulates nodes and flushes them on configurable batch size or when a node carries an embedding. `TripleMapper` converts RDF-style subject–predicate–object triples into a pair of `ContextNode`s with populated LOD (Level of Detail) fields.

## Key files

- `coral/src/ingest.rs` — `BatchIngestor`, `IngestStats`, `IngestError`
- `types/src/lib.rs` — `ContextNode`, `NodeId` (crate: `guidance-types`)

## Semantic Deviations

- **No arena allocator** — uses scoped `Vec` allocations instead of Zig's `ArenaAllocator`; all memory is reclaimed when `BatchIngestor` goes out of scope
- **Arc<Library>** for shared ownership instead of Zig's `*Library`
- **`IngestStats` tracks counts** — `triples_processed`, `nodes_created`, `edges_created` updated during ingestion
- **`flush()` is a no-op** in the current implementation — nodes are inserted immediately via `library.insert_node()` in `add()`; `flush()` only clears the internal batch Vec
- **TripleMapper** returns `(ContextNode, ContextNode)` pair rather than inserting directly — the caller controls persistence

## Example

```rust
use std::sync::Arc;
use guidance_types::ContextNode;
use guidance_coral::db::Library;
use guidance_coral::ingest::{BatchIngestor, TripleMapper};

let lib = Arc::new(Library::open_in_memory().expect("db"));
let mut ingestor = BatchIngestor::new(lib, 100);

let node = ContextNode {
    id: None,
    name: "Zig".into(),
    source: "Zig is a systems language".into(),
    lod: vec![],
    embedding: None,
};
let id = ingestor.add(node).expect("add");

let mapper = TripleMapper::new();
let (sub, obj) = mapper.map_triple("Zig", "is_a", "programming_language");
assert_eq!(sub.name.as_str(), "Zig");
assert_eq!(obj.name.as_str(), "programming_language");
```

## Zig reference

See `../src/coral/ingest.zig` in the Zig coral source tree for the original `BatchIngestor`, `IngestStats`, and `TripleMapper` with `ArenaAllocator`.
