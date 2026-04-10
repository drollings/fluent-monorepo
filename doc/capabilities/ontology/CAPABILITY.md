---
name: ontology
description: YAGO 4.5 ontology processing layer that maps RDF triples to Coral ContextNodes, applies rdfs/OWL inference rules, provides schema migration utilities, and enforces the YAGO_TYPE_WHITELIST to keep ingested graphs within size budgets.
anchors:
  - TripleMapper
  - MappingConfig
  - FlushResult
  - YAGO_TYPE_WHITELIST
---

# Ontology

`src/ontology/` maps RDF triples (from the rdf-parsing module) into typed `ContextNode` records that can be persisted to the Coral Library.

## TripleMapper

The central component. Accumulates RDF triples and, on `flush(library)`, converts them to `ContextNode` and edge records:

```zig
var mapper = TripleMapper.init(allocator, &library, config);
try mapper.addTriple(triple);          // buffered
const result = try mapper.flush();     // → Library.insertNode / insertRdfEdge
```

`MappingConfig` controls: whitelist enforcement, inference level, batch size.

## Inference engine

`ontology.inference` applies a configurable set of rdfs/OWL rules during mapping:

- `rdfs:subClassOf` transitivity
- `owl:sameAs` merging
- `rdfs:domain`/`rdfs:range` type inference

Rules are applied before `flush()` writes to the Library, so inferred triples are stored as first-class edges.

## YAGO integration

`ontology.yago` provides:
- Type URI helpers (e.g., `yago:Person` → canonical string)
- Predicate priority ordering for LOD slot assignment
- `isYagoType(uri) bool` — fast check used by the whitelist filter

## Migration

`ontology.migration` provides schema upgrade helpers for the `coral_nodes` and `coral_edges` SQLite tables.

## Key files

- `src/ontology/mapper.zig` — `TripleMapper`, `MappingConfig`, `FlushResult`
- `src/ontology/inference.zig` — inference rule engine
- `src/ontology/yago.zig` — YAGO 4.5 type/predicate helpers
- `src/ontology/migration.zig` — SQLite schema migration
- `src/ontology/root.zig` — umbrella re-exports

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (5 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/ontology/mapper.zig` | 1.0 | defines_anchor |
| `src/ontology/root.zig` | 0.9 | used_by |
| `src/ontology/inference.zig` | 0.4 | path_heuristic |
| `src/ontology/yago.zig` | 0.4 | path_heuristic |
| `src/ontology/migration.zig` | 0.4 | path_heuristic |

