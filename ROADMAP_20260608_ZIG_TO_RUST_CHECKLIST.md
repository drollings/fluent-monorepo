# ROADMAP CHECKLIST: Zig Monorepo → Rust Rewrite

**Date:** 2026-06-08
**Usage:** Follow-up coding agents tick boxes as they complete items.

---

## Legend

- `[ ]` — Not started
- `[-]` — In progress
- `[x]` — Completed & verified
- `[~]` — Blocked / Deferred

---

## Phase 0: Bootstrap & Infrastructure

### Workspace & Tooling
- [x] Create `rust-src/Cargo.toml` workspace root with members: `common`, `guidance`, `coral`, `dag`, `llm`, `wasm_ipc`, `bin/guidance`.
- [x] Add mandated crates to workspace `Cargo.toml` (exact versions pinned in `Cargo.lock`):
  - `clap` (derive), `notify`, `blake3`
  - `smol_str`, `internment`
  - `serde`, `serde_json`
  - `bon`
  - `rusqlite`, `sqlite-vec`
  - `bitvec`, `extism`, `async-openai`
  - `tree-sitter`, `tree-sitter-zig`, `tree-sitter-python`
- [x] Create `rust-src/.cargo/config.toml` with `target-dir = "target"` to avoid conflicts with Zig build artifacts.
- [x] Create `.github/workflows/rust.yml` (or update existing CI) with:
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - `cargo doc --workspace --no-deps`
- [x] Create `rust-src/fixtures/sample-project/` with Zig, Python, and Markdown files for parity testing.
- [x] Create `rust-src/doc/capabilities/` directory skeleton mirroring `doc/capabilities/`.
- [x] Smoke test: `cargo build --workspace` passes with zero source files (empty lib crates).

### Crate Skeletons
- [x] `crates/common/Cargo.toml` + `src/lib.rs`
- [x] `crates/guidance/Cargo.toml` + `src/lib.rs`
- [x] `crates/coral/Cargo.toml` + `src/lib.rs`
- [x] `crates/dag/Cargo.toml` + `src/lib.rs`
- [x] `crates/llm/Cargo.toml` + `src/lib.rs`
- [x] `crates/wasm_ipc/Cargo.toml` + `src/lib.rs`
- [x] `bin/guidance/Cargo.toml` + `src/main.rs`

---

## Phase 1: `crates/common/` — The Foundation

### Core Data Types
- [x] `src/types.rs` — `FileType`, `MemberType`, `EdgeType`, `StageKind` enums.
  - **Verify against:** `doc/capabilities/ast-indexing/CAPABILITY.md`, `doc/capabilities/target-registry/CAPABILITY.md`
- [x] `src/types.rs` — `Param`, `Member`, `Skill`, `Meta`, `CapabilityEval`, `GuidanceDoc` structs.
  - Use `smol_str::SmolStr` for `name`, `signature`, `type`, `default`.
  - `#[derive(Serialize, Deserialize, Debug, Clone)]` on all.
  - **Verify against:** `doc/capabilities/ast-indexing/CAPABILITY.md`
- [x] `src/types.rs` — `ContextNode` struct with LOD pyramid (`Arc<str>` or `ArcIntern<str>`).
  - **Verify against:** `doc/capabilities/coral-database/CAPABILITY.md`
- [x] `src/types.rs` — `KnnHit`, `GraphNode` structs.

### String Interning & Capability Registry
- [x] `src/interner.rs` — `CapabilityRegistry` wrapping `RwLock<HashMap<ArcIntern<str>, usize>>`.
  - `intern(name: &str) -> usize`
  - `get_index(name: &str) -> Option<usize>`
  - `get_name(idx: usize) -> Option<ArcIntern<str>>`
  - `intern_list(names: &[&str])`
  - `to_bitvec(names: &[&str]) -> bitvec::BitVec`
  - `bitvec_to_names(bits: &bitvec::BitVec) -> Vec<ArcIntern<str>>`
  - **Thread-safety test:** concurrent intern from 8 threads, all same index.
  - **Verify against:** `doc/capabilities/target-registry/CAPABILITY.md`

