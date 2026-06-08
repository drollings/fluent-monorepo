# ROADMAP: Zig Monorepo → Rust Rewrite (`rust-src/`)

**Date:** 2026-06-08
**Architect:** Lead Software Engineer & Systems Architect
**Scope:** Full port of `src/guidance/` (AST Orchestrator) and `src/coral/` (Edge AI Orchestrator) into idiomatic, high-performance Rust under `rust-src/`.

---

## 0. Executive Summary

This roadmap ports a deterministic-first Zig monorepo—built on the **Fluent WVR** (Fluent Wrapped Vtables with Reflection) pattern—into Rust. The Zig codebase achieves Python-like ergonomics via comptime reflection, arena-backed fluent builders, and explicit `{ptr, vtable}` structs. The Rust rewrite **eliminates arena allocators entirely** and replaces them with strict ownership, `bon` builders, `dyn Trait` objects, and `serde` boundary serialization.

The two primary systems are:

1. **Guidance** — AST-guided vector search. Uses `tree-sitter` for parsing, `blake3` for file hashing, SQLite for vector search, and enforces the RALPH loop (test → lint → fmt → guidance → structure).
2. **Coral Context** — Neurosymbolic knowledge base and DAG workflow engine. Uses bitwise capability matching (`bitvec`), WASM sandboxing (`extism`), and LOD context nodes in SQLite.

**Strategic Constraint:** We use **only** the mandated crate list. No `petgraph` (custom DAG), no `typetag`, no `derive_builder`, no arena allocators.

---

## 1. Phase 0: Bootstrap & Infrastructure

**Goal:** Establish the `rust-src/` workspace, CI pipeline, and crate skeleton before any domain logic is written.

### 1.1 Workspace Layout

```
rust-src/
├── Cargo.toml                    # workspace root
├── crates/
│   ├── common/                   # Registry, interner, embeddings, hash, I/O
│   ├── guidance/                 # AST orchestrator: parse, sync, query, vector_db
│   ├── coral/                    # Edge AI orchestrator: db, cache, DAG, WASM
│   ├── dag/                      # Custom topological engine (no petgraph)
│   ├── llm/                      # HTTP client & frontier model fallback
│   └── wasm_ipc/                 # #[repr(C)] binary schemas for Extism
├── bin/
│   └── guidance.rs               # CLI dispatcher (clap derive)
└── doc/capabilities/             # Feature parity docs mirroring Zig tree
```

### 1.2 Cargo.toml Manifest

Define workspace members and **strict** dependency pinning:

| Category | Crates |
|----------|--------|
| CLI & FS | `clap` (derive), `notify`, `blake3` |
| Strings | `smol_str`, `internment` (`ArcIntern`) |
| Data | `serde`, `serde_json` |
| Builders | `bon` |
| Database | `rusqlite`, `sqlite-vec` |
| Orchestration | `bitvec`, `extism`, `async-openai` |
| Parsing | `tree-sitter` |
| Async | `tokio` (implied by `async-openai` / `notify`) |
| Testing | `insta`, `tempfile` |

**Rule:** Any crate not in the table above requires architect approval.

### 1.3 TDD Scaffold

- `cargo test --workspace` must pass before any PR is merged.
- Every module gets `tests/` subdirectory (mirrors Zig `_tests.zig` convention).
- Smoke tests verify the binary builds and `--help` exits 0.

---

## 2. Phase 1: `rust-src/common/` — The Foundation

**Rationale:** `src/common/` in Zig is the dependency root. It contains no imports from `guidance/`, `coral/`, or `dag/`. Porting it first breaks cyclic dependencies and gives us the shared vocabulary (interner, embeddings, hash, builders) that everything else consumes.

### 2.1 Analysis per Zig File / Capability

