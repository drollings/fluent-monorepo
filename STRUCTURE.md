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
│   └── guidance-py
├── data
│   └── yago-4.5.0.2-tiny
│       └── yago-tiny.ttl
├── doc
│   ├── coral
│   │   ├── Legacy
│   │   │   ├── CoralContext
│   │   │   │   ├── CoralContext_Zig_BinaryBlock.md
│   │   │   │   ├── CoralContext_Zig_ContextNode_Embedding.md
│   │   │   │   ├── CoralContext_Zig_DatasetIngestion.md
│   │   │   │   ├── CoralContext_Zig_LEANN.md
│   │   │   │   ├── CoralContext_Zig_Legacy1.md
│   │   │   │   ├── CoralContext_Zig_Milestone1.md
│   │   │   │   ├── CoralContext_Zig_Milestone2.md
│   │   │   │   ├── CoralContext_Zig_Milestone3.md
│   │   │   │   ├── CoralContext_Zig_Milestone4.md
│   │   │   │   ├── CoralContext_Zig_Milestone5.md
│   │   │   │   ├── CoralContext_Zig_ToolCategories.md
│   │   │   │   └── CoralContext_Zig_ToolDevelopment.md
│   │   │   ├── coral-context-implementation-stages-python.md
│   │   │   ├── coral-context-overview-2026.md
│   │   │   ├── Gemini3-PriorSpecsAdapted.md
│   │   │   ├── legacy-aliases.json
│   │   │   ├── MAKEFILE_GUIDANCE.md
│   │   │   ├── REPORT_WORLDCORE.md
│   │   │   ├── ROADMAP_EXPLAIN_ENHANCE.md
│   │   │   ├── ROADMAP_EXPLAIN_ENHANCE_CHECKLIST.md
│   │   │   ├── ROADMAP_NEW_EXPLAIN.md
│   │   │   ├── ROADMAP_NEW_EXPLAIN_CHECKLIST.md
│   │   │   ├── TEST_EXPLAIN.md
│   │   │   ├── TEST_EXPLAIN_RESULTS.md
│   │   │   ├── TODO_COMMON.md
│   │   │   ├── TODO_CONCISION.md
│   │   │   ├── TODO_CONCISION_CHECKLIST.md
│   │   │   ├── TODO_EXPLORE.md
│   │   │   ├── TODO_EXPLORE_CHECKLIST.md
│   │   │   ├── TODO_REFLECTION.md
│   │   │   ├── TODO_YAGO.md
│   │   │   ├── TODO_YAGO_CHECKLIST.md
│   │   │   ├── unifiedprompt2.md
│   │   │   ├── YAGO-to-property.md
│   │   │   └── zig-reflection.md
│   │   ├── proposals
│   │   │   ├── CORAL_CONTEXT_BITOPS.md
│   │   │   ├── CORAL_CONTEXT_DECORATORS.md
│   │   │   ├── CORAL_CONTEXT_DYAMAKE.md
│   │   │   ├── CORAL_CONTEXT_FLUENT.md
│   │   │   ├── CORAL_CONTEXT_REASONING.md
│   │   │   └── VOICE_NOTE_CORAL.md
│   │   ├── CHANGELOG.md
│   │   ├── DETAILS.md
│   │   ├── OVERVIEW.md
│   │   └── VISION.md
│   ├── guidance
│   │   ├── proposals
│   │   │   ├── AIDER_USAGE.md
│   │   │   ├── DESIGN-DECISIONS-RECOMMENDATIONS.md
│   │   │   ├── DETAILED_SPECS.md
│   │   │   ├── EXAMPLE_QUERY.md
│   │   │   ├── GEMINI_DISCUSS_EMBEDDING.md
│   │   │   ├── GUIDANCE_LANCEDB.md
│   │   │   ├── MAKEFILE_GUIDANCE.md
│   │   │   ├── PROMPT_CONSOLIDATION.md
│   │   │   ├── REFACTOR.md
│   │   │   ├── ROADMAP_OPTIMIZE_VECTOR_SEARCH.md
│   │   │   ├── SECONDBRAIN.md
│   │   │   ├── SKILLGRAPH.md
│   │   │   ├── TINY_ZIG_AGENTS.md
│   │   │   ├── TODO.md
│   │   │   ├── TODO_AIDER.md
│   │   │   ├── TODO_GUIDANCE.md
│   │   │   ├── TODO_GUIDANCE2.md
│   │   │   ├── TODO_ZIG_GUIDANCE.md
│   │   │   └── ZIG_PROJECTS.md
│   │   ├── schemas
│   │   │   └── guidance.schema.json
│   │   └── DESIGN.md
│   ├── patterns
│   │   └── FLUENT_WVR.md
│   ├── capabilities
│   └── skills
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
│   │   ├── cli.zig
│   │   ├── context.zig
│   │   ├── embeddings.zig                                         # Embedding providers — convert text to vectors for semantic search.
│   │   ├── format.zig
│   │   ├── hash.zig                                               # hash.zig — Generic cryptographic hashing utilities
│   │   ├── interner.zig                                           # interner.zig — String interning with optional bitset support.
│   │   ├── io.zig                                                 # io.zig — Shared buffered I/O helpers
│   │   ├── json.zig                                               # json.zig — Generic JSON serialization helpers
│   │   ├── json_parser.zig
│   │   ├── llm.zig                                                # common — Shared utilities and LLM client for guidance, vector, and coral.
│   │   ├── local_model.zig                                        # local_model.zig — Local LLM Task Decomposition (P6.1)
│   │   ├── log.zig
│   │   ├── registry.zig
│   │   ├── repl.zig
│   │   ├── resolver.zig
│   │   ├── root.zig                                               # common — Module umbrella root.
│   │   ├── shared_string.zig                                      # SharedString — heap-allocated, reference-counted, immutable string.
│   │   ├── source.zig                                             # source.zig — Source code excerpt extraction helpers
│   │   ├── str.zig                                                # str.zig — Generic string classification and inspection helpers
│   │   ├── string.zig
│   │   ├── target.zig
│   │   ├── terminal.zig
│   │   └── url.zig                                                # url.zig — Generic URL validation helpers
│   ├── coral
│   │   ├── batch.zig                                              # batch.zig — Streaming Batch Ingestion Pipeline
│   │   ├── cache.zig                                              # cache.zig — 5-Tier Cache Hierarchy for Query Routing
│   │   ├── cli.zig                                                # cli.zig — Ingestion CLI Command Implementation
│   │   ├── config.zig                                             # Coral project configuration loader.
│   │   ├── context_node_schema.zig
│   │   ├── db.zig                                                 # db.zig — Coral Context Database Layer (SQLite backend)
│   │   ├── executor.zig                                           # executor.zig — DAG Executor for the YAGO ingestion pipeline.
│   │   ├── frontier.zig                                           # frontier.zig — M6: L5 Frontier Loop Context Minimization & Validation
│   │   ├── main.zig
│   │   ├── mcp.zig                                                # mcp.zig — Coral MCP (Model Context Protocol) server.
│   │   ├── pattern.zig
│   │   ├── schema.zig                                             # schema.zig — Coral Context SQLite Schema (DDL + Queries)
│   │   ├── scrub.zig                                              # scrub.zig — Comment quality filter for ast-guidance infill pipeline.
│   │   ├── targets.zig                                            # targets.zig — Ingestion DAG Target Definitions
│   │   ├── triage.zig                                             # Triage subcommand: generate TRIAGE.md from a TODO.md work item.
│   │   └── verify.zig                                             # verify.zig — Ingestion Verification and Integrity Checking
│   ├── guidance
│   │   ├── plugins
│   │   │   ├── markdown_plugin.zig                              # MarkdownPlugin — extracts sections and metadata from Markdown files.
│   │   │   └── zig_plugin.zig                                   # ZigPlugin — wraps ast_parser.zig as a LanguagePlugin.
│   │   ├── ast_parser.zig
│   │   ├── comment_cache.zig                                      # comment_cache.zig — In-process cache for generated doc comments.
│   │   ├── comment_checker.zig                                    # comment_checker.zig — Comment staleness detection for guidance.
│   │   ├── comment_inserter.zig                                   # comment_inserter.zig — Insert and replace doc comments in Zig source files.
│   │   ├── comment_parser.zig                                     # comment_parser.zig — Doc comment parsing and quality validation for guidance.
│   │   ├── comment_sync.zig                                       # comment_sync.zig — Source-code-first comment sync workflow for guidance.
│   │   ├── config.zig                                             # guidance project configuration loader.
│   │   ├── deps.zig
│   │   ├── enhancer.zig                                           # AI Docstring Enhancer for Zig guidance generation.
│   │   ├── git.zig
│   │   ├── hash.zig
│   │   ├── header_generator.zig                                   # header_generator.zig — File header comment generation for guidance.
│   │   ├── json_store.zig
│   │   ├── line_verify.zig                                        # line_verify.zig — Declaration-level line number verification for guidance.
│   │   ├── llm_filter.zig                                         # llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
│   │   ├── main.zig                                               # guidance — AST-guided SQLite vector search database generator.
│   │   ├── marker.zig                                             # Mtime-based change detection for guidance's incremental RALPH loop.
│   │   ├── pattern.zig
│   │   ├── plugin.zig                                             # LanguagePlugin — interface for language-specific AST providers.
│   │   ├── plugin_registry.zig                                    # PluginRegistry — maps file extensions to LanguagePlugin descriptors.
│   │   ├── provider_discovery.zig                                 # External language provider discovery for guidance.
│   │   ├── query_engine.zig                                       # query_engine.zig — explain, staged, show, test, check commands.
│   │   ├── scrub.zig                                              # scrub.zig — Synthetic comment detection and scrubbing.
│   │   ├── staged.zig                                             # staged.zig — Staged explain pipeline for `guidance explain`.
│   │   ├── structure.zig                                          # STRUCTURE.md generator.
│   │   ├── sync.zig
│   │   ├── sync_engine.zig                                        # sync_engine.zig — init, commit, gen, status, clean, pipeline, and utility commands.
│   │   ├── synthesize.zig                                         # synthesize.zig — LLM-based synthesis for the staged explain pipeline.
│   │   ├── tests.zig                                              # Unit tests for src/guidance — json_store merge logic, sync, config, and commit helpers.
│   │   ├── todo.zig                                               # todo.zig — Work item lifecycle tracking for guidance.
│   │   ├── triage.zig                                             # Triage subcommand: generate TRIAGE.md from a TODO.md work item.
│   │   └── types.zig
│   ├── llm
│   │   └── root.zig                                               # llm — General-purpose LLM inference client.
│   ├── ontology
│   │   ├── inference.zig                                          # inference.zig — Ontology Inference Engine (R5)
│   │   ├── mapper.zig                                             # mapper.zig — Triple → ContextNode Mapper
│   │   ├── migration.zig                                          # migration.zig — Ontology Versioning and Migration
│   │   ├── root.zig                                               # ontology/root.zig — Ontology processing module umbrella
│   │   └── yago.zig                                               # yago.zig — YAGO 4.5 Ontology Schema Definition
│   ├── rdf
│   │   ├── lexer.zig                                              # lexer.zig — Streaming Turtle (Terse RDF Triple Language) Lexer
│   │   ├── normalize.zig                                          # normalize.zig — RDF Term Normalization
│   │   ├── nquads.zig                                             # nquads.zig — N-Quads / N-Triples Parser (line-based, no prefix expansion)
│   │   ├── parser.zig                                             # parser.zig — Streaming Recursive-Descent Turtle Parser
│   │   └── root.zig                                               # rdf/root.zig — RDF parsing module umbrella
│   ├── reflection
│   │   ├── accessor.zig                                           # accessor.zig — Accessor, DynamicEditable, Editable(T), FieldMeta, TypeTag, OwnershipMode.
│   │   ├── binary.zig                                             # binary.zig — BinaryFieldCodec for wire-format encoding/decoding of struct fields.
│   │   ├── constraint.zig                                         # constraint.zig — ConstraintVTable, constraintSet, constraintGet, Constraint(T).
│   │   ├── enum_registry.zig                                      # enum_registry.zig — EnumRegistry for runtime enum name/value lookups.
│   │   ├── permissions.zig                                        # permissions.zig — Role-based permission system for Coral Context reflection.
│   │   ├── root.zig                                               # reflection — Coral Context field-level reflection, validation, and permission layer.
│   │   └── typed.zig                                              # typed.zig — TypedAccessorTable(T) and TypedEditable.
│   ├── vector
│   │   ├── math.zig                                               # Vector operations — cosine similarity, normalization, hybrid merge.
│   │   ├── root.zig                                               # guidance vector module — cosine search, embeddings, hybrid merge.
│   │   ├── simhash.zig                                            # simhash.zig — Charikar SimHash for approximate nearest-neighbour pre-filtering.
│   │   ├── simhash_projections.zig                                # simhash_projections.zig — auto-generated by tools/gen_simhash_projections.py
│   │   └── vector_db.zig                                          # guidance SQLite vector search database (cosine similarity via BLOB storage).
│   └── wasm
│       └── wasm.zig                                                 # wasm.zig — Milestone 4: WebAssembly Sandboxing (Extism)
├── tools
│   └── gen_simhash_projections.py
├── vendor
│   └── sqlite3
│       ├── sqlite3.c
│       ├── sqlite3.h
│       └── sqlite3ext.h
├── AGENTS.md
├── build.zig
├── build.zig.zon
├── CLAUDE.md
├── GEMINI_FLUENT_WVR_STANDARD_ADDITIONS.md
├── GLM_FLUENT_GUIDANCE_ASSESSMENT.md
├── GLM_REVIEW_FLUENT_WVR_STANDARD_ADDITIONS.md
├── LICENSE
├── LICENSE-Commercial-Requirement
├── LICENSE-Contributor-Agreement
├── Makefile
├── mise.toml
├── pyproject.toml
├── README.md
├── RECOMMENDATIONS_SEARCH_20260325.md
├── requirements.txt
├── REVIEW_20260325.md
├── REVIEW_20260326.md
├── ROADMAP_COMPLETION.md
├── ROADMAP_COMPLETION_CHECKLIST.md
├── ROADMAP_MONOREPO_MARCH.md
├── ROADMAP_MONOREPO_MARCH_CHECKLIST.md
├── STRUCTURE.md
├── TEST_EXPLAIN_PROMPT.md
├── TODO.md
├── TODO_AUDIT_REMEDY.md
├── TODO_AUDIT_REMEDY_CHECKLIST.md
├── TODO_GUIDANCE_SECONDBRAIN.md
├── TODO_GUIDANCE_SECONDBRAIN_CHECKLIST.md
├── TODO_NEW_COMMENTS.md
└── TODO_NEW_COMMENTS_CHECKLIST.md
```