### Embedding Providers
- [x] `src/embeddings.rs` — `trait EmbeddingProvider: Send + Sync`
  - `fn name(&self) -> &str;`
  - `fn dimensions(&self) -> u32;`
  - `fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError>;`
  - `fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbedError>;`
  - **Verify against:** `doc/capabilities/embedding-providers/CAPABILITY.md`
- [x] `src/embeddings.rs` — `NoopEmbedding` impl.
- [x] `src/embeddings.rs` — `OllamaEmbedding` impl (HTTP via `ureq`).
- [x] `src/embeddings.rs` — `OpenAiEmbedding` impl.
- [x] `src/embeddings.rs` — `create_embedding_provider()` factory.
- [x] `src/embeddings.rs` — JSON response parsers (`parse_ollama_response`, `parse_openai_response`).
- [x] **Test:** `embed_batch` round-trip with mock HTTP server (`httpmock`).
- [x] **Test:** Thread-safe concurrent `embed()` calls.

### Registry & Builders
- [x] `src/registry.rs` — `Target` struct.
  - Fields: `id: i64`, `name: ArcIntern<str>`, `target_type: TargetType`, `executor: ExecutorKind`, `depends: bitvec::BitVec`, `provides: bitvec::BitVec`, `command: String`, `essential: bool`.
  - `#[derive(Debug, Clone, bon::Builder)]` on `Target`.
  - **Verify against:** `doc/capabilities/target-registry/CAPABILITY.md`
- [x] `src/registry.rs` — `TargetRegistry`.
  - `register(target: Target) -> Result<(), RegistryError>`
  - `get(name: &str) -> Option<&Target>`
  - `get_by_bit_index(idx: usize) -> Option<&Target>`
  - `get_providers(bit_index: usize) -> Vec<&Target>`
  - `list_names() -> Vec<ArcIntern<str>>`
  - `essential_targets() -> Vec<&Target>`
  - `abstract_targets() -> Vec<&Target>`
  - **Test:** registration + retrieval + provider map consistency.
  - **Verify against:** `doc/capabilities/target-registry/CAPABILITY.md`

### Hash & I/O
- [x] `src/hash.rs` — `blake3_hash(data: &[u8]) -> [u8; 32]`
- [x] `src/hash.rs` — `blake3_hex(data: &[u8]) -> String`
- [x] `src/hash.rs` — `content_hash_with_model(content: &str, model: &str) -> String` (SHA-256).
- [x] `src/hash.rs` — `hash_file(path: &Path) -> Result<String, IoError>`
- [x] `src/io.rs` — `read_file_alloc(path: &str) -> Option<String>`
- [x] `src/io.rs` — `resolve_path(base: &str, relative: &str) -> String`
- [x] `src/io.rs` — `strip_path_prefix(path: &str, prefix: &str) -> &str`

### String Utilities
- [x] `src/string.rs` — `looks_like_identifier(s: &str) -> bool`
- [x] `src/string.rs` — `contains_ignore_case(haystack: &str, needle: &str) -> bool`
- [x] `src/string.rs` — `truncate_at_sentence(text: &str, max_chars: usize) -> String`
- [x] `src/string.rs` — `slugify(s: &str) -> String`
- [x] `src/string.rs` — `strip_boilerplate(text: &str) -> String`
- [x] `src/string.rs` — `is_noisy_comment(text: &str) -> bool`
- [x] `src/string.rs` — `lower_into(s: &str, buf: &mut String)`
- [x] **Test:** all string utilities against Zig fixture outputs.

### Content Node & LOD
- [x] `src/content_node.rs` — `ContentNode` with `ArcIntern<str>` source and LOD pyramid.
- [x] `src/content_node.rs` — `generate_lod_slices(full_text: &str) -> Vec<String>`
- [x] **Test:** LOD generation produces expected truncation boundaries.