#### `src/common/interner.zig` → `crates/common/src/interner.rs`
- **Zig:** `StringInterner` with `ArenaAllocator`, `std.Thread.RwLock`, double-checked locking, and `DynamicBitSetUnmanaged` bridge.
- **Rust:** Replace arena with `DashMap<ArcIntern<str>, usize>` or `RwLock<HashMap<...>>`. `internment::ArcIntern<str>` gives us global, copy-on-write, weakly-referenced string deduplication **for free**.
- **Decision:** Do **not** port `StringInterner` literally. Instead, use `ArcIntern<str>` as the primary string handle across the entire Rust codebase. For the bitset bridge (mapping names → indices), maintain a thin `CapabilityRegistry` that wraps a `RwLock<HashMap<ArcIntern<str>, usize>>` and produces `bitvec::BitVec` instances.
- **Capability doc:** `doc/capabilities/target-registry/CAPABILITY.md`

#### `src/common/embeddings.zig` → `crates/common/src/embeddings.rs`
- **Zig:** Explicit `{ptr, vtable}` struct (`EmbeddingProvider`) with `NoopEmbedding`, `OllamaEmbedding`, `OpenAiEmbedding`.
- **Rust:** Define `trait EmbeddingProvider: Send + Sync { fn name(&self) -> &str; fn dimensions(&self) -> u32; fn embed(&self, text: &str) -> Vec<f32>; fn embed_batch(&self, texts: &[&str]) -> BatchEmbedding; }`. Implementors are concrete structs. The registry stores `Arc<dyn EmbeddingProvider>`.
- **Thread safety:** Zig used debug-build thread_id assertions. Rust enforces this at compile time via `Send + Sync` bounds. Ollama/OpenAI clients hold `reqwest::Client` (which is `Clone`/`Send`/`Sync`).
- **Capability docs:** `doc/capabilities/embedding-providers/CAPABILITY.md`, `doc/capabilities/vector-search/CAPABILITY.md`

#### `src/common/hash.zig` → `crates/common/src/hash.rs`
- **Zig:** SHA-256 hex, `blake3Hash`, `contentHashWithModel`, batch hashing.
- **Rust:** Use `blake3` crate directly. SHA-256 via `sha2` (transitive dep if needed, but `blake3` is mandated). Expose `blake3::Hasher` wrappers for streaming and batch hashing.
- **Capability doc:** `doc/capabilities/config-system/CAPABILITY.md`

#### `src/common/registry.zig` + `src/common/target.zig` → `crates/common/src/registry.rs`
- **Zig:** `TargetRegistry` + `TargetBuilder` (canonical Fluent Builder) with arena-backed error accumulation.
- **Rust:** Eliminate arena. `Target` is a plain struct with `#[derive(Debug, Clone)]`. `TargetRegistry` owns `Vec<Target>` and a `HashMap<ArcIntern<str>, usize>` for name→index. The builder is generated by `#[derive(bon::Builder)]` on a `TargetCreateArgs` struct. The registry's `register(target: Target)` consumes the value.
- **Builder error pattern:** Rust's `?` operator replaces Zig's `BuilderError` accumulation. If a setter can fail, it returns `Result<Self, RegistryError>`. The `bon` builder handles this naturally because each `#[builder(into)]` setter is infallible; validation moves to `build()`.
- **Capability doc:** `doc/capabilities/target-registry/CAPABILITY.md`

#### `src/common/builder_error.zig` → **REPLACED**
- **Zig:** Rich error type with phase, field, value, constraint.
- **Rust:** Use `thiserror` (transitive dep) or manual `#[derive(Error)]` enums. The "rich context" is provided by `tracing` spans and `eyre` (if desired). Since the builder pattern is now `bon`-generated, we do not need a hand-rolled builder error type. Instead, each registry exposes its own `RegistryError` with contextual variants.

#### `src/common/string.zig` → `crates/common/src/string.rs`
- **Zig:** `looksLikeIdentifier`, `containsIgnoreCase`, `truncateAtSentence`, `slugify`, etc.
- **Rust:** Port as pure functions over `&str` / `smol_str::SmolStr`. Use `smol_str` for all AST token storage. `truncate_at_sentence` is a deterministic string utility with no allocation semantics change.

#### `src/common/json.zig`, `io.zig`, `url.zig`, `args.zig` → `crates/common/src/{json,io,url,args}.rs`
- **Zig:** JSON stringify helpers, file read helpers, URL validation, CLI arg parsing.
- **Rust:** `serde_json` replaces custom JSON writers. `std::fs` + `tokio::fs` for I/O. `clap` derive for args. URL validation with `url` crate (transitive dep of `reqwest`/`async-openai`).

