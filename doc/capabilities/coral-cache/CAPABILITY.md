---
name: coral-cache
description: 5-tier cache hierarchy (L1 memory → L2 WASM → L3 graph → L4 KNN → L4.5 local decomposition → L5 LLM) for routing queries through the Coral knowledge base, implemented in QueueReactor with a fluent builder.
anchors:
  - QueueReactor
  - QueueReactorBuilder
  - L1Cache
  - RoutingResult
---

# Coral Cache

The `QueueReactor` in `src/coral/cache.zig` routes queries through five tiers, trying the fastest hit first and falling back progressively. Each tier has a latency budget:

| Tier | Name | Latency | Mechanism | Status |
|------|------|---------|-----------|--------|
| L1 | Memory Cache | <10ms | Exact query-hash → pre-rendered `ContextNode[]` in `StringHashMap` | Implemented |
| L2 | Workflow Cache | <50ms | Pre-compiled WASM tools via Extism | Stub — `findWasmTool()` wired, Extism execution is TODO (P3.3) |
| L3 | Graph Traversal | <200ms | SQLite recursive CTE BFS from named seed node | Implemented |
| L4 | Semantic Search | <500ms | KNN cosine similarity over stored embeddings | Implemented |
| L4.5 | Local Decomposition | variable | LLM splits query into subtasks; each subtask re-routed recursively | Implemented |
| L5 | LLM Fallback | >1s | External HTTP call to MCP/LLM endpoint | Implemented |

## QueueReactorBuilder (fluent API)

```zig
const reactor = try QueueReactorBuilder.init(allocator)
    .library(&lib)
    .embedder(embedding_provider)
    .knnK(10)
    .l4Threshold(0.7)
    .l3MaxDepth(4)
    .decomposerConfig(decomp_cfg)
    .build();
```

## Cache result promotion

After L3/L4/L4.5 produce a non-empty result, `persistSolution()` stores the result as a `ContextNode` in the Library so that future identical/similar queries hit L1 or L4 instead.

## Key files

- `src/coral/cache.zig` — `CacheTier`, `RoutingResult`, `L1Cache`, `QueueReactor`, `QueueReactorBuilder`
- `src/coral/db.zig` — `Library.knnSearch`, `Library.traverseFrom` (L3/L4 backends)
- `src/common/local_model.zig` — `LocalDecomposer` (L4.5)

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (41 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/coral/cache.zig` | 1.0 | defines_anchor |
| `src/coral/main.zig` | 0.9 | used_by |
| `src/coral/mcp.zig` | 0.9 | used_by |
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

