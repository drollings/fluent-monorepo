---
name: coral-ingestion
description: Batch ingestion pipeline for RDF/YAGO 4.5 datasets into the Coral Library. BatchIngestor fluent API parses N-Quads/Turtle, maps triples to ContextNodes via TripleMapper, applies a YAGO_TYPE_WHITELIST to keep the graph under 5M nodes / 1 GB, and flushes to Library via insertNode/insertRdfEdge.
---

# Coral Ingestion

Converts large RDF datasets (specifically YAGO 4.5) into the Coral Library's `ContextNode` graph. The pipeline is designed for sparse loads: only whitelisted YAGO types are kept.

## CLI

```bash
coral ingest [--file <path> | <path>]
```

Prints `IngestStats` (nodes inserted, edges inserted, triples skipped) on completion.

## Pipeline

```
N-Quads / Turtle file
  → rdf.Parser (Turtle lexer + parser) or rdf.NQuadsParser
  → ontology.TripleMapper.addTriple() — per-triple type check against YAGO_TYPE_WHITELIST
  → BatchIngestor.flushBatch() → mapper.flush(library)
      → library.insertNode()   (per ContextNode)
      → library.insertRdfEdge() (per predicate edge)
```

## BatchIngestor fluent API

```zig
var ingestor = try BatchIngestor.init(allocator, &library)
    .batchSize(1000)
    .embedder(provider)
    .build();
defer ingestor.deinit();
try ingestor.ingestFile("yago-facts.nt");
const stats = ingestor.stats();
```

## YAGO_TYPE_WHITELIST

Defined in `src/coral/config.zig`. Only entities whose `rdf:type` matches a whitelisted YAGO class (e.g. `Person`, `Organization`, `SoftwareApplication`) are ingested. This enforces the <5M node / <1 GB SQLite target.

## Batch arena management

Each batch gets an `ArenaAllocator` (`batch_arena`) for all `TripleMapper` allocations. The arena is reset between batches, keeping peak memory proportional to batch size rather than total dataset size.

## Key files

- `src/coral/batch.zig` — `BatchIngestor`, `IngestStats`, `flushBatch`
- `src/ontology/mapper.zig` — `TripleMapper`, `MappingConfig`, `FlushResult`
- `src/ontology/yago.zig` — YAGO 4.5 type helpers, `YAGO_TYPE_WHITELIST`
- `src/ontology/inference.zig` — Rdfs/OWL inference rules applied during mapping
- `src/rdf/parser.zig` — Turtle `Parser`, `Triple`, `Term`
- `src/rdf/nquads.zig` — N-Quads streaming parser
- `src/coral/config.zig` — `YAGO_TYPE_WHITELIST` constant