### Index Primitives
- [x] `src/word_index.rs` — `DocRegistry` (path ↔ u32 mapping, embedded in word_index.rs).
- [x] `src/word_index.rs` — `WordIndex` inverted index.
- [x] `src/trigram_index.rs` — `TrigramIndex` with mmap support (`memmap2`).
- [x] `src/freq_table.rs` — `FrequencyTable` for adaptive tokenization.
- [x] **Test:** round-trip insert/query for all indexes.

### Error Types
- [x] `src/error.rs` — `RegistryError`, `EmbedError`, `IoError`, `ResolverError`, `DbError`, `CacheError` enums with `thiserror`.

---

## Phase 2: `crates/guidance/` — The AST Orchestrator

### AST Parsing
- [x] `src/ast_parser.rs` — `AstParser` struct wrapping `tree_sitter::Parser`.
- [x] `src/ast_parser.rs` — `parse_file(path: &Path, source: &str) -> Result<GuidanceDoc, ParseError>`.
- [x] `src/ast_parser.rs` — Extract `fn_decl`, `struct`, `enum`, `const`, `type` with signatures.
- [x] `src/ast_parser.rs` — Extract doc comments (`///` and `//!`).
- [x] `src/ast_parser.rs` — Extract visibility (`pub` / `pub(crate)`).
- [x] `src/ast_parser.rs` — Extract line numbers.
- [x] `src/ast_parser.rs` — Python support via `tree-sitter-python`.
- [x] **Test:** Parse fixtures and assert member count ≥ 1.
- [x] **Test:** Parse `fixtures/sample-project/*.py` and assert class/function extraction.
- [x] **Verify against:** `doc/capabilities/ast-indexing/CAPABILITY.md`

### Sync Pipeline
- [x] `src/sync/json_store.rs` — `load_guidance(path: &Path) -> Result<GuidanceDoc, JsonError>`.
- [x] `src/sync/json_store.rs` — `save_guidance(path: &Path, doc: &GuidanceDoc) -> Result<(), JsonError>`.
- [x] `src/sync/json_writer.rs` — Deterministic JSON output (sorted keys, no trailing comma).
- [x] `src/sync/staleness.rs` — `is_stale(json_path: &Path, source_path: &Path) -> bool` (mtime logic).
- [x] `src/sync/staleness.rs` — `match_hash` computation (blake3 of signature).
- [x] `src/sync/comments.rs` — `sync_comments(source_path: &Path, doc: &GuidanceDoc) -> Result<(), SyncError>`.
- [x] `src/sync_engine.rs` — `SyncEngine` orchestrating the full pipeline.
- [x] **Test:** staleness detection matrix (absent, newer, older >1s, older = src-1s).
- [x] **Test:** incremental sync preserves unchanged members.
- [x] **Verify against:** `doc/capabilities/sync-pipeline/CAPABILITY.md`

### Query Engine
- [x] `src/query/identifier.rs` — `IdentifierQuery` matching keywords, trigrams, signatures.
- [x] `src/query/strategy.rs` — `QueryStrategy` enum (Identifier, Capability, Concept).
- [x] `src/query/strategy.rs` — `matches(query: &str, db: &GuidanceDb) -> bool` per variant.
- [x] `src/query/llm_filter.rs` — `LlmFilter` using `async-openai` for relevance scoring.
- [x] `src/query/llm_filter_batch.rs` — Batch scoring for ≤20 candidates.
- [x] `src/query/synthesize.rs` — `Synthesizer` combining results into `Vec<Stage>`.
- [x] `src/query_engine.rs` — `QueryEngine` with deterministic fast path (<100ms) and LLM fallback.
- [x] **Test:** deterministic keyword query returns results on test doc.
- [x] **Test:** LLM synthesis mock (return canned response) verifies pipeline flow.
- [x] **Verify against:** `doc/capabilities/explain-query/CAPABILITY.md`

