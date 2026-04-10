---
name: coral-ingestion
description: Batch ingestion pipeline for RDF/YAGO 4.5 datasets into the Coral Library. BatchIngestor fluent API parses N-Quads/Turtle, maps triples to ContextNodes via TripleMapper, applies a YAGO_TYPE_WHITELIST to keep the graph under 5M nodes / 1 GB, and flushes to Library via insertNode/insertRdfEdge.
anchors:
  - BatchIngestor
  - IngestStats
  - TripleMapper
  - ingestFile
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

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (44 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/coral/batch.zig` | 1.0 | defines_anchor |
| `src/ontology/mapper.zig` | 1.0 | defines_anchor |
| `src/coral/cli.zig` | 0.9 | used_by |
| `src/coral/root.zig` | 0.9 | used_by |
| `src/ontology/root.zig` | 0.9 | used_by |
| `src/rdf/parser.zig` | 0.7 | keyword_overlap |
| `src/coral/verify.zig` | 0.4 | path_heuristic |
| `src/coral/frontier_tool_compiler.zig` | 0.4 | path_heuristic |
| `src/coral/main_tests.zig` | 0.4 | path_heuristic |
| `src/coral/targets.zig` | 0.4 | path_heuristic |
| `src/coral/db.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/pagerank.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/louvain.zig` | 0.4 | path_heuristic |
| `src/coral/cache.zig` | 0.4 | path_heuristic |
| `src/coral/delegation.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/union_find.zig` | 0.4 | path_heuristic |
| `src/coral/cache_l1.zig` | 0.4 | path_heuristic |
| `src/coral/benchmark.zig` | 0.4 | path_heuristic |
| `src/coral/type_inference.zig` | 0.4 | path_heuristic |
| `src/coral/yago_ingest.zig` | 0.4 | path_heuristic |
| `src/coral/mcp.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/degree_centrality.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/edge_weights.zig` | 0.4 | path_heuristic |
| `src/coral/metrics.zig` | 0.4 | path_heuristic |
| `src/coral/http_transport_test.zig` | 0.4 | path_heuristic |
| `src/coral/frozen_snapshot.zig` | 0.4 | path_heuristic |
| `src/coral/agent_loop.zig` | 0.4 | path_heuristic |
| `src/coral/http_transport.zig` | 0.4 | path_heuristic |
| `src/coral/token_budget.zig` | 0.4 | path_heuristic |
| `src/coral/config.zig` | 0.4 | path_heuristic |
| `src/coral/cache_router.zig` | 0.4 | path_heuristic |
| `src/coral/frontier.zig` | 0.4 | path_heuristic |
| `src/coral/global_search.zig` | 0.4 | path_heuristic |
| `src/coral/tool_registry.zig` | 0.4 | path_heuristic |
| `src/coral/executor.zig` | 0.4 | path_heuristic |
| `src/coral/cache_test.zig` | 0.4 | path_heuristic |
| `src/coral/cache_reactor.zig` | 0.4 | path_heuristic |
| `src/coral/schema.zig` | 0.4 | path_heuristic |
| `src/coral/algorithms/shortest_path.zig` | 0.4 | path_heuristic |
| `src/coral/csr_graph.zig` | 0.4 | path_heuristic |
| `src/coral/algorithm_runner.zig` | 0.4 | path_heuristic |
| `src/coral/main.zig` | 0.4 | path_heuristic |
| `src/coral/context_node_schema.zig` | 0.4 | path_heuristic |
| `src/coral/session.zig` | 0.4 | path_heuristic |

