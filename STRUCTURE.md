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
`{guidance_dir}/.skills/{skill}/SKILL.md`

So if you find a file you're looking for named file.zig:
`file.zig      # [zig-current, gof-patterns] Summary of files' contents` , 
Then you you must read

```
{guidance_dir}/.skills/zig-current/SKILL.md
{guidance_dir}/.skills/gof-patterns/SKILL.md
```

---

## Directory Tree (Git-Tracked Files Only)

```
.
├── bin
│   └── guidance-py
├── doc
│   ├── capabilities
│   │   ├── ast-indexing
│   │   │   └── CAPABILITY.md
│   │   ├── config-system
│   │   │   └── CAPABILITY.md
│   │   ├── embedding-providers
│   │   │   └── CAPABILITY.md
│   │   ├── explain-query
│   │   │   └── CAPABILITY.md
│   │   ├── plugin-system
│   │   │   └── CAPABILITY.md
│   │   ├── sync-pipeline
│   │   │   └── CAPABILITY.md
│   │   └── vector-search
│   │       └── CAPABILITY.md
│   └── DESIGN.md
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
│   │   ├── args.zig                              # Parses command-line arguments into a structured CommonArgs object for configuration and processing.
│   │   ├── cli.zig                               # Defines CLI command structure, registration, and error handling for a Zig-based tool.
│   │   ├── context.zig                           # Manages Zig build context, resolves dependencies, tracks builds, and handles errors.
│   │   ├── format.zig                            # Defines table structure with columns, rows, and formatting logic for JSON rendering.
│   │   ├── hash.zig                              # Provides SHA-256 and content+model hashing utilities for Zig, supporting allocator-friendly outputs and secure key ge...
│   │   ├── interner.zig                          # Manages stable string indices with arena-allocated storage, supporting interned strings and bitmask bitsets.
│   │   ├── io.zig                                # [gof-patterns] Manages buffered I/O for stdout/stdin, ensuring safe writer/filer initialization and preventing dangli...
│   │   ├── json.zig                              # Provides JSON serialization, escaping, and file parsing utilities for Zig tools, supporting allocators and JSON text ...
│   │   ├── json_parser.zig                       # Handles Zig JSON parsing, validates targets, and manages allocators for efficient memory usage.
│   │   ├── llm.zig                               # Contains utility modules, state definitions, and configuration for Zig AST parsing and LLM integration.
│   │   ├── log.zig                               # Defines logging levels, formatting, and configuration for a Zig application with color support.
│   │   ├── reflection.zig                        # Provides field-level reflection, validation, and permission handling for data structures, ensuring safe access and ro...
│   │   ├── registry.zig                          # Manages Zig target registry with allocator, interners, targets, bit index mapping, and provider lists.
│   │   ├── repl.zig                              # Implements a Zig REPL interface handling commands, parsing input and managing stdout/stdin streams.
│   │   ├── resolver.zig                          # [gof-patterns] Manages Zig dependency resolution with topological sorting, handling abstract and concrete targets via...
│   │   ├── source.zig                            # Extracts Zig source excerpts based on node type and line limits for documentation and LLM use.
│   │   ├── str.zig                               # Provides utility functions to detect code identifiers, test paths, and extract skill names from Zig source files.
│   │   ├── string.zig                            # Contains utility functions to check substring presence in Zig bytecode, supporting case-insensitive matching and keyw...
│   │   ├── target.zig                            # [gof-patterns] Defines target execution types, manages WASM/executor lifecycle, and handles dynamic bit manipulation ...
│   │   ├── terminal.zig                          # Handles terminal size, width, height, and user interaction in a Zig terminal context.
│   │   └── url.zig                               # Validates URLs as HTTPS or localhost/127.0.0.1, ensuring safe API endpoint checks.
│   └── guidance
│       ├── plugins
│       │   ├── markdown_plugin.zig               # MarkdownPlugin — extracts sections and metadata from Markdown files.
│       │   └── zig_plugin.zig                    # ZigPlugin — wraps ast_parser.zig as a LanguagePlugin.
│       ├── vector
│       │   ├── embeddings.zig                    # [gof-patterns]  Embedding providers — convert text to vectors for semantic search.
│       │   ├── math.zig                          # Vector operations — cosine similarity, normalization, hybrid merge.
│       │   └── root.zig                          # guidance vector module — cosine search, embeddings, hybrid merge.
│       ├── ast_parser.zig                          # Parses Zig AST, extracts member signatures, and manages memory for the parser.
│       ├── config.zig                              # [gof-patterns] Defines configuration paths for guidance system using precomputed absolute routes across project and u...
│       ├── deps.zig                                # Extracts dependency information from Zig source files, building a map of module paths and their imports.
│       ├── enhancer.zig                            # Zig enhancement enhancer for generating concise docstrings via LLM, optimizing comments and tags.
│       ├── git.zig                                 # Manages Gitignore patterns, loads from files, and handles exclusions for Zig projects.
│       ├── hash.zig                                # Implements SHA-256 hashing and struct hashing utilities for Zig code, generating hex digests and ensuring determinist...
│       ├── json_store.zig                          # Manages Zig guidance parsing, stores content, and tracks leaked prompts for cleanup.
│       ├── lance_db.zig                            # guidance LanceDB-style vector search database.
│       ├── llm_filter.zig                          # llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
│       ├── main.zig                                # [gof-patterns]  guidance — AST-guided LanceDB vector search database generator.
│       ├── marker.zig                              # Mtime-based change detection for guidance's incremental RALPH loop.
│       ├── pattern.zig                             # [gof-patterns] Detects design patterns in Zig AST nodes using text heuristics and node metadata.
│       ├── plugin.zig                              # LanguagePlugin — interface for language-specific AST providers.
│       ├── plugin_registry.zig                     # PluginRegistry — maps file extensions to LanguagePlugin descriptors.
│       ├── provider_discovery.zig                  # External language provider discovery for guidance.
│       ├── staged.zig                              # staged.zig — Staged explain pipeline for `guidance explain`.
│       ├── structure.zig                           # Generates structured Markdown from Zig project directories, merging new comments with existing ones.
│       ├── sync.zig                                # Handles Zig file parsing, AST processing, and supports comment stripping, storage, and optional LLM-enhanced document...
│       ├── synthesize.zig                          # synthesize.zig — LLM-based synthesis for the staged explain pipeline.
│       ├── tests.zig                               # [gof-patterns] Tests JSON store merge, sync, config, and commit helpers in Zig guidance.
│       ├── triage.zig                              # Generates TRIAGE.md from TODO.md using lifecycle detection, risk assessment, and checklist steps.
│       └── types.zig                               # Defines file type classification for Zig source files, mapping extensions and patterns to predefined types for proces...
├── vendor
│   └── sqlite3
│       ├── sqlite3.c
│       ├── sqlite3.h
│       └── sqlite3ext.h
├── AGENTS.md
├── build.zig
├── build.zig.zon
├── GUIDANCE_LANCEDB.md
├── LICENSE
├── LICENSE-Commercial-Requirement
├── LICENSE-Contributor-Agreement
├── Makefile
├── mise.toml
├── pyproject.toml
├── README.md
├── REFACTOR.md
├── requirements.txt
├── ROADMAP_OPTIMIZE_VECTOR_SEARCH.md
├── STRUCTURE.md
└── TEST_EXPLAIN_PROMPT.md
```
