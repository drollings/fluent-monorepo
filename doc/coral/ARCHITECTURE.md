# coral-context — Architecture

> `doc/coral/OVERVIEW.md` and `doc/coral/DETAILS.md` describe an
> earlier Zig-era design; their `src/**/*.zig` paths no longer
> exist. This document is the current index over the Rust tree
> (`src/coral/`, package `coral-context`; binaries in
> `src/bin/coral` and `src/bin/guidance`).

## Module table

| Module | Purpose | Path |
|--------|---------|------|
| `db` | `Library` graph store: `db/nodes.rs::{insert_node(s), knn_search}`, `db/edges.rs::traverse_from`, `db/schema.rs`, `db/hnsw.rs`, `db/embeddings.rs` | `src/coral/src/db/` |
| `cache` | `cache/reactor.rs` tier cascade over `TierRegistry` (`tier_units::{L2WasmUnit, L3GraphUnit, L4SemanticUnit, L5FrontierUnit}`), `cache/stats.rs`, `cache_l1.rs`, `cache_router.rs` | `src/coral/src/cache/`, `cache_l1.rs`, `cache_router.rs` |
| `mcp` | JSON-RPC 2.0 over STDIO: `coral_query` / `coral_insert` / `coral_traverse` handlers | `src/coral/src/mcp.rs` |
| `wasm_runtime` | Sandboxed WASM plugin runtime (`CachedPlugin`, no host access by default) | `src/coral/src/wasm_runtime.rs` |
| `packer` | LOD text selection (`get_lod_text`, `PackedNode`) over `Library`; re-exports `fluent_llm::ContextPacker` (single packing primitive, defined in `src/llm/src/context_packer.rs`) | `src/coral/src/packer.rs` |
| `ingest` | Batch RDF/Turtle ingestion (`IngestionConfig`, `IngestStats`, transactional flush) | `src/coral/src/ingest.rs` |
| `knowledge` / `tier_units` / `wvr` | Knowledge helpers, tier `WorkUnit`s, `fluent-wvr` trait bridges | `src/coral/src/knowledge.rs`, `tier_units.rs`, `wvr.rs` |
| `error` / `test_stubs` | `LibraryError` taxonomy; test doubles | `src/coral/src/error.rs`, `test_stubs.rs` |

## Load-bearing invariants

- Canonical store type is `fluent_types::ContentNode` (shared with
  the router ledger); the SQLite table keeps the name
  `context_nodes`.
- No second packing primitive, no second method table: the MCP
  methods are exactly the three handlers above.
- LOD tiers derive from full text, never chained; see `VISION.md`.
