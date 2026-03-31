# AST-Guidance Project Structure

A fast, lightweight code navigation and orchestration framework friendly to
human and human-in-the-loop LLM agentic software engineering.  It is based
on enriched AST, and uses optional AI for documentation which is cached,
idempotent, and upcycled for lightweight searches and local agentic
intelligence.

## Quick Navigation (Coding Assistants)

| Purpose | File | Use When |
|---------|------|----------|
| **Find related code** | `make query QUERY="search terms"` | Searching for code |
| **Check Implementation** | `make explore QUERY="search terms"` | Before implementing anything |
| **Understand patterns** | `doc/capabilities/*.md` | Implementation examples + patterns |
| **Find existing code** | `mcp_grep` or `mcp_lsp_find_references` | Searching for implementations |

## **Attention**: Skills needed to understand files

Skills are referenced per-file in comments below.  The lookup path for the skills is: 
`{guidance_dir}/skills/{skill}/SKILL.md`

So if you find a file you're looking for named file.zig:
`file.zig      # [zig-current, gof-patterns] Summary of files' contents` , 
Then you you must read

```
{guidance_dir}/skills/zig-current/SKILL.md
{guidance_dir}/skills/gof-patterns/SKILL.md
```

---

## Directory Tree (Git-Tracked Files Only)

