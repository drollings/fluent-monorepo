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
│   ├── gen_simhash_projections.py                  # Generate 64x384 random unit vectors for SimHash random projection LSH.
│   ├── guidance-php
│   ├── guidance-py
│   └── guidance-ts
├── data
│   └── yago-4.5.0.2-tiny
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
│   │   ├── wasm-tools
│   │   │   └── CAPABILITY.md
│   │   └── INDEX.md
│   ├── coral
│   │   ├── CHANGELOG.md
│   │   ├── DETAILS.md
│   │   ├── OVERVIEW.md
│   │   └── VISION.md
│   ├── guidance
│   │   ├── schemas
│   │   │   └── guidance.schema.json
│   │   ├── DESIGN.md
│   │   ├── MCP.md
│   │   └── VISION.md
│   ├── prompts
│   ├── reviews
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
│   │   ├── content_node.zig                      # content_node.zig — ContentNode: LOD text pyramid backed by SharedString
│   │   ├── delegation.zig                        # delegation.zig — Delegation Pattern for Child Agent Spawning (P4.3)
│   │   ├── drift.zig                             # drift.zig — BitSet DRIFT: deterministic follow-up query generation.
│   │   ├── embeddings.zig                        # [gof-patterns]  Embedding providers — convert text to vectors for semantic search.
│   │   ├── error_context.zig                     # error_context.zig — Structured error context for non-builder code paths.
│   │   ├── format.zig
│   │   ├── hash.zig                              # hash.zig — Generic cryptographic hashing utilities
│   │   ├── interner.zig                          # interner.zig — String interning with optional bitset support.
│   │   ├── io.zig                                # io.zig — Shared buffered I/O helpers
│   │   ├── json.zig                              # json.zig — Generic JSON serialization helpers
│   │   ├── json_parser.zig
│   │   ├── limits.zig                            # limits.zig — Shared resource-limit constants
│   │   ├── local_model.zig                       # local_model.zig — Local LLM Task Decomposition (P6.1)
│   │   ├── log.zig
│   │   ├── logging.zig                           # logging.zig — Structured logging context and timing scope for Fluent WEAVER.
│   │   ├── metrics.zig                           # metrics.zig — Generic latency histogram primitive (M8.1)
│   │   ├── pattern.zig                           # pattern.zig — Design pattern detection heuristics for Zig source code
│   │   ├── refcount.zig                          # refcount.zig — Reference-counted VTable handle wrapper (M7).
│   │   ├── repl.zig
│   │   ├── root.zig                              # common — Module umbrella root.
│   │   ├── shared_string.zig                     # SharedString — heap-allocated, reference-counted, copy-on-write immutable string.
│   │   ├── shell.zig                             # shell.zig — Shared shell command execution helpers
│   │   ├── shell_parser.zig                      # shell_parser.zig — Safe command-string tokenizer
│   │   ├── source.zig                            # source.zig — Source code excerpt extraction helpers
│   │   ├── string.zig                            # string.zig — Generic string classification and inspection helpers
│   │   ├── terminal.zig
│   │   ├── types.zig                             # Number of LOD (Level of Detail) text slots per content node.
│   │   ├── url.zig                               # url.zig — Generic URL validation helpers
│   │   └── wrapper.zig                           # wrapper.zig — Conditional and composable comptime wrappers (M9).
│   ├── concurrency
│   │   ├── any_work_unit.zig                     # any_work_unit.zig — Type-erased work unit and typed wrapper (M11).
│   │   ├── channel.zig                           # channel.zig — Bounded, mutex-backed MPMC channel (M13).
│   │   ├── context.zig                           # [domain-patterns]  context.zig — Cancellation and deadline propagation (M11).
│   │   ├── error_group.zig                       # error_group.zig — Structured parallel dispatch with error capture (M14).
│   │   ├── root.zig                              # concurrency/root.zig — Public API re-exports for the concurrency layer.
│   │   └── spawn.zig                             # spawn.zig — Fire-and-forget dispatch over std.Thread.Pool (M12).
│   ├── coral
│   │   ├── algorithms
│   │   │   ├── degree_centrality.zig           # degree_centrality.zig — Node degree computation for Coral Context graph.
│   │   │   ├── edge_weights.zig                # edge_weights.zig — Co-occurrence edge weight computation.
│   │   │   ├── louvain.zig                     # louvain.zig — Louvain community detection (single-level).
│   │   │   ├── pagerank.zig                    # pagerank.zig — PageRank via power iteration (optional, CLI-only).
│   │   │   ├── shortest_path.zig               # shortest_path.zig — Dijkstra's shortest-path algorithm on CSRGraph.
│   │   │   └── union_find.zig                  # union_find.zig — Union-Find with path compression and union by size.
│   │   ├── agent_loop.zig                        # agent_loop.zig — Agent-Loop Reserved Tools (P4.2)
│   │   ├── algorithm_runner.zig                  # algorithm_runner.zig — Algorithm Runner with Strict Ingestion/Query Separation (P3.6)
│   │   ├── batch.zig                             # batch.zig — Streaming Batch Ingestion Pipeline
│   │   ├── benchmark.zig                         # benchmark.zig — G5 Performance Benchmarks
│   │   ├── cache.zig                             # cache.zig — 5-Tier Cache Hierarchy for Query Routing
│   │   ├── cache_test.zig                        # cache_test.zig — Integration tests for L1-L5 routing pipeline
│   │   ├── cli.zig                               # cli.zig — Ingestion CLI Command Implementation
│   │   ├── config.zig                            # Coral project configuration loader.
│   │   ├── context_node_schema.zig
│   │   ├── csr_graph.zig                         # [domain-patterns]  csr_graph.zig — Compressed Sparse Row (CSR) graph representation.
│   │   ├── db.zig                                # db.zig — Coral Context Database Layer (SQLite backend)
│   │   ├── drift.zig                             # drift.zig — Re-exports BitSet DRIFT from src/common/drift.zig.
│   │   ├── executor.zig                          # executor.zig — DAG Executor for the YAGO ingestion pipeline.
│   │   ├── fixtures.zig                          # fixtures.zig — Test factory functions for coral integration tests
│   │   ├── frontier.zig                          # frontier.zig — M6: L5 Frontier Loop Context Minimization & Validation
│   │   ├── frontier_tool_compiler.zig            # frontier_tool_compiler.zig — Compiles LLM-generated source into WASM tools.
│   │   ├── frozen_snapshot.zig                   # frozen_snapshot.zig — Frozen State Snapshot for Session Prompt Stability
│   │   ├── global_search.zig                     # global_search.zig — GlobalSearch Map-Reduce over Communities (P3.4)
│   │   ├── http_transport.zig                    # http_transport.zig — M4.1/M4.2 HTTP Transport Layer with SSE
│   │   ├── http_transport_test.zig               # http_transport_test.zig — Unit tests for HTTP transport layer
│   │   ├── main.zig
│   │   ├── mcp.zig                               # mcp.zig — Coral MCP (Model Context Protocol) server.
│   │   ├── metrics.zig                           # metrics.zig — Coral Latency Histograms and Resolution Counters (M8.1)
│   │   ├── pattern.zig
│   │   ├── schema.zig                            # schema.zig — Coral Context SQLite Schema (DDL + Queries)
│   │   ├── session.zig                           # session.zig — Coral Session Persistence (SQLite + FTS5)
│   │   ├── targets.zig                           # targets.zig — Ingestion DAG Target Definitions
│   │   ├── token_budget.zig                      # token_budget.zig — Token Estimation for Context Packing (M7.1)
│   │   ├── tool_registry.zig                     # tool_registry.zig — Tool Registry Pattern (P4.1)
│   │   ├── triage.zig                            # Triage subcommand: generate TRIAGE.md from a TODO.md work item.
│   │   ├── type_inference.zig                    # type_inference.zig — Type Inference Cache (P3.7)
│   │   ├── verify.zig                            # verify.zig — Ingestion Verification and Integrity Checking
│   │   └── yago_ingest.zig                       # yago_ingest.zig — YAGO 4.5 Baseline Ingestion (M3.2)
│   ├── dag
│   │   ├── context.zig
│   │   ├── dag_executor.zig                      # dag_executor.zig — M6.1 Parallel DAG Execution
│   │   ├── registry.zig
│   │   ├── resolver.zig
│   │   ├── root.zig                              # dag — DAG execution engine for build systems.
│   │   └── target.zig
│   ├── guidance
│   │   ├── plugins
│   │   │   ├── markdown_plugin.zig             # MarkdownPlugin — extracts sections and metadata from Markdown files.
│   │   │   └── zig_plugin.zig                  # ZigPlugin — wraps ast_parser.zig as a LanguagePlugin.
│   │   ├── ast_parser.zig
│   │   ├── call_extractor.zig                    # call_extractor.zig — AST-based call site extraction for codehealth Phase 2b.
│   │   ├── codebase_map.zig                      # codebase_map.zig — Structural discovery layer for `guidance explain`.
│   │   ├── codehealth.zig                        # codehealth — detect unused modules, redundant code, and dead code candidates.
│   │   ├── comment_cache.zig                     # comment_cache.zig — In-process cache for generated doc comments.
│   │   ├── comment_checker.zig                   # comment_checker.zig — Comment staleness detection for guidance.
│   │   ├── comment_inserter.zig                  # comment_inserter.zig — Insert and replace doc comments in Zig source files.
│   │   ├── comment_parser.zig                    # comment_parser.zig — Doc comment parsing and quality validation for guidance.
│   │   ├── comment_sync.zig                      # comment_sync.zig — Source-code-first comment sync workflow for guidance.
│   │   ├── config.zig                            # guidance project configuration loader.
│   │   ├── deps.zig
│   │   ├── doc_parser.zig                        # doc_parser.zig — Unified parser for SKILL.md and CAPABILITY.md frontmatter.
│   │   ├── document_indexer.zig                  # [gof-patterns]  document_indexer.zig — DocumentIndexer VTable for unified document abstraction.
│   │   ├── enhancer.zig                          # AI Docstring Enhancer for Zig guidance generation.
│   │   ├── git.zig
│   │   ├── hash.zig
│   │   ├── header_generator.zig                  # header_generator.zig — File header comment generation for guidance.
│   │   ├── identifier_match.zig                  # identifier_match.zig — Identifier pattern detection for TIER 0/1 query routing.
│   │   ├── infer_capabilities.zig                # infer_capabilities.zig — M4: InferCapabilities — Capability Discovery Without CAPABILITY.md
│   │   ├── json_store.zig
│   │   ├── line_verify.zig                       # line_verify.zig — Declaration-level line number verification for guidance.
│   │   ├── llm_filter.zig                        # llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
│   │   ├── llm_filter_batch.zig                  # llm_filter_batch.zig — Batch LLM relevance filtering for the staged explain pipeline.
│   │   ├── main.zig                              # guidance — AST-guided SQLite vector search database generator.
│   │   ├── marker.zig                            # Mtime-based change detection for guidance's incremental RALPH loop.
│   │   ├── mcp.zig                               # mcp.zig — guidance MCP server (STDIO transport, JSON-RPC 2.0).
│   │   ├── pattern.zig
│   │   ├── plugin.zig                            # LanguagePlugin — interface for language-specific AST providers.
│   │   ├── plugin_registry.zig                   # PluginRegistry — maps file extensions to LanguagePlugin descriptors.
│   │   ├── provider_discovery.zig                # External language provider discovery for guidance.
│   │   ├── query_engine.zig                      # [gof-patterns]  query_engine.zig — explain, staged, show, test, check commands.
│   │   ├── query_strategy.zig                    # query_strategy.zig — QueryStrategy VTable for intent-based query routing.
│   │   ├── ralph.zig                             # [domain-patterns]  ralph.zig — RALPH Loop: Read → Ask → Learn → Plan → Help
│   │   ├── scanner.zig                           # scanner.zig — M9: CodebaseScanner — Generic Codebase Analysis
│   │   ├── schema_validator.zig                  # schema_validator.zig — GuidanceDoc field validation.
│   │   ├── stage_builder.zig                     # [gof-patterns]  stage_builder.zig — StageBuilder VTable for typed, pre-allocated stage production.
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
│   ├── guidance-cpp
│   │   └── main.cpp                              # guidance-cpp: C++ AST provider for the guidance system
│   ├── guidance-rs
│   │   └── src
│   │       └── main.rs
│   ├── guidance-rust
│   ├── llm
│   │   ├── anonymize.zig                         # anonymize.zig — PII anonymization for frontier LLM context minimization.
│   │   ├── context_compressor.zig                # context_compressor.zig — Context Compression for Token Budget Management
│   │   ├── context_packer.zig                    # context_packer.zig — Context Packing with Head/Tail Protection (P3.3)
│   │   ├── llm.zig                               # llm.zig — LLM response post-processing for guidance and coral.
│   │   ├── root.zig                              # llm — General-purpose LLM inference client.
│   │   └── token_budget.zig                      # token_budget.zig — Token Estimation (shared between guidance and coral).
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
│   │   └── mock_vtable.zig                       # [gof-patterns]  mock_vtable.zig — Mock implementations of VTable interfaces for testing.
│   ├── vector
│   │   ├── hnsw.zig                              # hnsw.zig — M5.1 HNSW (Hierarchical Navigable Small World) Index
│   │   ├── math.zig                              # Vector operations — cosine similarity, normalization, hybrid merge.
│   │   ├── quantized_embedding.zig               # quantized_embedding.zig — int8 Quantized Embeddings for Memory Efficiency
│   │   ├── root.zig                              # guidance vector module — cosine search, embeddings, hybrid merge.
│   │   ├── simhash.zig                           # simhash.zig — Locality-sensitive hashing for embeddings and tokens.
│   │   ├── simhash_projections.zig               # simhash_projections.zig — auto-generated by bin/gen_simhash_projections.py
│   │   └── vector_db.zig                         # guidance SQLite vector search database (cosine similarity via BLOB storage).
│   └── wasm
│       ├── execution_request.zig                   # execution_request.zig — M1.1 ExecutionRequestBuilder and ExecutionResultReader
│       └── wasm.zig                                # wasm.zig — Milestone 4: WebAssembly Sandboxing (Extism)
├── tools
├── vendor
│   └── sqlite3
│       ├── sqlite3.c
│       ├── sqlite3.h
│       └── sqlite3ext.h
├── AGENTS.md
├── build.zig
├── build.zig.zon
├── Cargo.toml
├── CLAUDE.md
├── LICENSE
├── LICENSE-Commercial-Requirement
├── LICENSE-Contributor-Agreement
├── Makefile
├── mise.toml
├── package.json
├── pyproject.toml
├── README.md
├── requirements.txt
└── STRUCTURE.md
```
