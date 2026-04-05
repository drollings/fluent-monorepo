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

- **ast-indexing**: ast-indexing: Parses Zig/Python files via AST to output JSON metadata (functions, structs, enums, types) in...
- **config-system**: Two-level config loader: reads guidance-config.json from .guidance/, then ~/.config/guidance/, then defaults.
- **coral-cache**: 5-tier cache hierarchy routes Coral KB queries, built in QueueReactor with a fluent builder.
- **coral-database**: SQLite KG with 6-level LOD, BLOB embeddings, KNN search, recursive CTE, duck-typing, thread-safe access.
- **coral-ingestion**: coral-ingestion: Batch RDF ingest into Coral Lib. Parses N-Quads/Turtle, maps ContextNodes, inserts nodes/edges.
- **coral-mcp**: JSON‑RPC 2.0 server over STDIO for Coral, with coral_query, coral_insert_node, coral_explain; isolated arenas, JSON...
- **embedding-providers**: Pluggable embedding provider: text-to-vector, supports Ollama, OpenAI APIs, fallback.
- **explain-query**: NL codebase query engine: SimHash+keyword search, LLM synthesis, returns source locations and skills.
- **llm-client**: Minimal LLM HTTP client for OpenAI/Ollama with think-mode toggle, response post-processing, and fallback.
- **local-model-decomposition**: Local cache tier that decomposes a query into 5 sub‑tasks, routes recursively, dedups, merges, caches for L1/L4 hits.
- **ontology**: YAGO 4.5 ontology: maps RDF to ContextNodes, runs rdfs/OWL inference, handles schema migration, limits size with...
- **plugin-system**: External AST provider for non-Zig; Guidance locates executables via PATH, calls per file, merges JSON into index.
- **rdf-parsing**: Zig module parses Turtle and N-Quads, normalizes IRIs, outputs triples for ontology mapping (YAGO 4.5).
- **reflection**: Zig reflection: vtable, role perms, codecs, enum registry; Editable(T) mixin for string get/set no per-field heap.
- **sync-pipeline**: Incrementally syncs source files, running tests, lint, format, and AST guidance on changes via mtime/hash.
- **target-registry**: Target-registry: DAG-based registry for named Targets with deps and capability bitsets; Builder pattern; string...
- **vector-search**: Cosine similarity vector search on AST embeddings in .guidance.db for natural-language code queries.
- **wasm-tools**: L2 cache tier using Extism for WASM tools, defined binary IPC, tool registration & matching implemented, exec stub P3.3.

---

Run `guidance explain "<keyword>"` to explore any capability.
