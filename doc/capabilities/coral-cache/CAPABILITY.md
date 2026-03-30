---
name: coral-cache
description: 5-tier cache hierarchy (L1 memory → L2 WASM → L3 graph → L4 KNN → L4.5 local decomposition → L5 LLM) for routing queries through the Coral knowledge base, implemented in QueueReactor with a fluent builder.
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