### Vector Search
- [x] `src/vector/vector_db.rs` — `GuidanceDb` wrapping `rusqlite::Connection`.
- [x] `src/vector/vector_db.rs` — `vector_search(query_vec: &[f32], k: usize) -> Result<Vec<SearchResult>, DbError>`.
- [x] `src/vector/vector_db.rs` — `keyword_search(query: &str) -> Result<Vec<SearchResult>, DbError>`.
- [x] `src/vector/vector_db.rs` — `hybrid_search(query: &str, k: usize) -> Result<Vec<SearchResult>, DbError>` (RRF fusion).
- [x] `src/vector/math.rs` — `cosine_similarity(a: &[f32], b: &[f32]) -> f32`.
- [x] `src/vector/math.rs` — `vec_to_bytes(v: &[f32]) -> Vec<u8>`, `bytes_to_vec(b: &[u8]) -> Vec<f32>`.
- [x] `src/vector/quantized_embedding.rs` — `QuantizedEmbedding` (int8) with 4× reduction.
- [x] `src/vector/quantized_embedding.rs` — `cosine_similarity_q8(a: &QuantizedEmbedding, b: &QuantizedEmbedding) -> f32`.
- [x] `src/vector/semantic_aliases.rs` — `SemanticAliases` loaded from JSON, token expansion.
- [x] **Test:** cosine similarity matches expected values within 1e-6.
- [x] **Test:** hybrid search RRF weights (0.65 vector + 0.35 keyword) produce expected ranking.
- [x] **Verify against:** `doc/capabilities/vector-search/CAPABILITY.md`

### Plugin System
- [x] `src/plugin.rs` — `PluginRegistry` (HashMap<ext, PathBuf>).
- [x] `src/plugin.rs` — `discover_providers() -> PluginRegistry` (scan `bin/` and PATH for `guidance-*`).
- [x] `src/plugin.rs` — `invoke_provider_file(plugin: &Path, file: &Path) -> Result<GuidanceDoc, PluginError>`.
- [x] **Test:** discover `guidance-*` fixture and invoke on file.
- [x] **Verify against:** `doc/capabilities/plugin-system/CAPABILITY.md`

---

## Phase 3: `crates/coral/` — The Edge AI Orchestrator

### Database Layer
- [x] `src/db.rs` — `Library` struct with `rusqlite::Connection`.
- [x] `src/db.rs` — `init_schema()` creates tables: `context_nodes`, `edges`, `wasm_tools`, `targets`, `embedding_cache`.
- [x] `src/db.rs` — `insert_node(node: &ContextNode) -> Result<NodeId, DbError>`.
- [x] `src/db.rs` — `find_node_by_name(name: &str) -> Result<Option<NodeId>, DbError>`.
- [x] `src/db.rs` — `knn_search(query_vec: &[f32], k: usize) -> Result<Vec<KnnHit>, DbError>`.
- [x] `src/db.rs` — `traverse_from(node_id: NodeId, max_depth: u8) -> Result<Vec<GraphNode>, DbError>` (recursive CTE).
- [x] `src/db.rs` — `insert_wasm_tool(tool: &WasmTool) -> Result<(), DbError>`.
- [x] `src/db.rs` — BLOB read/write for embeddings and bitsets.
- [x] **Test:** SQLite round-trip for `ContextNode` (all LOD levels preserved).
- [x] **Test:** KNN against synthetic nodes returns correct top-N.
- [x] **Test:** Graph traversal from a root node reaches expected depth.
- [x] **Verify against:** `doc/capabilities/coral-database/CAPABILITY.md`

### Cache Hierarchy
- [x] `src/cache_l1.rs` — `L1Cache` (`DashMap<String, RoutingResult>`).
- [x] `src/cache_reactor.rs` — `QueueReactor` struct.
- [x] `src/cache_reactor.rs` — `#[derive(bon::Builder)]` on `QueueReactorCreateArgs`.
- [x] `src/cache_reactor.rs` — `find_wasm_tool(query: &str) -> Option<WasmTool>` (bitset coverage check).
- [x] `src/cache_router.rs` — `ParallelRouter` for concurrent tier evaluation.
- [x] **Test:** L1 hit returns cached result.
- [x] **Test:** L1 miss returns error when no other tiers match.
- [x] **Test:** QueueReactor builds with library.
- [x] **Verify against:** `doc/capabilities/coral-cache/CAPABILITY.md`

