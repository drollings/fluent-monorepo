---
name: local-model-decomposition
description: L4.5 cache tier that calls a local LLM to decompose a complex query into up to 5 ordered sub-tasks, routes each sub-task through QueueReactor recursively (max depth enforced), merges and deduplicates results, and caches the solution so future identical queries hit L1 or L4.
anchors:
  - LocalDecomposer
  - DecomposerConfig
  - decompose
---

# Local Model Decomposition

`LocalDecomposer` in `src/common/local_model.zig` implements the L4.5 tier of `QueueReactor`. It bridges complex multi-step queries and the existing L1–L4 tiers by using a local LLM to plan a decomposition strategy.

## How it works

```
Complex query
  → LocalDecomposer.decompose(arena, query)
      → LlmClient.complete() with task-planner system prompt
      → JSON array response parsed by isMalformedResponse + parseJsonArray
      → [][]const u8 sub-task list (fallback: single-element slice if LLM fails)
  → for each sub-task: reactor.route(sub_task)   ← recursive, depth-limited
  → merge + deduplicate ContextNode results
  → persistSolution(query, merged_result)         ← caches for future queries
```

## DecomposerConfig

```zig
pub const DecomposerConfig = struct {
    llm: LlmConfig,          // endpoint + model
    max_subtasks: usize = 5,
    max_depth: u8 = 2,       // max recursive route() depth
};
```

## Robustness

`isMalformedResponse()` detects non-array JSON, empty arrays, and think-block noise (e.g. `<think>…</think>` preamble from reasoning models). On any failure the function returns a single-element slice containing the original query, so `route()` always proceeds.

## Solution caching

After a successful L4.5 result, `persistSolution()` stores a `ContextNode` with:
- `lod[4]` = query text (entity name)
- `lod[0]` = node-name summary of matched nodes

The node ID is derived from a SHA-256 hash of the query, making re-inserts idempotent (`AlreadyExists` is caught and ignored). The next similar query hits L4 KNN instead of re-decomposing.

## Key files

- `src/common/local_model.zig` — `LocalDecomposer`, `DecomposerConfig`
- `src/coral/cache.zig` — `QueueReactor.localDecompose()`, `CacheTier.l4_5_decompose`, `persistSolution()`
- `src/llm/root.zig` — `LlmClient`, `LlmConfig`

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (3 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/llm/llm.zig` | 1.0 | defines_anchor |
| `src/llm/root.zig` | 0.9 | used_by |
| `src/llm/root_tests.zig` | 0.9 | used_by |

