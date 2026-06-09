---
name: rdf-parsing
description: RDF triple mapping embedded in the Coral ingestion pipeline. TripleMapper converts (subject, predicate, object) strings into ContextNode pairs for the Library database.
anchors:
  - TripleMapper
  - IngestError
  - BatchIngestor
  - ContextNode
---

# RDF Parsing

Unlike the Zig version which has a full standalone `src/rdf/` module with Turtle lexer, recursive-descent parser, and N-Quads streaming parser, the Rust implementation does **not** have a standalone RDF parser crate. N-Triples parsing is simpler and done via line-by-line string processing embedded directly in the ingestion pipeline.

## Architecture

```
Line-by-line string splitting
  → TripleMapper::map_triple(subject, predicate, object)
  → (ContextNode, ContextNode) pair
  → BatchIngestor::add() → Library::insert_node()
```

Raw RDF parsing (Turtle, N-Quads) is a **future concern** — currently the pipeline expects pre-split triples as strings.

## Core types

| Type | Location | Purpose |
|------|----------|---------|
| `TripleMapper` | `coral/src/ingest.rs:57` | Maps (s, p, o) strings to `ContextNode` pairs |
| `BatchIngestor` | `coral/src/ingest.rs:21` | Batched node insertion with automatic flush |
| `ContextNode` | `common/src/types.rs:234` | Node with name, source, LOD levels, optional embedding |

## Example

```rust
use coral::ingest::TripleMapper;

let mapper = TripleMapper::new();
let (subject, object) = mapper.map_triple("Zig", "is_a", "language");
assert_eq!(subject.name.as_str(), "Zig");
assert_eq!(object.name.as_str(), "language");
assert_eq!(subject.lod.len(), 3); // full triple, edge, entity
```

## Key files

- `coral/src/ingest.rs` — `TripleMapper`, `BatchIngestor`, `IngestError`

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| RDF parser crate | Standalone `src/rdf/` (Turtle, N-Quads, normalize) | **None** — line-by-line string processing only |
| Triple type | `Triple { subject, predicate, object: Term }` | `TripleMapper::map_triple(s, p, o) → (ContextNode, ContextNode)` |
| IRI normalization | `rdf.normalize` module | Not yet implemented |
| Streaming parser | `NQuadsParser` for `.nq`/`.nt` | Not yet implemented |
| Tokenizer | `rdf.lexer` for Turtle | Not yet implemented |

## Zig reference

See `doc/capabilities/rdf-parsing/CAPABILITY.md` in the Zig project for the original module design (Turtle parser, N-Quads streaming, IRI normalization).