### Ingestion
- [x] `src/ingest.rs` — `BatchIngestor` (no arena; uses scoped `Vec` allocations).
- [x] `src/ingest.rs` — `TripleMapper` (RDF triple → `ContextNode` + edges).
- [x] `src/ingest.rs` — `flush()` writes batch to SQLite in a transaction.
- [x] **Test:** ingest and verify node count.
- [x] **Verify against:** `doc/capabilities/coral-ingestion/CAPABILITY.md`

### MCP Server
- [x] `src/mcp.rs` — `McpServer` over STDIO (`tokio::io` ready).
- [x] `src/mcp.rs` — JSON-RPC 2.0 request parsing (`serde_json`).
- [x] `src/mcp.rs` — `coral_query`, `coral_insert`, `coral_traverse` methods.
- [x] `src/mcp.rs` — Response serialization.
- [x] **Test:** send JSON-RPC request, assert valid response.
- [x] **Verify against:** `doc/capabilities/coral-mcp/CAPABILITY.md`

---

## Phase 4: `crates/dag/` — Custom Topological Engine

### Target Model
- [x] `src/target.rs` — `Target` struct with `bitvec::BitVec` (re-exported from common).
- [x] `src/target.rs` — `TargetType` enum (`File`, `Phony`, `Abstract`).
- [x] `src/target.rs` — `ExecutorKind` enum (`Native`, `Docker`, `Wasm`).
- [x] **Test:** target creation and clone.

### Registry
- [x] `src/registry.rs` — `TargetRegistry` (Vec<Target> + HashMap name→index + providers map).
- [x] `src/registry.rs` — `register(target: Target)`.
- [x] `src/registry.rs` — `get(name)`, `get_by_bit_index(idx)`, `get_providers(idx)`, `find_providers()`.
- [x] **Test:** provider map consistency after multiple registrations.
- [x] **Verify against:** `doc/capabilities/target-registry/CAPABILITY.md`

### Resolver
- [x] `src/resolver.rs` — `DependencyResolver`.
- [x] `src/resolver.rs` — `resolve(target_names: &[&str]) -> Result<ExecutionPlan, ResolverError>`.
- [x] `src/resolver.rs` — Kahn's algorithm using `HashMap<usize, Vec<usize>>` adjacency.
- [x] `src/resolver.rs` — `resolve_abstract_dependencies(..., provided: &BitVec) -> Result<ExecutionPlan, ResolverError>`.
- [x] `src/resolver.rs` — Cycle detection → `ResolverError::CircularDependency`.
- [x] **Test:** linear chain resolves in order.
- [x] **Test:** diamond graph resolves correctly.
- [x] **Test:** missing dependency with `strict=true` returns error.
- [x] **Test:** circular dependency detected and error returned.
- [x] **Verify against:** `doc/capabilities/target-registry/CAPABILITY.md`

### Executor
- [x] `src/executor.rs` — `DagExecutor`.
- [x] `src/executor.rs` — `execute(plan: &ExecutionPlan) -> Result<Vec<ExecutionResult>, ExecutionError>`.
- [x] **Test:** execute a 3-node DAG and assert completion order.

---

## Phase 5: `crates/llm/` + CLI

### LLM Client
- [x] `src/client.rs` — `LlmClient` wrapping `async-openai`.
- [x] `src/client.rs` — `chat_complete(messages: &[ChatMessage]) -> Result<String, LlmError>`.
- [x] `src/client.rs` — Support for OpenAI and custom base URLs (Ollama compatibility).
- [x] `src/context_packer.rs` — `ContextPacker` truncating context to token budget.
- [x] `src/anonymize.rs` — `anonymize(text: &str) -> String` (PII regex stripping).
- [x] **Test:** message serialization and client creation.
- [x] **Verify against:** `doc/capabilities/llm-client/CAPABILITY.md`