#### `src/reflection/` → **REPLACED by `serde` + `bon`**
- **Zig:** `Editable(T)`, `DynamicEditable`, `ConstraintVTable`, `Accessor`, `FieldMeta`, `BinaryFieldCodec`, `EnumRegistry`, `TypedAccessorTable`.
- **Rust:** This is the deepest paradigm shift. Zig's comptime reflection has **no direct Rust equivalent** without proc macros (which we avoid writing). Instead:
  - **Boundary serialization:** `serde::Serialize` / `serde::Deserialize` on every struct that crosses a JSON, SQLite, or WASM boundary.
  - **Schema description:** Derive `JsonSchema` from `schemars` (optional, but if we want AI-readable schemas, we can use it; otherwise, hand-write schema functions). Since `schemars` is not in the mandated list, we will hand-write `fn schema() -> serde_json::Value` for the few structs that need it (MCP tool parameters).
  - **Runtime dynamic access:** For WASM tool configs and SQLite row hydration where field names are only known at runtime, use `serde_json::Map<String, serde_json::Value>` or `HashMap<ArcIntern<str>, serde_json::Value>`. Do **not** try to recreate `DynamicEditable` with offset-based field access. It is unnecessary in Rust; `HashMap` lookups are fast enough for boundary code.
  - **Binary codec:** Replaced by `#[repr(C, packed)]` structs + byte-slice mapping (see §Binary IPC).
- **Capability docs:** `doc/capabilities/reflection/CAPABILITY.md` (document the serde-based replacement)

#### `src/common/content_node.zig`, `types.zig` → `crates/common/src/content_node.rs`
- **Zig:** `ContentNode` with LOD pyramid and `SharedString` (ref-counted from external `zigsharedstring` package).
- **Rust:** Replace `SharedString` with `ArcIntern<str>` or `Arc<str>`. `ContentNode` owns an `Arc<str>` for source text and `Vec<Arc<str>>` for LOD levels 1–5. The LOD pyramid is deterministic truncation; port the logic directly.

#### `src/common/doc_registry.zig`, `word_index.zig`, `trigram_index.zig` → `crates/common/src/index.rs`
- **Zig:** Inverted word index, trigram index with mmap support.
- **Rust:** Port as-is using `HashMap` / `BTreeMap` and `memmap2` (if needed for mmap). These are mechanical ports with no pattern changes.

---

## 3. Phase 2: `rust-src/guidance/` — The AST Orchestrator

**Goal:** Rebuild the deterministic-first code navigation engine.

### 3.1 `src/guidance/types.zig` → `crates/guidance/src/types.rs`
- **Zig:** `GuidanceDoc`, `Member`, `Meta`, `CapabilityEval`, `FileType`, `MemberType`, `Param`, `Stage`, `QueryResult`.
- **Rust:** Plain structs with `#[derive(Serialize, Deserialize, Debug, Clone)]`. Use `smol_str::SmolStr` for all small string fields (`name`, `signature`, `type`, `default`) to reduce heap pressure. Use `Vec<Member>` instead of slices. `FileType` becomes a Rust enum with `#[serde(rename_all = "snake_case")]`.
- **Memory model:** `GuidanceDoc` owns its `Vec<Member>` and `Vec<Skill>`. No arena; drop the doc and all members go away. This is cleaner than Zig's arena-scoped ownership.

### 3.2 `src/guidance/ast_parser.zig` → `crates/guidance/src/ast_parser.rs`
- **Zig:** Uses `std.zig.Ast` (built-in Zig parser).
- **Rust:** Uses `tree-sitter` with `tree-sitter-zig` and `tree-sitter-python` grammars. Extract function declarations, struct definitions, parameters, doc comments, visibility, and line numbers.
- **Ergonomics:** The AST walker returns `Vec<Member>` directly. No vtables; just an iterator over nodes.
- **Capability doc:** `doc/capabilities/ast-indexing/CAPABILITY.md`

