---
name: ontology
description: RDF triple-to-ContextNode mapping embedded in the Coral ingestion pipeline. No standalone ontology crate; mapping logic lives in the ingest module.
anchors:
  - TripleMapper
  - BatchIngestor
  - ContextNode
---

# Ontology

The Rust codebase has no standalone `ontology/` crate. Ontology mapping (RDF triples → `ContextNode` records) is embedded directly in the Coral ingestion pipeline within `coral/src/ingest.rs`. The `TripleMapper` struct converts `(subject, predicate, object)` triples into a pair of `ContextNode` values with pre-computed LOD slices.

## TripleMapper

A minimal embedded mapper. It does not accumulate triples or perform batch flushes — it maps one triple at a time and returns two `ContextNode` values ready for insertion:

```rust
let mapper = TripleMapper::new();
let (subject_node, object_node) = mapper.map_triple("Zig", "is_a", "language");
```

## BatchIngestor

`BatchIngestor` wraps a `Library` reference and provides buffered insertion with explicit `flush()` control. Nodes with embeddings are flushed immediately; otherwise the batch flushes on capacity.

## Key files

- `coral/src/ingest.rs` — `TripleMapper`, `BatchIngestor`, `IngestError`

## Semantic Deviations

- **No standalone ontology crate** — `TripleMapper` is embedded in `coral::ingest` rather than split across `ontology/mapper.zig`, `ontology/inference.zig`, `ontology/yago.zig`, `ontology/migration.zig`
- **No `MappingConfig`** — configuration is hard-coded; no whitelist, inference level, or batch size settings
- **No `YAGO_TYPE_WHITELIST`** — the Rust version has no YAGO 4.5 integration; no type URI helpers, predicate priority ordering, or whitelist filtering
- **No inference engine** — `rdfs:subClassOf` transitivity, `owl:sameAs` merging, and `rdfs:domain`/`rdfs:range` inference are not implemented
- **No `FlushResult`** — `BatchIngestor::flush()` returns `Result<(), IngestError>` without summary stats
- **No schema migration** — SQLite schema migrations for `coral_nodes` / `coral_edges` are not present
- **`map_triple` returns two `ContextNode`s** — instead of accumulating triples and flushing in batch; each call is stateless

## Example

```rust
use coral::ingest::TripleMapper;

let mapper = TripleMapper::new();
let (sub, obj) = mapper.map_triple("Rust", "is_a", "language");
assert_eq!(sub.name.as_str(), "Rust");
assert_eq!(obj.name.as_str(), "language");
assert_eq!(sub.lod.len(), 3); // triple string, subject->object, subject
```

## Zig reference

See `../doc/capabilities/ontology/CAPABILITY.md` in the Zig guidance source tree for the original full ontology stack: `TripleMapper`, `MappingConfig`, `FlushResult`, inference engine, YAGO 4.5 integration, and schema migration.