### WASM IPC
- [x] `crates/wasm_ipc/src/lib.rs` — `#[repr(C, packed)]` structs:
  - `BinaryHeader` (magic: [u8;4], version: u32, payload_type: u32, payload_size: u32, checksum: u32)
  - `BinaryExecutionRequest` (header, target_id: i64, input_offset: u32, input_len: u32, flags: u32)
  - `BinaryExecutionResult` (header, success: u32, error_code: u32, output_offset: u32, output_len: u32, provides_words_offset: u32, provides_words_count: u32)
  - `BinaryContextNode` (header, id: i64, valid_from_ts: i64, valid_to_ts: i64, confidence: i32, provenance_id: i32, lod_offsets: [u32;6], lod_lengths: [u32;6])
- [x] `crates/wasm_ipc/src/lib.rs` — `encode_request(req: &BinaryExecutionRequest, input: &[u8]) -> Vec<u8>`.
- [x] `crates/wasm_ipc/src/lib.rs` — `decode_result(buf: &[u8]) -> Result<(BinaryExecutionResult, Vec<u8>), IpcError>`.
- [x] `crates/wasm_ipc/src/lib.rs` — `get_provides_bitset(result: &BinaryExecutionResult, payload: &[u8]) -> Result<bitvec::BitVec, IpcError>`.
- [x] **Test:** round-trip encode/decode produces identical field values.
- [x] **Test:** bitset reconstruction from trailing u64 words matches original.
- [x] **Verify against:** `doc/capabilities/wasm-tools/CAPABILITY.md`

### Extism Integration
- [x] `crates/coral/src/wasm_runtime.rs` — `WasmRuntime` trait (isolates Extism details).
- [x] `crates/coral/src/wasm_runtime.rs` — `ExtismWasmRuntime` impl using `extism` crate.
- [x] `crates/coral/src/wasm_runtime.rs` — `WasmPlugin` trait with `call()`.
- [x] **Test:** runtime creation and file-not-found error.
- [x] **Verify against:** `doc/capabilities/wasm-tools/CAPABILITY.md`

### CLI Dispatcher
- [x] `bin/guidance/src/main.rs` — `clap` derive for subcommands:
  - `explain`, `show`, `test`, `telemetry`, `cache-stats`, `serve`
  - `init`, `gen`, `status`, `clean`, `commit`, `check`, `todo`, `diary`
- [x] `bin/guidance/src/main.rs` — Dispatch to subcommands.
- [x] `bin/guidance/src/main.rs` — `--debug` and `--show-prompts` flags.
- [x] **Test:** CLI help renders without panic.
- [x] **Test:** `guidance --help` exits 0 (via integration test).

---

## Phase 6: Documentation & Parity

### Capability Docs
For **each** capability below, create `rust-src/doc/capabilities/<name>/CAPABILITY.md` and mark `[x]` only after it has been reviewed against the original Zig doc:
- [ ] `ast-indexing`
- [ ] `config-system`
- [ ] `coral-cache`
- [ ] `coral-database`
- [ ] `coral-ingestion`
- [ ] `coral-mcp`
- [ ] `embedding-providers`
- [ ] `explain-query`
- [ ] `llm-client`
- [ ] `local-model-decomposition`
- [ ] `ontology`
- [ ] `plugin-system`
- [ ] `rdf-parsing`
- [ ] `reflection` (describe serde replacement)
- [ ] `sync-pipeline`
- [ ] `target-registry`
- [ ] `vector-search`
- [ ] `wasm-tools`

### End-to-End Parity
- [ ] Run Zig `guidance gen` on fixture repo; capture JSON output.
- [ ] Run Rust `guidance gen` on same fixture; capture JSON output.
- [ ] `diff` the two JSON trees; structural differences must be ≤ 1% (whitespace / key ordering excluded).
- [ ] Run Zig `guidance explain "cmdExplain"`; capture output.
- [ ] Run Rust `guidance explain "cmdExplain"`; capture output.
- [ ] Semantic similarity of outputs must be ≥ 95% (manual review).
- [ ] Run full RALPH loop on Rust monorepo itself: `make pre-commit` passes.

---

*End of Checklist*
