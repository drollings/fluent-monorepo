# guidance — AST-guided Vector Search

`guidance explain "<query>"` is the first stop to gain relevant context about this codebase.

A single keyword like `cmdExplain` triggers a deterministic search without LLM synthesis. Queries with spaces use the LLM for synthesis.

**Example:**  
```
guidance explain "cmdExplain"
```

Look up suggested search terms from results to discover related features. Use regular file tools once you're confident about the implementation.

**Important:** Run `guidance explain` to check for existing features before writing duplicate code.

---

## Capabilities

- **ast-indexing**: Parses Zig and Python source files via AST to extract structured metadata (functions, structs, enums, types) into per-file JSON guidance documents under .guidance/src/.
- **config-system**: Two-level configuration loader for guidance projects. Reads guidance-config.json from the project .guidance/ directory, falls back to ~/.config/guidance/, then to built-in defaults.
- **coral-cache**: 5-tier cache hierarchy (L1 memory → L2 WASM → L3 graph → L4 KNN → L4.5 local decomposition → L5 LLM) for routing queries through the Coral knowledge base, implemented in QueueReactor with a fluent builder.
- **coral-database**: SQLite-backed knowledge graph storing ContextNodes with a 6-level LOD text pyramid, float embeddings as BLOBs, and graph edges. Supports KNN semantic search, recursive CTE graph traversal, duck-typing capability queries, and thread-safe concurrent access.
- **coral-ingestion**: Batch ingestion pipeline for RDF/YAGO 4.5 datasets into the Coral Library. BatchIngestor fluent API parses N-Quads/Turtle, maps triples to ContextNodes via TripleMapper, applies a YAGO_TYPE_WHITELIST to keep the graph under 5M nodes / 1 GB, and flushes to Library via insertNode/insertRdfEdge.
- **coral-mcp**: JSON-RPC 2.0 MCP server over STDIO for Coral. Exposes coral_query, coral_insert_node, and coral_explain tools to Claude Code, NullClaw, and Cursor. Each request gets an isolated arena; only the serialized response escapes to the caller.
- **embedding-providers**: Pluggable embedding provider system that converts text to dense float vectors for semantic search. Supports Ollama (local), OpenAI-compatible APIs, and a no-op keyword-only fallback.
- **explain-query**: Natural language codebase query engine that uses LanceDB hybrid vector+keyword search with LLM synthesis to answer questions about how code works, surfacing relevant source locations, skills, and capabilities.
- **llm-client**: Minimal LLM HTTP client (LlmClient) supporting OpenAI-compatible chat-completion endpoints and Ollama. Handles think-mode toggle for reasoning models, response post-processing (stripThinkBlock, isMalformedResponse), and malformed-response fallback. Used by guidance synthesis, local-model-decomposition, and coral L5 fallback.
- **local-model-decomposition**: L4.5 cache tier that calls a local LLM to decompose a complex query into up to 5 ordered sub-tasks, routes each sub-task through QueueReactor recursively (max depth enforced), merges and deduplicates results, and caches the solution so future identical queries hit L1 or L4.
- **ontology**: YAGO 4.5 ontology processing layer that maps RDF triples to Coral ContextNodes, applies rdfs/OWL inference rules, provides schema migration utilities, and enforces the YAGO_TYPE_WHITELIST to keep ingested graphs within size budgets.
- **plugin-system**: External AST provider protocol for non-Zig languages. Guidance discovers provider executables via PATH or config, invokes them per-file, and merges their JSON output into the guidance index.
- **rdf-parsing**: Zig RDF parsing module covering Turtle (lexer + recursive-descent parser), N-Quads streaming parser, and IRI normalization. Produces Triple/Term values used by the ontology mapper to ingest YAGO 4.5 and other RDF datasets.
- **reflection**: Standalone Zig reflection module providing vtable-driven field access, role-based permission enforcement, typed/binary codecs, and enum registry. Zero-cost mixin Editable(T) enables string-driven get/set on any struct at runtime without heap allocation per-field.
- **sync-pipeline**: Incremental source file synchronisation pipeline that runs test, lint, format, and AST guidance generation for each changed file, using mtime and match_hash for cheap change detection.
- **target-registry**: DAG-based target registry for guidance build pipelines. TargetRegistry stores named Target nodes with dependency and capability bitsets. TargetBuilder (in registry.zig) is the canonical Fluent Builder pattern in this codebase — chained *Self setters, deferred error at register(). StringInterner provides thread-safe string→bitset-index interning via RwLock.
- **vector-search**: Cosine similarity search over AST node embeddings stored in .guidance.db, enabling natural-language queries that find semantically related code even without exact keyword matches.
- **wasm-tools**: L2 Workflow Cache tier using Extism (libextism) for sandboxed WASM tool execution. Binary IPC schema (BinaryExecutionRequest/BinaryExecutionResult) is fully defined. WasmTool registration and findWasmTool() matching are implemented. Extism execution path is a TODO stub (P3.3).

---

Run `guidance explain "<keyword>"` to explore any capability.