```
.
├── bin
│   ├── gen_simhash_projections.py
│   └── guidance-py
├── data
│   └── yago-4.5.0.2-tiny
│       └── yago-tiny.ttl
├── doc
│   ├── capabilities
│   │   ├── ast-indexing
│   │   │   └── CAPABILITY.md
│   │   ├── config-system
│   │   │   └── CAPABILITY.md
│   │   ├── coral-cache
│   │   │   └── CAPABILITY.md
│   │   ├── coral-database
│   │   │   └── CAPABILITY.md
│   │   ├── coral-ingestion
│   │   │   └── CAPABILITY.md
│   │   ├── coral-mcp
│   │   │   └── CAPABILITY.md
│   │   ├── embedding-providers
│   │   │   └── CAPABILITY.md
│   │   ├── explain-query
│   │   │   └── CAPABILITY.md
│   │   ├── llm-client
│   │   │   └── CAPABILITY.md
│   │   ├── local-model-decomposition
│   │   │   └── CAPABILITY.md
│   │   ├── ontology
│   │   │   └── CAPABILITY.md
│   │   ├── plugin-system
│   │   │   └── CAPABILITY.md
│   │   ├── rdf-parsing
│   │   │   └── CAPABILITY.md
│   │   ├── reflection
│   │   │   └── CAPABILITY.md
│   │   ├── sync-pipeline
│   │   │   └── CAPABILITY.md
│   │   ├── target-registry
│   │   │   └── CAPABILITY.md
│   │   ├── vector-search
│   │   │   └── CAPABILITY.md
│   │   └── wasm-tools
│   │       └── CAPABILITY.md
│   ├── coral
│   │   ├── CHANGELOG.md
│   │   ├── DETAILS.md
│   │   ├── OVERVIEW.md
│   │   └── VISION.md
│   ├── guidance
│   │   ├── schemas
│   │   │   └── guidance.schema.json
│   │   └── DESIGN.md
│   └── skills
│       ├── fluent-wvr
│       │   └── SKILL.md
│       ├── gof-patterns
│       │   └── SKILL.md
│       └── zig-current
│           └── SKILL.md
├── env
│   ├── mise
│   │   ├── mise.go.toml
│   │   ├── mise.php.toml
│   │   ├── mise.pine.toml
│   │   ├── mise.rust.toml
│   │   ├── mise.wasm.toml
│   │   └── mise.zig.toml
│   └── mk
│       ├── targets
│       │   ├── go.mk
│       │   ├── php.mk
│       │   ├── pine.mk
│       │   ├── py.mk
│       │   ├── rust.mk
│       │   └── zig.mk
│       ├── common.mk
│       └── target_language.mk
├── src
│   ├── common
│   │   ├── args.zig
│   │   ├── builder_error.zig                     # builder_error.zig — Structured error type for fluent builder chains.
│   │   ├── cli.zig
│   │   ├── context.zig
│   │   ├── dag_executor.zig                      # dag_executor.zig — M6.1 Parallel DAG Execution
│   │   ├── embeddings.zig                        # Embedding providers — convert text to vectors for semantic search.
│   │   ├── error_context.zig                     # error_context.zig — Structured error context for non-builder code paths.
│   │   ├── format.zig
│   │   ├── hash.zig                              # hash.zig — Generic cryptographic hashing utilities
│   │   ├── interner.zig                          # interner.zig — String interning with optional bitset support.
│   │   ├── io.zig                                # io.zig — Shared buffered I/O helpers
│   │   ├── json.zig                              # json.zig — Generic JSON serialization helpers
│   │   ├── json_parser.zig
│   │   ├── limits.zig                            # limits.zig — Shared resource-limit constants
│   │   ├── llm.zig                               # common — Shared utilities and LLM client for guidance, vector, and coral.
│   │   ├── local_model.zig                       # local_model.zig — Local LLM Task Decomposition (P6.1)
│   │   ├── log.zig
│   │   ├── logging.zig                           # logging.zig — Structured logging context and timing scope for Fluent WEAVER.
│   │   ├── refcount.zig                          # refcount.zig — Reference-counted VTable handle wrapper (M7).
│   │   ├── registry.zig
│   │   ├── repl.zig
│   │   ├── resolver.zig
│   │   ├── root.zig                              # common — Module umbrella root.
│   │   ├── shared_string.zig                     # SharedString — heap-allocated, reference-counted, immutable string.
│   │   ├── shell_parser.zig                      # shell_parser.zig — Safe command-string tokenizer
│   │   ├── source.zig                            # source.zig — Source code excerpt extraction helpers
│   │   ├── str.zig                               # str.zig — Generic string classification and inspection helpers
│   │   ├── string.zig
│   │   ├── target.zig
│   │   ├── terminal.zig
│   │   ├── types.zig                             # Represents a unique node identifier; managed via ownership model; ensures stable references.
│   │   ├── url.zig                               # url.zig — Generic URL validation helpers
│   │   └── wrapper.zig                           # wrapper.zig — Conditional and composable comptime wrappers (M9).
│   ├── concurrency
│   │   ├── any_work_unit.zig                     # any_work_unit.zig — Type-erased work unit and typed wrapper (M11).
│   │   ├── channel.zig                           # channel.zig — Bounded, mutex-backed MPMC channel (M13).
│   │   ├── context.zig                           # context.zig — Cancellation and deadline propagation (M11).
│   │   ├── error_group.zig                       # error_group.zig — Structured parallel dispatch with error capture (M14).
│   │   ├── root.zig                              # concurrency/root.zig — Public API re-exports for the concurrency layer.
│   │   └── spawn.zig                             # spawn.zig — Fire-and-forget dispatch over std.Thread.Pool (M12).
│   ├── coral
│   │   ├── anonymize.zig                         # anonymize.zig — PII anonymization for frontier LLM context minimization.
│   │   ├── batch.zig                             # batch.zig — Streaming Batch Ingestion Pipeline
│   │   ├── benchmark.zig                         # benchmark.zig — G5 Performance Benchmarks
│   │   ├── cache.zig                             # cache.zig — 5-Tier Cache Hierarchy for Query Routing
│   │   ├── cache_test.zig                        # cache_test.zig — Integration tests for L1-L5 routing pipeline
│   │   ├── cli.zig                               # cli.zig — Ingestion CLI Command Implementation
│   │   ├── config.zig                            # Coral project configuration loader.
│   │   ├── context_node_schema.zig
│   │   ├── db.zig                                # db.zig — Coral Context Database Layer (SQLite backend)
│   │   ├── executor.zig                          # executor.zig — DAG Executor for the YAGO ingestion pipeline.
│   │   ├── fixtures.zig                          # fixtures.zig — Test factory functions for coral integration tests
│   │   ├── frontier.zig                          # frontier.zig — M6: L5 Frontier Loop Context Minimization & Validation
│   │   ├── frontier_tool_compiler.zig            # frontier_tool_compiler.zig — Compiles LLM-generated source into WASM tools.
│   │   ├── http_transport.zig                    # http_transport.zig — M4.1/M4.2 HTTP Transport Layer with SSE
│   │   ├── http_transport_test.zig               # http_transport_test.zig — Unit tests for HTTP transport layer
│   │   ├── main.zig
│   │   ├── mcp.zig                               # mcp.zig — Coral MCP (Model Context Protocol) server.
│   │   ├── metrics.zig                           # metrics.zig — Coral Latency Histograms and Resolution Counters (M8.1)
│   │   ├── pattern.zig
│   │   ├── schema.zig                            # schema.zig — Coral Context SQLite Schema (DDL + Queries)
│   │   ├── scrub.zig                             # scrub.zig — Comment quality filter for ast-guidance infill pipeline.
│   │   ├── targets.zig                           # targets.zig — Ingestion DAG Target Definitions
│   │   ├── token_budget.zig                      # token_budget.zig — Token Estimation for Context Packing (M7.1)
│   │   ├── triage.zig                            # Triage subcommand: generate TRIAGE.md from a TODO.md work item.
│   │   ├── verify.zig                            # verify.zig — Ingestion Verification and Integrity Checking
│   │   └── yago_ingest.zig                       # yago_ingest.zig — YAGO 4.5 Baseline Ingestion (M3.2)
│   ├── guidance
│   │   ├── plugins
│   │   │   ├── markdown_plugin.zig             # MarkdownPlugin — extracts sections and metadata from Markdown files.
│   │   │   └── zig_plugin.zig                  # ZigPlugin — wraps ast_parser.zig as a LanguagePlugin.
│   │   ├── ast_parser.zig
│   │   ├── comment_cache.zig                     # comment_cache.zig — In-process cache for generated doc comments.
│   │   ├── comment_checker.zig                   # comment_checker.zig — Comment staleness detection for guidance.
│   │   ├── comment_inserter.zig                  # comment_inserter.zig — Insert and replace doc comments in Zig source files.
│   │   ├── comment_parser.zig                    # comment_parser.zig — Doc comment parsing and quality validation for guidance.
│   │   ├── comment_sync.zig                      # comment_sync.zig — Source-code-first comment sync workflow for guidance.
│   │   ├── config.zig                            # guidance project configuration loader.
│   │   ├── deps.zig
│   │   ├── enhancer.zig                          # AI Docstring Enhancer for Zig guidance generation.
│   │   ├── git.zig
│   │   ├── hash.zig
│   │   ├── header_generator.zig                  # header_generator.zig — File header comment generation for guidance.
│   │   ├── json_store.zig
│   │   ├── line_verify.zig                       # line_verify.zig — Declaration-level line number verification for guidance.
│   │   ├── llm_filter.zig                        # llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
│   │   ├── main.zig                              # guidance — AST-guided SQLite vector search database generator.
│   │   ├── marker.zig                            # Mtime-based change detection for guidance's incremental RALPH loop.
│   │   ├── pattern.zig
│   │   ├── plugin.zig                            # LanguagePlugin — interface for language-specific AST providers.
│   │   ├── plugin_registry.zig                   # PluginRegistry — maps file extensions to LanguagePlugin descriptors.
│   │   ├── provider_discovery.zig                # External language provider discovery for guidance.
│   │   ├── query_engine.zig                      # query_engine.zig — explain, staged, show, test, check commands.
│   │   ├── scrub.zig                             # scrub.zig — Synthetic comment detection and scrubbing.
│   │   ├── simhash.zig                           # simhash.zig — 64-bit SimHash for near-duplicate detection.
│   │   ├── staged.zig                            # staged.zig — Staged explain pipeline for `guidance explain`.
│   │   ├── structure.zig                         # STRUCTURE.md generator.
│   │   ├── sync.zig
│   │   ├── sync_engine.zig                       # sync_engine.zig — init, commit, gen, status, clean, pipeline, and utility commands.
│   │   ├── synthesize.zig                        # synthesize.zig — LLM-based synthesis for the staged explain pipeline.
│   │   ├── tests.zig                             # Unit tests for src/guidance — json_store merge logic, sync, config, and commit helpers.
│   │   ├── todo.zig                              # todo.zig — Work item lifecycle tracking for guidance.
│   │   ├── triage.zig                            # Triage subcommand: generate TRIAGE.md from a TODO.md work item.
│   │   ├── types.zig
│   │   └── vector_db.zig                         # vector_db.zig — Hybrid keyword + vector search for guidance generation.
│   ├── llm
│   │   └── root.zig                              # llm — General-purpose LLM inference client.
│   ├── ontology
│   │   ├── inference.zig                         # inference.zig — Ontology Inference Engine (R5)
│   │   ├── mapper.zig                            # mapper.zig — Triple → ContextNode Mapper
│   │   ├── migration.zig                         # migration.zig — Ontology Versioning and Migration
│   │   ├── root.zig                              # ontology/root.zig — Ontology processing module umbrella
│   │   └── yago.zig                              # yago.zig — YAGO 4.5 Ontology Schema Definition
│   ├── rdf
│   │   ├── lexer.zig                             # lexer.zig — Streaming Turtle (Terse RDF Triple Language) Lexer
│   │   ├── normalize.zig                         # normalize.zig — RDF Term Normalization
│   │   ├── nquads.zig                            # nquads.zig — N-Quads / N-Triples Parser (line-based, no prefix expansion)
│   │   ├── parser.zig                            # parser.zig — Streaming Recursive-Descent Turtle Parser
│   │   └── root.zig                              # rdf/root.zig — RDF parsing module umbrella
│   ├── reflection
│   │   ├── accessor.zig                          # accessor.zig — Accessor, DynamicEditable, Editable(T), FieldMeta, TypeTag, OwnershipMode.
│   │   ├── binary.zig                            # binary.zig — BinaryFieldCodec for wire-format encoding/decoding of struct fields.
│   │   ├── constraint.zig                        # constraint.zig — ConstraintVTable, constraintSet, constraintGet, Constraint(T).
│   │   ├── enum_registry.zig                     # enum_registry.zig — EnumRegistry for runtime enum name/value lookups.
│   │   ├── permissions.zig                       # permissions.zig — Role-based permission system for Coral Context reflection.
│   │   ├── root.zig                              # reflection — Coral Context field-level reflection, validation, and permission layer.
│   │   ├── schema_version.zig                    # schema_version.zig — Versioning primitives for the reflection schema.
│   │   ├── sql.zig                               # sql.zig — Schema-driven SQLite binding and hydration.
│   │   ├── typed.zig                             # typed.zig — TypedAccessorTable(T) and TypedEditable.
│   │   └── validate.zig                          # validate.zig — Runtime validation pipeline for FieldMeta constraints (M6).
│   ├── testing
│   │   └── mock_vtable.zig                       # mock_vtable.zig — Mock implementations of VTable interfaces for testing.
│   ├── vector
│   │   ├── hnsw.zig                              # hnsw.zig — M5.1 HNSW (Hierarchical Navigable Small World) Index
│   │   ├── math.zig                              # Vector operations — cosine similarity, normalization, hybrid merge.
│   │   ├── root.zig                              # guidance vector module — cosine search, embeddings, hybrid merge.
│   │   ├── simhash.zig                           # simhash.zig — Charikar SimHash for approximate nearest-neighbour pre-filtering.
│   │   ├── simhash_projections.zig               # simhash_projections.zig — auto-generated by bin/gen_simhash_projections.py
│   │   └── vector_db.zig                         # guidance SQLite vector search database (cosine similarity via BLOB storage).
│   └── wasm
│       ├── execution_request.zig                   # execution_request.zig — M1.1 ExecutionRequestBuilder and ExecutionResultReader
│       └── wasm.zig                                # wasm.zig — Milestone 4: WebAssembly Sandboxing (Extism)
├── vendor
│   └── sqlite3
│       ├── sqlite3.c
│       ├── sqlite3.h
│       └── sqlite3ext.h
├── AGENTS.md
├── build.zig
├── build.zig.zon
├── CLAUDE.md
├── LICENSE
├── LICENSE-Commercial-Requirement
├── LICENSE-Contributor-Agreement
├── Makefile
├── mise.toml
├── pyproject.toml
├── README.md
├── requirements.txt
└── STRUCTURE.md
```
