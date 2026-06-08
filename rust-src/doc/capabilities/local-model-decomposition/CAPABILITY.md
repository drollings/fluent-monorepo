---
name: local-model-decomposition
description: Multi-tier cache routing (L1/L3/L4) with KNN fallback and graph traversal. The Rust implementation omits the L4.5 LLM decomposition tier present in the Zig version.
anchors:
  - QueueReactor
  - ParallelRouter
  - RoutingResult
  - L1Cache
---

# Local Model Decomposition

The Rust cache reactor implements L1 (hot cache), L3 (graph traversal), and L4 (KNN vector search) tiers but does **not** include the L4.5 local-LLM decomposition tier from the Zig version. Complex queries are routed via `ParallelRouter` with KNN fallback and graph traversal rather than decomposed into sub-tasks.

## Cache tiers

| Tier | Mechanism | Present in Rust |
|------|-----------|-----------------|
| L1   | In-memory `HashMap` (hot cache) | Yes |
| L3   | Graph traversal (`traverse_all`) | Yes |
| L4   | KNN vector search (`knn_search`) | Yes |
| L4.5 | LLM decomposition + sub-task routing | **No** |

## Key files

- `coral/src/cache_reactor.rs` — `QueueReactor`, `QueueReactorCreateArgs`, `route()`
- `coral/src/cache_router.rs` — `ParallelRouter`, `route()`, `route_with_embedding()`
- `coral/src/cache_l1.rs` — `L1Cache`, `RoutingResult`

## Semantic Deviations

- **No `LocalDecomposer`** — the Rust code has no equivalent of Zig's `LocalDecomposer.decompose(arena, query)` or `DecomposerConfig`
- **No L4.5 tier** — complex queries that miss L1/L3/L4 return `CacheError::CacheMiss` instead of being decomposed by a local LLM
- **No solution caching** — the Rust reactor caches at L1 via `l1_cache.set()` but does not persist decomposed solutions to the Library
- **`QueueReactor` uses `bon::Builder`** for construction instead of Zig's explicit field-initialization pattern
- **`ParallelRouter`** replaces Zig's per-tier dispatch with a sequential fallthrough (L4 → L3 → miss)
- **No `persistSolution()`** — decomposed-solution persistence is not implemented

## Example

```rust
use std::sync::Arc;
use coral::cache_reactor::{QueueReactor, QueueReactorCreateArgs};
use coral::cache_l1::L1Cache;
use coral::db::Library;

let lib = Arc::new(Library::open_in_memory().expect("db"));
let reactor = QueueReactor::new(
    QueueReactorCreateArgs::builder()
        .library(lib)
        .l4_threshold(0.7)
        .knn_k(10)
        .build(),
);

match reactor.route("how does sync work") {
    Ok(result) => println!("Cache hit at tier: {}", result.tier),
    Err(_) => println!("Cache miss (no L4.5 decomposition fallback)"),
}
```

## Zig reference

See `../doc/capabilities/local-model-decomposition/CAPABILITY.md` in the Zig guidance source tree for the original `LocalDecomposer`, `DecomposerConfig`, `decompose()`, and `persistSolution()` implementation.