### 3.3 `src/guidance/sync/*.zig` + `sync_engine.zig` → `crates/guidance/src/sync/`
- **Zig:** Incremental JSON sync based on `blake3` file hashing and `match_hash` (SHA-256 of signature).
- **Rust:** 
  - `blake3` for content hashing.
  - `serde_json` for reading/writing `.guidance/src/**/*.json`.
  - Staleness detection (JSON absent, JSON newer than source, JSON older by >1s) is a pure function over `std::fs::metadata` mtime.
  - `notify` for filesystem watching (optional; can be added later for live-reload).
- **Capability doc:** `doc/capabilities/sync-pipeline/CAPABILITY.md`

### 3.4 `src/guidance/query_engine.zig` + `query/*.zig` → `crates/guidance/src/query/`
- **Zig:** `explain` pipeline: identifier query → strategy selection → LLM filter → synthesis.
- **Rust:** 
  - Deterministic path: direct string matching on `ArcIntern<str>` keywords and trigram index. Sub-100ms guarantee.
  - LLM fallback: `async-openai` for synthesis when query contains spaces. Async Rust (`tokio`) is natural here.
  - `QueryStrategy` becomes an enum with associated data, not a vtable. The `matches` predicate is a method on the enum variant.
- **Capability doc:** `doc/capabilities/explain-query/CAPABILITY.md`

### 3.5 `src/vector/vector_db.zig` + `math.zig` + `quantized_embedding.zig` → `crates/guidance/src/vector/`
- **Zig:** SQLite BLOB storage for embeddings, in-process cosine similarity, hybrid search (RRF), `QuantizedEmbedding` (int8).
- **Rust:** 
  - `rusqlite` for database connection and BLOB I/O.
  - `sqlite-vec` for vector similarity index (replaces manual cosine scan where appropriate; fallback to manual Rust cosine for small datasets or quantized vectors).
  - `QuantizedEmbedding` is a simple `Vec<i8>` + `f32` scale factor. Port the int8 dot-product in pure Rust.
  - `SemanticAliases`: load from JSON, expand tokens before search.
- **Capability doc:** `doc/capabilities/vector-search/CAPABILITY.md`

### 3.6 `src/guidance/provider_discovery.zig` + `plugin*.zig` → `crates/guidance/src/plugin.rs`
- **Zig:** Discovers `guidance-<ext>` binaries on PATH, invokes them, parses JSON stdout.
- **Rust:** Use `tokio::process::Command` for async invocation. Plugin registry is a `HashMap<String, PathBuf>` of discovered executables. JSON protocol is identical.
- **Capability doc:** `doc/capabilities/plugin-system/CAPABILITY.md`

---

## 4. Phase 3: `rust-src/coral/` — The Edge AI Orchestrator

**Goal:** Rebuild the neurosymbolic knowledge base and cache hierarchy.

### 4.1 `src/coral/db.zig` + `schema.zig` → `crates/coral/src/db.rs`
- **Zig:** Raw `sqlite3.h` C bindings. `ContextNode` with LOD pyramid, `Library` (schema init, insert, query, KNN).
- **Rust:** 
  - `rusqlite` with `params![]` macro.
  - `sqlite-vec` loaded as an extension via `libsqlite3-sys` (or `rusqlite` extension loading).
  - `ContextNode` uses `Arc<str>` for text. `Library` owns the `rusqlite::Connection` (with internal `Mutex` if shared across threads).
  - `KnnHit` and `EdgeType` are plain data structs.
- **Capability docs:** `doc/capabilities/coral-database/CAPABILITY.md`, `doc/capabilities/coral-ingestion/CAPABILITY.md`

### 4.2 `src/coral/cache*.zig` → `crates/coral/src/cache.rs`
- **Zig:** 5-tier cache: L1 memory → L2 WASM → L3 graph → L4 KNN → L5 LLM. `QueueReactorBuilder` (arena-backed fluent builder) + `QueueReactor`.
- **Rust:** 
  - `L1Cache`: `dashmap::DashMap` or `std::collections::HashMap` inside a `RwLock` for query→result caching.
  - `QueueReactor` owns `Arc<Library>` and `Option<Arc<dyn EmbeddingProvider>>`. The builder is `#[derive(bon::Builder)]` on `QueueReactorCreateArgs`. `build()` consumes the builder and returns `QueueReactor`.
  - Routing logic is a straight `match` on tier predicates, not vtable dispatch.
  - `findWasmTool` matches by `bitvec::BitVec` coverage (see DAG section).
