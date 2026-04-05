---
name: target-registry
description: DAG-based target registry for guidance build pipelines. TargetRegistry stores named Target nodes with dependency and capability bitsets. TargetBuilder (in registry.zig) is the canonical Fluent Builder pattern in this codebase — chained *Self setters, deferred error at register(). StringInterner provides thread-safe string→bitset-index interning via RwLock.
anchors:
  - TargetRegistry
  - TargetBuilder
  - Target
  - StringInterner
---

# Target Registry

`src/common/registry.zig` + `src/common/target.zig` + `src/common/interner.zig` implement the build target DAG used by both guidance (`src/guidance/`) and the coral ingestion pipeline (`src/coral/targets.zig`).

## Target

A `Target` represents one node in the capability DAG:

| Field | Type | Purpose |
|-------|------|---------|
| `id` | `i64` | Stable numeric ID |
| `name` | `[]const u8` | Interned name string |
| `target_type` | `TargetType` | `.file`, `.phony`, `.abstract` |
| `executor` | `ExecutorKind` | `.native`, `.docker`, `.wasm(WasmExecutor)` |
| `depends` | `DynamicBitSetUnmanaged` | Bitset of required capability indices |
| `provides` | `DynamicBitSetUnmanaged` | Bitset of provided capability indices |
| `command` | `[]const u8` | Shell command or empty |
| `essential` | `bool` | Must run in every plan |

`dependsSatisfiedBy(available)` and `distanceFrom(available)` enable topological sort and plan generation.

## TargetBuilder — canonical Fluent Builder

```zig
try registry.target("build", .file)
    .depends(&.{"compile", "link"})
    .provides(&.{"artifact"})
    .command("zig build -Doptimize=ReleaseFast")
    .essential()
    .register();
```

`TargetBuilder` accumulates configuration through chained `*Self` setters. Each setter tests `if (self.err != null) return self;` and records any allocation error in `err: ?anyerror`. The terminal method `register()` is the only `try` point — it surfaces the accumulated error, transfers ownership to the registry, and sets `self.target = null`.

The factory `TargetRegistry.target(name, type)` returns the builder by value. Any allocation failure at factory time is encoded in the builder's `err` field, not propagated to the caller.

## TargetSchema — DynamicEditable for Target

`TargetSchema` (in `target.zig`) builds a runtime `Accessor` array for `Target` fields, enabling `DynamicEditable`-style string-driven get/set. It must be **heap-allocated** via `TargetSchema.create(allocator, interner)` because the bitset vtables for `depends` and `provides` are stored in the schema struct itself — stack allocation would dangle the vtable pointers on move.

## StringInterner — RwLock-protected index mapping

`StringInterner` maps interned strings to monotonically increasing bit indices. Used by `TargetBuilder.depends()` and `.provides()` to convert string names to bitset positions.

Thread safety: `std.Thread.RwLock` with double-checked locking in `intern()` — shared lock for lookup (fast path), write lock for insertion (slow path).

## coral/targets.zig — YAGO ingestion DAG

`src/coral/targets.zig` defines the 7-node YAGO 4.5 ingestion pipeline as `IngestTargetDefs`:

```
yago_ingest (phony)
├── yago_download  — fetch YAGO 4.5 TTL
├── yago_parse     — Turtle → triples
├── yago_map       — triples → ContextNodes + edges
├── yago_embed     — compute embeddings
├── yago_index     — build ANN index
└── yago_verify    — integrity checks
```

These are static compile-time-known targets; `IngestTargetDefs` uses stack-allocated `[N]usize` dependency arrays. `MultiArrayList` / dynamic registry is not used here — the FLUENT_WVR_REFACTOR guidance explicitly preserves this as correct for a fixed small DAG.

## Key files

- `src/common/registry.zig` — `TargetRegistry`, `TargetBuilder`
- `src/common/target.zig` — `Target`, `TargetType`, `ExecutorKind`, `WasmExecutor`, `TargetSchema`
- `src/common/interner.zig` — `StringInterner` (RwLock + double-checked locking)
- `src/coral/targets.zig` — `IngestTargetDefs`, YAGO pipeline constants

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (8 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/dag/registry.zig` | 1.0 | defines_anchor |
| `src/dag/context.zig` | 0.9 | used_by |
| `src/dag/json_parser.zig` | 0.9 | used_by |
| `src/dag/repl.zig` | 0.9 | used_by |
| `src/dag/resolver.zig` | 0.9 | used_by |
| `src/dag/root.zig` | 0.9 | used_by |
| `src/dag/target.zig` | 0.7 | keyword_overlap |
| `src/coral/targets.zig` | 0.4 | path_heuristic |

