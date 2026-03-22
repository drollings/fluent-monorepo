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
│   │   ├── args.zig                                               # Parses command-line arguments into a structured CommonArgs object for configuration handling.
│   │   ├── cli.zig                                                # Defines CLI command structure, registration, and error handling for a Zig-based tool.
│   │   ├── context.zig                                            # Manages Zig build context, resolves dependencies, tracks builds, and handles allocator cleanup.
│   │   ├── embeddings.zig                                         # [gof-patterns]  Embedding providers — convert text to vectors for semantic search.
│   │   ├── format.zig                                             # Defines table structure with columns, rows, and formatting logic for JSON rendering.
│   │   ├── hash.zig                                               # Provides SHA-256 and content+model hashing utilities for Zig, supporting allocator-friendly outputs and secure key ge...
│   │   ├── interner.zig                                           # Manages stable string indices with arena-allocated storage, supporting interned strings and bitmask bitsets.
│   │   ├── io.zig                                                 # [gof-patterns] Manages buffered I/O for stdout/stdin, ensuring safe writer/filer initialization and preventing dangli...
│   │   ├── json.zig                                               # Provides JSON serialization, escaping, and file loading utilities with allocator safety and no external dependencies.
│   │   ├── json_parser.zig                                        # Handles Zig JSON parsing, validates targets, and manages allocators for efficient memory handling.
│   │   ├── llm.zig                                                # common — Shared utilities and LLM client for guidance, vector, and coral.
│   │   ├── local_model.zig                                        # Handles LLM task decomposition, parses JSON arrays, manages sub-task lists with fallbacks.
│   │   ├── log.zig                                                # Defines logging configuration, formatting, and file handling for a Zig application with color support.
│   │   ├── registry.zig                                           # Manages Zig target registry with allocator, interners, targets, bit index mapping, and provider lists.
│   │   ├── repl.zig                                               # Implements a Zig REPL interface handling commands, parsing input and managing stdout/stdin streams.
│   │   ├── resolver.zig                                           # [gof-patterns] Manages Zig dependency resolution with topological sorting, handling abstract and concrete targets via...
│   │   ├── root.zig                                               # common — Module umbrella root.
│   │   ├── source.zig                                             # Extracts Zig source excerpts based on node type and line limits for documentation and LLM use.
│   │   ├── str.zig                                                # Provides utility functions to detect code identifiers, test paths, and extract skill names from Zig AST paths.
│   │   ├── string.zig                                             # Implements string search utilities with case-insensitive matching and keyword checks for a Zig source file.
│   │   ├── target.zig                                             # [gof-patterns, gof-patterns] Defines target execution types, manages WASM/executor lifecycle, and handles dynamic bit...
│   │   ├── terminal.zig                                           # Handles terminal size, width, height, and user interaction in a Zig terminal environment.
│   │   └── url.zig                                                # Validates API URLs as HTTPS or localhost, ensuring safe API calls.
│   ├── coral
│   │   ├── batch.zig                                              # Streaming batch ingestion pipeline for Turtle files, processing triples in configurable batches to CozoDB with memory...
│   │   ├── cache.zig                                              # [gof-patterns] Implements a 5-tier cache hierarchy routing system with L1 to L5 performance tiers and associated algo...
│   │   ├── cli.zig                                                # Manages ingestion CLI arguments, tracks progress, and stores checkpoints in CozoDB.
│   │   ├── config.zig                                             # Defines Coral project config with multi-level path resolution for guidance system, supporting project, user, and defa...
│   │   ├── context_node_schema.zig                                # [gof-patterns] Defines schema structures, payload types, and binary header validation for Coral DB context nodes.
│   │   ├── db.zig                                                 # [gof-patterns, gof-patterns] Defines CozoDB integration for Coral, handling embeddings, graph hydration, LOD selectio...
│   │   ├── main.zig                                               # Handles Zig build configuration, loads JSON config, initializes LLM and registry, and processes user queries.
│   │   ├── mcp.zig                                                # Implements JSON-RPC 2.0 over STDIO for Coral MCP, handling routing, responses, and arena-based execution.
│   │   ├── pattern.zig                                            # [gof-patterns, gof-patterns] Detects design patterns in Zig AST nodes using text heuristics, supporting domain and Go...
│   │   ├── schema.zig                                             # Defines Coral Context schema using CozoDB, integrating payloads, embeddings, and time-travel features with Datalog tr...
│   │   ├── scrub.zig                                              # Detects synthetic or LLM-generated comments in Zig code for re-infilling.
│   │   ├── targets.zig                                            # Defines the YAGO ingestion pipeline with structured target nodes and dependencies for data processing.
│   │   ├── triage.zig                                             # Generates TRIAGE.md from a TODO.md file by analyzing affected paths, assessing risk, and suggesting steps.
│   │   └── verify.zig                                             # This file defines verification logic for Zig data ingestion, tracking errors, warnings, and report metrics using a cu...
│   ├── guidance
│   │   ├── plugins
│   │   │   ├── markdown_plugin.zig                              # MarkdownPlugin — extracts sections and metadata from Markdown files.
│   │   │   └── zig_plugin.zig                                   # ZigPlugin — wraps ast_parser.zig as a LanguagePlugin.
│   │   ├── ast_parser.zig                                         # Parses Zig AST, extracts member signatures, and manages memory for the parser.
│   │   ├── comment_cache.zig
│   │   ├── comment_checker.zig
│   │   ├── comment_inserter.zig
│   │   ├── comment_parser.zig
│   │   ├── comment_sync.zig
│   │   ├── config.zig                                             # [gof-patterns, gof-patterns] Defines configuration paths for guidance system using precomputed absolute routes across...
│   │   ├── deps.zig                                               # Extracts dependency information from Zig source files, building a map of module paths and their imports.
│   │   ├── enhancer.zig                                           # Zig enhancement enhancer for generating concise docstrings via LLM, optimizing comments and tags.
│   │   ├── git.zig                                                # Manages Gitignore patterns, loads from files, and handles exclusions for Zig projects.
│   │   ├── hash.zig                                               # Implements SHA-256 hashing and struct hashing utilities for Zig code, generating hex digests and ensuring determinist...
│   │   ├── header_generator.zig
│   │   ├── json_store.zig                                         # Manages Zig guidance parsing, stores content, and tracks leaked prompts for cleanup.
│   │   ├── line_verify.zig
│   │   ├── llm_filter.zig                                         # llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
│   │   ├── main.zig                                               # [gof-patterns, gof-patterns]  guidance — AST-guided LanceDB vector search database generator.
│   │   ├── marker.zig                                             # Mtime-based change detection for guidance's incremental RALPH loop.
│   │   ├── pattern.zig                                            # [gof-patterns] Detects design patterns in Zig AST nodes using text heuristics and node metadata.
│   │   ├── plugin.zig                                             # LanguagePlugin — interface for language-specific AST providers.
│   │   ├── plugin_registry.zig                                    # PluginRegistry — maps file extensions to LanguagePlugin descriptors.
│   │   ├── provider_discovery.zig                                 # External language provider discovery for guidance.
│   │   ├── staged.zig                                             # staged.zig — Staged explain pipeline for `guidance explain`.
│   │   ├── structure.zig                                          # Generates structured Markdown from Zig project directories, merging new comments with existing ones.
│   │   ├── sync.zig                                               # Handles Zig file parsing, AST processing, and supports comment stripping and enhancement for documentation generation.
│   │   ├── synthesize.zig                                         # synthesize.zig — LLM-based synthesis for the staged explain pipeline.
│   │   ├── tests.zig                                              # [gof-patterns, gof-patterns] Tests JSON store merge, sync, config, and commit helpers in Zig guidance.
│   │   ├── triage.zig                                             # Generates TRIAGE.md from TODO.md using lifecycle detection, risk assessment, and checklist steps.
│   │   └── types.zig                                              # Defines file type classification for Zig source files, mapping extensions and patterns to predefined types for proces...
│   ├── llm
│   │   └── root.zig                                               # llm — General-purpose LLM inference client.
│   ├── ontology
│   │   ├── inference.zig                                          # Defines inference engine stub for RDFS/OWL, handling transitive rules and materialization stubs.
│   │   ├── mapper.zig                                             # Transforms RDF triples into ContextNodes and edges for CozoDB, routing properties via YAGO schema and accumulating no...
│   │   ├── migration.zig                                          # Tracks ontology versions and provides stub migration functions for YAGO schema changes.
│   │   ├── root.zig                                               # Handles ontology processing with YAGO helpers, mapping, migration, and inference.
│   │   └── yago.zig                                               # Defines YAGO 4.5 ontology schema with classes, properties, and registry for structured knowledge representation.
│   ├── rdf
│   │   ├── lexer.zig                                              # This file defines a streaming lexer for Terse RDF Triple Language, handling tokens, line/column tracking, and returni...
│   │   ├── normalize.zig                                          # Normalizes RDF IRI strings to deterministic hashes for CozoDB storage using Blake3, supports scope and blank node has...
│   │   ├── nquads.zig                                             # Parses Zig source code into structured quad structures, supporting terms, literals, and graphs.
│   │   ├── parser.zig                                             # Streaming parser for Zig RDF, efficiently producing triples without full AST storage.
│   │   └── root.zig                                               # Handles RDF parsing, N-Quads processing, and term normalization in Zig code.
│   ├── reflection
│   │   ├── accessor.zig                                           # Defines accessor metadata, type tags, ownership modes, and field descriptions for schema and AI context.
│   │   ├── binary.zig                                             # Encodes/decodes struct fields using BinaryFieldCodec for wire format, supporting integers, floats, booleans, enums, a...
│   │   ├── constraint.zig                                         # Defines a type-safe vtable for constraint values with optional advanced features like context, release, and conversion.
│   │   ├── enum_registry.zig                                      # Manages enum registration, lookup, and deinitialization with efficient index mapping.
│   │   ├── permissions.zig                                        # Defines role-based permissions for Coral Context reflection, mapping six roles to read/write/derive capabilities usin...
│   │   ├── root.zig                                               # This file exports core reflection utilities for validation, access control, and type handling in the Coral codebase.
│   │   └── typed.zig                                              # Defines typed accessor structures, type conversions, and permission handling for Zig type safety.
│   ├── vector
│   │   ├── lance_db.zig                                           # guidance LanceDB-style vector search database.
│   │   ├── math.zig                                               # Vector operations — cosine similarity, normalization, hybrid merge.
│   │   └── root.zig                                               # guidance vector module — cosine search, embeddings, hybrid merge.
│   └── wasm
│       └── wasm.zig                                                 # [gof-patterns, gof-patterns] Implements secure sandboxed WebAssembly execution using dynamic loading, zero-copy IPC, ...
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
├── requirements.txt
├── ROADMAP_MONOREPO_MARCH.md
├── ROADMAP_MONOREPO_MARCH_CHECKLIST.md
├── STRUCTURE.md
├── TEST_EXPLAIN_PROMPT.md
├── TODO.md
├── TODO_GUIDANCE_SECONDBRAIN.md
├── TODO_GUIDANCE_SECONDBRAIN_CHECKLIST.md
├── TODO_NEW_COMMENTS.md
└── TODO_NEW_COMMENTS_CHECKLIST.md
```