- **Capability docs:** `doc/capabilities/coral-cache/CAPABILITY.md`, `doc/capabilities/local-model-decomposition/CAPABILITY.md`

### 4.3 `src/coral/batch.zig` + `yago_ingest.zig` → `crates/coral/src/ingest.rs`
- **Zig:** `BatchIngestor` with per-batch arena reset.
- **Rust:** No arenas. A batch is a function scope. Intermediate allocations are owned by local `Vec`s and `String`s. At scope exit, Rust drops them automatically. For triple→node mapping, stream RDF triples into `ContextNode` structs and flush to SQLite in transactions.
- **Capability doc:** `doc/capabilities/coral-ingestion/CAPABILITY.md`

### 4.4 `src/coral/mcp.zig` → `crates/coral/src/mcp.rs`
- **Zig:** JSON-RPC 2.0 MCP server over STDIO. `handleRequest` uses per-request arena.
- **Rust:** `tokio::io` for STDIO async reading. Each request spawns a `tokio::task` with its own scope; all intermediate data is dropped when the async block ends. Response serialization via `serde_json`.
- **Capability doc:** `doc/capabilities/coral-mcp/CAPABILITY.md`

### 4.5 `src/coral/context_node_schema.zig` + `src/wasm/wasm.zig` → `crates/wasm_ipc/src/lib.rs`
- **Zig:** `extern struct align(1)` for zero-copy binary IPC across Extism boundary.
- **Rust:** `#[repr(C, packed)]` structs (`BinaryHeader`, `BinaryExecutionRequest`, `BinaryExecutionResult`, `BinaryContextNode`). Manual byte-slice mapping via `std::slice::from_raw_parts` or safe `bytemuck`-style parsing (without adding `bytemuck` to deps; we write 50 lines of manual LE integer read/write).
- **Extism integration:** Use the `extism` crate (high-level Rust API) instead of raw C bindings. The binary payload is assembled into a `Vec<u8>` and passed to `Plugin::call::<&[u8], Vec<u8>>("execute", payload)`.
- **Capability docs:** `doc/capabilities/wasm-tools/CAPABILITY.md`, `doc/capabilities/coral-cache/CAPABILITY.md`

---

## 5. Phase 4: `rust-src/dag/` — Custom Topological Engine

**Goal:** Replace Zig's `DependencyResolver` and `DagExecutor` with a custom Rust implementation.

### 5.1 Why No `petgraph`

The prompt explicitly forbids `petgraph`. The DAG in this codebase is not a general graph; it is a **capability-matched dependency graph** with the following properties:
- Nodes are `Target`s with `depends` and `provides` bitsets.
- Edges are implied by bitset overlap.
- We need topological sort, cycle detection, and plan generation.
- The graph is small (hundreds to low-thousands of nodes).

### 5.2 Implementation Plan

- `dag/src/target.rs`: `Target` struct with `bitvec::BitVec` for `depends` and `provides`.
- `dag/src/registry.rs`: `TargetRegistry` with `Vec<Target>` and `HashMap<ArcIntern<str>, usize>`.
- `dag/src/resolver.rs`: 
  - `DependencyResolver` with `resolve(names: &[&str]) -> Result<ExecutionPlan, ResolverError>`.
  - Kahn's algorithm for topological sort using `HashMap<usize, Vec<usize>>` adjacency and `HashMap<usize, usize>` in-degree.
  - Cycle detection: if sorted length != visited set length, return `ResolverError::CircularDependency`.
- `dag/src/executor.rs`: `DagExecutor` that consumes `ExecutionPlan` and runs targets in topological waves. Native targets run inline; WASM targets call `extism`.
- **Capability docs:** `doc/capabilities/target-registry/CAPABILITY.md`

---

## 6. Phase 5: `rust-src/llm/` — Frontier Model Fallback

**Goal:** Minimal LLM client for enhancement and L5 cache tier.

