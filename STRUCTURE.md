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
│   └── gen_simhash_projections.py                  # Generate 64x384 random unit vectors for SimHash random projection LSH.
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
│   │   ├── vaxis_stub
│   │   │   └── root.zig
│   │   ├── args.zig
│   │   ├── builder_error.zig                     # builder_error.zig — Structured error type for fluent builder chains.
│   │   ├── builder_error_tests.zig               # Tests for builder_error.zig.
│   │   ├── cli.zig
│   │   ├── constants.zig                         # constants.zig — Shared resource-limit constants
│   │   ├── content_node.zig                      # content_node.zig — ContentNode: LOD text pyramid backed by SharedString
│   │   ├── drift.zig                             # drift.zig — BitSet DRIFT: deterministic follow-up query generation.
│   │   ├── embeddings.zig                        # [gof-patterns]  Embedding providers — convert text to vectors for semantic search.
│   │   ├── embeddings_tests.zig                  # Tests for embeddings.zig.
│   │   ├── entity.zig
│   │   ├── error_context.zig                     # error_context.zig — Structured error context for non-builder code paths.
│   │   ├── file_lock.zig
│   │   ├── format.zig
│   │   ├── freq_table.zig
│   │   ├── hash.zig                              # hash.zig — Generic cryptographic hashing utilities
│   │   ├── hash_tests.zig                        # Tests for hash.zig.
│   │   ├── interner.zig                          # interner.zig — String interning with optional bitset support.
│   │   ├── io.zig                                # io.zig — Shared buffered I/O helpers
│   │   ├── io_tests.zig                          # Tests for io.zig.
│   │   ├── json.zig                              # json.zig — Generic JSON serialization helpers
│   │   ├── json_tests.zig                        # Tests for json.zig.
│   │   ├── log.zig                               # Global logger with console + file output.
│   │   ├── logging.zig                           # logging.zig — Structured logging context and timing scope for Fluent WEAVER.
│   │   ├── logging_tests.zig                     # Tests for logging.zig.
│   │   ├── metrics.zig                           # metrics.zig — Generic latency histogram primitive (M8.1)
│   │   ├── pattern.zig                           # pattern.zig — Design pattern detection heuristics for Zig source code
│   │   ├── pattern_tests.zig                     # Tests for pattern.zig.
│   │   ├── query_cache.zig
│   │   ├── refcount.zig                          # refcount.zig — Reference-counted VTable handle wrapper (M7).
│   │   ├── root.zig                              # common — Module umbrella root.
│   │   ├── shell.zig                             # shell.zig — Shared shell command execution helpers
│   │   ├── shell_parser.zig                      # shell_parser.zig — Safe command-string tokenizer
│   │   ├── shell_parser_tests.zig                # Tests for shell_parser.zig.
│   │   ├── shell_tests.zig                       # Tests for shell.zig.
│   │   ├── snapshot.zig
│   │   ├── source.zig                            # source.zig — Source code excerpt extraction helpers
│   │   ├── source_tests.zig                      # Tests for source.zig.
│   │   ├── string.zig                            # string.zig — Generic string classification and inspection helpers
│   │   ├── string_tests.zig                      # Tests for string.zig.
│   │   ├── terminal.zig
│   │   ├── tokenizer.zig
│   │   ├── trigram_index.zig
│   │   ├── types.zig                             # Number of LOD (Level of Detail) text slots per content node.
│   │   ├── url.zig                               # url.zig — Generic URL validation helpers
│   │   ├── url_tests.zig                         # Tests for url.zig.
│   │   ├── word_index.zig
│   │   ├── wrapper.zig                           # wrapper.zig — Conditional and composable comptime wrappers (M9).
│   │   └── wrapper_tests.zig                     # Tests for wrapper.zig.
│   ├── concurrency
│   │   ├── any_work_unit.zig                     # any_work_unit.zig — Type-erased work unit and typed wrapper (M11).
│   │   ├── channel.zig                           # channel.zig — Bounded, mutex-backed MPMC channel (M13).
│   │   ├── channel_tests.zig                     # Tests for channel.zig.
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
│   │   ├── cache.zig                             # cache.zig — 5-Tier Cache Hierarchy for Query Routing (re-export facade)
│   │   ├── cache_l1.zig                          # cache_l1.zig — L1/L1Hash Cache Types
│   │   ├── cache_reactor.zig                     # [gof-patterns]  cache_reactor.zig — QueueReactorBuilder and QueueReactor.
│   │   ├── cache_router.zig                      # cache_router.zig — ParallelRouter for batched concurrent query routing.
│   │   ├── cache_test.zig                        # cache_test.zig — Integration tests for L1-L5 routing pipeline
│   │   ├── cli.zig                               # cli.zig — Ingestion CLI Command Implementation
│   │   ├── config.zig                            # Coral project configuration loader.
│   │   ├── context_node_schema.zig
│   │   ├── db.zig                                # db.zig — Coral Context Database Layer (SQLite backend)
│   │   ├── delegation.zig                        # delegation.zig — Delegation Pattern for Child Agent Spawning (P4.3)
│   │   ├── executor.zig                          # executor.zig — DAG Executor for the YAGO ingestion pipeline.
│   │   ├── frontier.zig                          # frontier.zig — M6: L5 Frontier Loop Context Minimization & Validation
│   │   ├── frontier_tool_compiler.zig            # frontier_tool_compiler.zig — Compiles LLM-generated source into WASM tools.
│   │   ├── global_search.zig                     # global_search.zig — GlobalSearch Map-Reduce over Communities (P3.4)
│   │   ├── http_transport.zig                    # http_transport.zig — M4.1/M4.2 HTTP Transport Layer with SSE
│   │   ├── http_transport_test.zig               # http_transport_test.zig — Unit tests for HTTP transport layer
│   │   ├── main.zig
│   │   ├── main_tests.zig                        # Tests for main.zig.
│   │   ├── mcp.zig                               # mcp.zig — Coral MCP (Model Context Protocol) server.
│   │   ├── metrics.zig                           # metrics.zig — Coral Latency Histograms and Resolution Counters (M8.1)
│   │   ├── root.zig                              # coral/root.zig — Public API re-exports for the coral module.
│   │   ├── schema.zig                            # schema.zig — Coral Context SQLite Schema (DDL + Queries)
│   │   ├── session.zig                           # session.zig — Coral Session Persistence (SQLite + FTS5)
│   │   ├── targets.zig                           # targets.zig — Ingestion DAG Target Definitions
│   │   ├── token_budget.zig                      # token_budget.zig — Token Estimation for Context Packing (M7.1)
│   │   ├── tool_registry.zig                     # tool_registry.zig — Tool Registry Pattern (P4.1)
│   │   ├── verify.zig                            # verify.zig — Ingestion Verification and Integrity Checking
│   │   └── yago_ingest.zig                       # yago_ingest.zig — YAGO 4.5 Baseline Ingestion (M3.2)
│   ├── dag
│   │   ├── context.zig
│   │   ├── dag_executor.zig                      # dag_executor.zig — M6.1 Parallel DAG Execution
│   │   ├── json_parser.zig
│   │   ├── registry.zig
│   │   ├── repl.zig
│   │   ├── resolver.zig
│   │   ├── root.zig                              # dag — DAG execution engine for build systems.
│   │   └── target.zig
│   ├── guidance
│   │   ├── codehealth
│   │   │   ├── build_validation.zig            # build_validation.zig — Phase 1.5: build.zig consistency validation.
│   │   │   ├── build_validation_tests.zig      # Tests for build_validation.zig.
│   │   │   ├── extractor.zig                   # call_extractor.zig — AST-based call site extraction for codehealth Phase 2b.
│   │   │   ├── extractor_tests.zig             # Tests for extractor.zig.
│   │   │   ├── main.zig                        # codehealth — detect unused modules, redundant code, and dead code candidates.
│   │   │   ├── main_tests.zig                  # Tests for main.zig.
│   │   │   ├── orphan.zig                      # orphan.zig — Phase 0: Orphaned source file detection for `guidance codehealth`.
│   │   │   ├── orphan_tests.zig                # Tests for orphan.zig.
│   │   │   ├── test_audit.zig                  # test_audit.zig — Phase 2: Test file convention enforcement.
│   │   │   ├── test_audit_tests.zig            # Tests for test_audit.zig.
│   │   │   ├── test_mover.zig                  # test_mover.zig — Move inline tests from source .zig files to <name>_tests.zig.
│   │   │   └── test_mover_tests.zig            # Tests for test_mover.zig.
│   │   ├── comments
│   │   │   ├── core.zig                        # comments/core.zig — Merged doc comment processing for guidance.
│   │   │   ├── core_tests.zig                  # Tests for core.zig.
│   │   │   ├── header.zig                      # header_generator.zig — File header comment generation for guidance.
│   │   │   ├── header_tests.zig                # Tests for header.zig.
│   │   │   ├── inserter.zig                    # comment_inserter.zig — Insert and replace doc comments in Zig source files.
│   │   │   ├── inserter_tests.zig              # Tests for inserter.zig.
│   │   │   ├── sync.zig                        # comment_sync.zig — Source-code-first comment sync workflow for guidance.
│   │   │   └── sync_tests.zig                  # Tests for sync.zig.
│   │   ├── core
│   │   │   ├── drift.zig                       # core/drift.zig — Drift follow-up suggestion logic.
│   │   │   ├── excerpt.zig                     # core/excerpt.zig — Unified source excerpt extraction.
│   │   │   ├── format.zig                      # core/format.zig — Unified markdown formatting for explain output.
│   │   │   ├── intent.zig                      # core/intent.zig — Deterministic query intent classification.
│   │   │   ├── metadata.zig                    # core/metadata.zig — Unified GuidanceDoc JSON metadata loading.
│   │   │   ├── ranking.zig                     # core/ranking.zig — Unified result ranking and scoring.
│   │   │   └── skill_loader.zig                # core/skill_loader.zig — Unified SKILL.md paragraph loading.
│   │   ├── plugins
│   │   │   ├── markdown_plugin.zig             # MarkdownPlugin — extracts sections and metadata from Markdown files.
│   │   │   ├── markdown_plugin_tests.zig       # Tests for markdown_plugin.zig.
│   │   │   ├── treesitter_extractor.zig        # TreeSitterExtractor — walks tree-sitter syntax trees and extracts guidance members.
│   │   │   ├── treesitter_loader.zig           # TreeSitterLoader — loads and manages tree-sitter language grammars.
│   │   │   ├── treesitter_plugin.zig           # TreeSitterPlugin — universal AST parser using tree-sitter for non-Zig languages.
│   │   │   ├── zig_plugin.zig                  # ZigPlugin — wraps ast_parser.zig as a LanguagePlugin.
│   │   │   └── zig_plugin_tests.zig            # Tests for zig_plugin.zig.
│   │   ├── query
│   │   │   ├── args.zig                        # query/args.zig — Argument parsing for explain and related query commands.
│   │   │   ├── identifier.zig                  # identifier_match.zig — Identifier pattern detection for TIER 0/1 query routing.
│   │   │   ├── llm_filter.zig                  # llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
│   │   │   ├── llm_filter_batch.zig            # llm_filter_batch.zig — Batch LLM relevance filtering for the staged explain pipeline.
│   │   │   ├── strategy.zig                    # query_strategy.zig — Query routing by intent.
│   │   │   ├── strategy_tests.zig              # Tests for strategy.zig.
│   │   │   └── synthesize.zig                  # synthesize.zig — LLM-based synthesis for the staged explain pipeline.
│   │   ├── sync
│   │   │   ├── commit.zig                      # sync/commit.zig — Git commit message generation from staged diff + guidance JSON context.
│   │   │   ├── gen_files.zig                   # sync/gen_files.zig — Gen command, file pipeline, and DB sync logic.
│   │   │   ├── json_store.zig                  # JSON store for guidance sync — reads/writes .guidance/src/**/*.json files.
│   │   │   ├── json_writer.zig                 # sync/json_writer.zig — JSON serialization for guidance documents.
│   │   │   ├── line_verify.zig                 # line_verify.zig — Declaration-level line number verification for guidance.
│   │   │   ├── line_verify_tests.zig           # Tests for line_verify.zig.
│   │   │   ├── marker.zig                      # Mtime-based change detection for guidance's incremental RALPH loop.
│   │   │   ├── marker_tests.zig                # Tests for marker.zig.
│   │   │   └── ralph.zig                       # sync/ralph.zig — RALPH loop orchestration (check phase helpers).
│   │   ├── agents_md.zig                         # AGENTS.md content generator for guidance init.
│   │   ├── ast_parser.zig                        # AST parser for Zig source files — extracts declarations and comments.
│   │   ├── codebase_map.zig                      # codebase_map.zig — Structural discovery layer for `guidance explain`.
│   │   ├── config.zig                            # [gof-patterns]  guidance project configuration loader.
│   │   ├── doc_parser.zig                        # doc_parser.zig — Unified parser for SKILL.md and CAPABILITY.md frontmatter.
│   │   ├── doc_parser_tests.zig                  # Tests for doc_parser.zig.
│   │   ├── document_indexer.zig                  # [gof-patterns]  document_indexer.zig — Document indexer for Guidance JSON documents.
│   │   ├── document_indexer_tests.zig            # Tests for document_indexer.zig.
│   │   ├── enhancer.zig                          # AI Docstring Enhancer for Zig guidance generation.
│   │   ├── enhancer_tests.zig                    # Tests for enhancer.zig.
│   │   ├── git.zig                               # Gitignore-aware file filtering for guidance scanner.
│   │   ├── git_tests.zig                         # Tests for git.zig.
│   │   ├── hash.zig                              # Hash utilities for guidance — computes stable hashes for API signatures and struct members.
│   │   ├── hash_tests.zig                        # Tests for hash.zig.
│   │   ├── infer_capabilities.zig                # infer_capabilities.zig — M4: InferCapabilities — Capability Discovery Without CAPABILITY.md
│   │   ├── main.zig                              # guidance — AST-guided SQLite vector search database generator.
│   │   ├── mcp.zig                               # mcp.zig — guidance MCP server (STDIO transport, JSON-RPC 2.0).
│   │   ├── pattern.zig                           # Pattern detection for Zig AST nodes — detects GoF and domain patterns.
│   │   ├── plugin.zig                            # LanguagePlugin — interface for language-specific AST providers.
│   │   ├── plugin_registry.zig                   # PluginRegistry — maps file extensions to LanguagePlugin descriptors.
│   │   ├── plugin_registry_tests.zig             # Tests for plugin_registry.zig.
│   │   ├── plugin_tests.zig                      # Tests for plugin.zig.
│   │   ├── provider_discovery.zig                # External language provider discovery for guidance.
│   │   ├── provider_discovery_tests.zig          # Tests for provider_discovery.zig.
│   │   ├── query_engine.zig                      # [gof-patterns]  query_engine.zig — explain, staged, show, test, check commands.
│   │   ├── ralph.zig                             # [domain-patterns]  ralph.zig — RALPH Loop: Read → Ask → Learn → Plan → Help
│   │   ├── ralph_tests.zig                       # Tests for ralph.zig.
│   │   ├── scanner.zig                           # scanner.zig — M9: CodebaseScanner — Generic Codebase Analysis
│   │   ├── scanner_tests.zig                     # Tests for scanner.zig.
│   │   ├── schema_validator.zig                  # schema_validator.zig — GuidanceDoc field validation.
│   │   ├── skeleton.zig                          # skeleton.zig — File and struct skeleton extraction for token-efficient discovery.
│   │   ├── stage_builder.zig                     # [gof-patterns]  stage_builder.zig — Stage builder for typed, pre-allocated stage production.
│   │   ├── stage_builder_tests.zig               # Tests for stage_builder.zig.
│   │   ├── staged.zig                            # staged.zig — Staged explain pipeline for `guidance explain`.
│   │   ├── staged_tests.zig                      # Tests for staged.zig.
│   │   ├── structure.zig                         # STRUCTURE.md generator.
│   │   ├── sync.zig                              # Sync engine for guidance — processes source files and generates JSON metadata.
│   │   ├── sync_engine.zig                       # sync_engine.zig — init, commit, gen, status, clean, pipeline, and utility commands.
│   │   ├── tests.zig                             # [gof-patterns]  Unit tests for src/guidance — json_store merge logic, sync, config, and commit helpers.
│   │   ├── todo.zig                              # todo.zig — Work item lifecycle tracking for guidance.
│   │   ├── todo_tests.zig                        # Tests for todo.zig.
│   │   ├── triage.zig                            # Triage subcommand: generate TRIAGE.md from a TODO.md work item.
│   │   ├── triage_tests.zig                      # Tests for triage.zig.
│   │   ├── types.zig                             # Shared types for guidance — FileType, MemberType, Member, Stage, QueryResult, etc.
│   │   └── types_tests.zig                       # Tests for types.zig.
│   ├── guidance-cpp
│   │   └── main.cpp
│   ├── guidance-rs
│   │   └── src
│   │       └── main.rs
│   ├── llm
│   │   ├── anonymize.zig                         # anonymize.zig — PII anonymization for frontier LLM context minimization.
│   │   ├── context_compressor.zig                # context_compressor.zig — Context Compression for Token Budget Management
│   │   ├── context_packer.zig                    # context_packer.zig — Context Packing with Head/Tail Protection (P3.3)
│   │   ├── llm.zig                               # llm.zig — LLM client, response post-processing, and task decomposition.
│   │   ├── root.zig                              # llm — General-purpose LLM inference client.
│   │   ├── root_tests.zig                        # Tests for root.zig.
│   │   ├── token_budget.zig                      # token_budget.zig — Token Estimation (shared between guidance and coral).
│   │   └── token_budget_tests.zig                # Tests for token_budget.zig.
│   ├── ontology
│   │   ├── inference.zig                         # inference.zig — Ontology Inference Engine (R5)
│   │   ├── mapper.zig                            # mapper.zig — Triple → ContextNode Mapper
│   │   ├── migration.zig                         # migration.zig — Ontology Versioning and Migration
│   │   ├── root.zig                              # ontology/root.zig — Ontology processing module umbrella
│   │   └── yago.zig                              # yago.zig — YAGO 4.5 Ontology Schema Definition
│   ├── rdf
│   │   ├── lexer.zig                             # lexer.zig — Streaming Turtle (Terse RDF Triple Language) Lexer
│   │   ├── lexer_tests.zig                       # Tests for lexer.zig.
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
│   │   ├── schema_version_tests.zig              # Tests for schema_version.zig.
│   │   ├── sql.zig                               # sql.zig — Schema-driven SQLite binding and hydration.
│   │   ├── sql_tests.zig                         # Tests for sql.zig.
│   │   ├── typed.zig                             # typed.zig — TypedAccessorTable(T) and TypedEditable.
│   │   ├── validate.zig                          # validate.zig — Runtime validation pipeline for FieldMeta constraints (M6).
│   │   └── validate_tests.zig                    # Tests for validate.zig.
│   ├── testing
│   │   ├── mock_vtable.zig                       # [gof-patterns]  mock_vtable.zig — Mock implementations of VTable interfaces for testing.
│   │   └── mock_vtable_tests.zig                 # Tests for mock_vtable.zig.
│   ├── vector
│   │   ├── hnsw.zig                              # hnsw.zig — M5.1 HNSW (Hierarchical Navigable Small World) Index
│   │   ├── math.zig                              # Vector operations — cosine similarity, normalization, hybrid merge.
│   │   ├── math_tests.zig                        # Tests for math.zig.
│   │   ├── quantized_embedding.zig               # quantized_embedding.zig — int8 Quantized Embeddings for Memory Efficiency
│   │   ├── root.zig                              # guidance vector module — cosine search, embeddings, hybrid merge.
│   │   ├── simhash.zig                           # simhash.zig — Locality-sensitive hashing for embeddings and tokens.
│   │   ├── simhash_projections.zig               # simhash_projections.zig — auto-generated by bin/gen_simhash_projections.py
│   │   ├── simhash_tests.zig                     # Tests for simhash.zig.
│   │   ├── vector_db.zig                         # guidance SQLite vector search database (cosine similarity via BLOB storage).
│   │   └── vector_db_tests.zig                   # Tests for vector_db.zig.
│   └── wasm
│       ├── execution_request.zig                   # execution_request.zig — M1.1 ExecutionRequestBuilder and ExecutionResultReader
│       ├── root.zig                                # wasm — WebAssembly Sandboxing (Extism)
│       └── wasm.zig                                # wasm.zig — Milestone 4: WebAssembly Sandboxing (Extism)
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
