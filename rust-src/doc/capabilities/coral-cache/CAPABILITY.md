---
name: coral-cache
description: Multi-tier routing cache with L1 (DashMap) hot cache and L3/L4 fallback via KNN and graph traversal
anchors:
  - QueueReactor
  - QueueReactorCreateArgs
  - L1Cache
  - RoutingResult
  - ParallelRouter
---

# Coral Cache

Implements a multi-tier caching strategy for routing queries. `QueueReactor` coordinates between an `L1Cache` (hot in-memory cache backed by `DashMap`), and a `ParallelRouter` that performs KNN search (L4 tier) or graph traversal (L3 tier) against the `Library` database. Results are cached in L1 on first miss to serve subsequent identical queries instantly.

## Key files

- `coral/src/cache_reactor.rs` — `QueueReactor`, `QueueReactorCreateArgs` (bon Builder)
- `coral/src/cache_l1.rs` — `L1Cache`, `RoutingResult` (DashMap-backed)
- `coral/src/cache_router.rs` — `ParallelRouter`

## Semantic Deviations

- **bon Builder** replaces Zig's `QueueReactorBuilder` — `#[derive(Builder)]` on `QueueReactorCreateArgs` with auto-generated `builder()` method and `.build()`
- **dashmap::DashMap** replaces Zig's `std.AutoHashMap` with concurrent read/write support — no explicit `Mutex` needed for L1
- **Arc<Library>** shared between reactor and router instead of Zig's `*Library` pointer
- **RoutingResult** is a plain `Clone + Serialize` struct rather than a tagged union
- **CacheError** lives in `guidance_common::error::CacheError` instead of a Zig error set

## Example

```rust
use std::sync::Arc;
use guidance_coral::cache_reactor::{QueueReactor, QueueReactorCreateArgs};
use guidance_coral::db::Library;

let lib = Arc::new(Library::open_in_memory().expect("db"));
let args = QueueReactorCreateArgs::builder()
    .library(lib)
    .knn_k(10)
    .l4_threshold(0.7)
    .build();
let reactor = QueueReactor::new(args);

// L1 miss fallback
let result = reactor.route("my query");
// Subsequent identical queries hit L1
let cached = reactor.route("my query");
```

## Zig reference

See `../src/coral/cache_reactor.zig`, `coral/cache_l1.zig`, `coral/cache_router.zig` in the Zig coral source tree for the original `QueueReactor`, `L1Cache`, and `ParallelRouter`.