### 6.1 `src/llm/llm.zig` + `context_packer.zig` + `anonymize.zig` → `crates/llm/src/`
- **Zig:** Manual HTTP client for Ollama/OpenAI chat completions. Token budget estimation. Context packing.
- **Rust:** 
  - Use `async-openai` for OpenAI-compatible endpoints (including custom base URLs).
  - For Ollama chat endpoints that diverge from OpenAI spec, use `reqwest` directly (it's a transitive dep).
  - `context_packer`: deterministic truncation based on token budgets (use `tiktoken-rs` if needed; otherwise, estimate with byte length / 4 heuristic, as Zig did).
  - `anonymize`: regex-based PII stripping.
- **Capability docs:** `doc/capabilities/llm-client/CAPABILITY.md`, `doc/capabilities/local-model-decomposition/CAPABILITY.md`

---

## 7. TDD Strategy

### 7.1 Testing Pyramid

| Level | What | When |
|-------|------|------|
| **Unit** | Every public function in `common/` (interner, hash, string utils, math). | During module development. |
| **Integration** | SQLite round-trip (insert node → query node → assert LOD equality). | After `coral::db` is ready. |
| **Integration** | `tree-sitter` parse → `GuidanceDoc` JSON round-trip. | After `guidance::ast_parser` is ready. |
| **Integration** | DAG resolution: build graph → resolve → assert topo order. | After `dag::resolver` is ready. |
| **Smoke** | `cargo run --bin guidance -- --help` and `guidance explain "test"`. | Before every PR. |
| **End-to-End** | Full RALPH loop on a fixture repo. | Phase 5. |

### 7.2 Fixture Repositories

Create `rust-src/fixtures/sample-project/` with:
- Zig source files (for AST parsing parity tests).
- Python source files (for plugin tests).
- A pre-generated `.guidance/` directory (for sync staleness tests).

### 7.3 Parity Gates

Before declaring a module "done", an automated script must:
1. Run the Zig test suite for that module.
2. Run the Rust test suite for the ported module.
3. Compare coverage %; Rust must be ≥ Zig.
4. Compare benchmark timings; Rust must be ≤ 1.2× Zig for I/O-bound work, ≤ 1.5× for CPU-bound work (acceptable given safety gains).

---

## 8. Documentation Requirements

### 8.1 Capability Documents

Every ported capability **must** have a `rust-src/doc/capabilities/<name>/CAPABILITY.md` that:
- References the original Zig `doc/capabilities/<name>/CAPABILITY.md`.
- Lists the Rust crates/files that implement it.
- Notes any semantic deviations (e.g., "No `BuilderError`; Rust uses `Result` and `tracing` spans").
- Contains a verified code example.

### 8.2 Module READMEs

Each crate gets a `README.md` with:
- One-sentence purpose.
- Public API surface (module-level docs generated by `cargo doc`).
- How to run tests.

---

## 9. Progression Summary

| Phase | Focus | Feature Parity Target |
|-------|-------|----------------------|
| 0 | Bootstrap | Workspace builds, CI green. |
| 1 | `common/` | Interner, embeddings, registry, hash, builders, reflection→serde. |
| 2 | `guidance/` | AST parse, JSON sync, query engine, vector search, plugins. |
| 3 | `coral/` | SQLite DB, cache hierarchy, ingestion, MCP, WASM IPC. |
| 4 | `dag/` | Custom topo sort, executor, bitset capability matching. |
| 5 | `llm/` + CLI | Frontier fallback, `clap` CLI, end-to-end RALPH loop. |

---

## 10. Risk Register

| Risk | Mitigation |
|------|-----------|
| `tree-sitter` Zig grammar is incomplete vs `std.zig.Ast`. | Maintain a fallback parser for edge-case Zig syntax; test on monorepo itself. |
| `sqlite-vec` extension not available on all target platforms. | Fallback to manual cosine similarity in Rust (already planned for quantized path). |
| `extism` Rust crate API differs from Zig C-API. | Isolate Extism calls behind a `WasmRuntime` trait; swap implementations if needed. |
| No arena → more granular allocations → fragmentation. | Use `ArcIntern<str>` and `smol_str` to minimize allocations; profile with `dhat`. |
| `bon` builder limitations vs hand-rolled Zig builders. | If `bon` cannot express a pattern, fall back to manual `impl` builder methods (still no `derive_builder`). |

---

*End of Roadmap*
