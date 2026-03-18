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
│   │   ├── io.zig                                # [gof-patterns] Manages buffered I/O for stdout/stdin, ensuring safe writer/freader usage with fixed buffer and no dan...
│   │   ├── llm.zig                               # Handles Zig AST parsing, manages writer/reader states, and processes LLM responses with thinking controls.
│   │   └── source.zig                            # Extracts Zig source excerpts based on node type and line limits for documentation and LLM use.
│   └── guidance
│       ├── plugins
│       │   ├── markdown_plugin.zig               # MarkdownPlugin — extracts sections and metadata from Markdown files.
│       │   └── zig_plugin.zig                    # ZigPlugin — wraps ast_parser.zig as a LanguagePlugin.
│       ├── vector
│       │   ├── embeddings.zig                    # [gof-patterns]  Embedding providers — convert text to vectors for semantic search.
│       │   ├── math.zig                          # Vector operations — cosine similarity, normalization, hybrid merge.
│       │   └── root.zig                          # guidance vector module — cosine search, embeddings, hybrid merge.
│       ├── ast_parser.zig                          # Parses Zig AST, extracts member signatures, and manages memory for the parser.
│       ├── config.zig                              # Defines configuration paths for guidance system using precomputed absolute routes and fallback locations.
│       ├── deps.zig                                # Extracts dependency information from Zig source files, building a map of import paths and their relationships.
│       ├── enhancer.zig                            # Zig enhancement enhancer for generating concise docstrings via LLM, supporting token limits and comment upgrades.
│       ├── gitignore.zig                           # Manages Gitignore patterns, patterns, negations, and project root for file loading and exclusion.
│       ├── hash.zig                                # Implements SHA-256 hashing and signature generation for Zig types, ensuring deterministic output and type normalization.
│       ├── json_store.zig                          # Manages Zig guidance parsing, stores content, tracks leaked prompts, and supports AST reconstruction.
│       ├── lance_db.zig                            # guidance LanceDB-style vector search database.
│       ├── llm_filter.zig                          # llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
│       ├── main.zig                                # [gof-patterns]  guidance — AST-guided LanceDB vector search database generator.
│       ├── marker.zig                              # Mtime-based change detection for guidance's incremental RALPH loop.
│       ├── pattern.zig                             # [gof-patterns] Analyzes Zig AST nodes to detect design patterns using text heuristics and node metadata.
│       ├── plugin.zig                              # LanguagePlugin — interface for language-specific AST providers.
│       ├── plugin_registry.zig                     # PluginRegistry — maps file extensions to LanguagePlugin descriptors.
│       ├── provider_discovery.zig                  # External language provider discovery for guidance.
│       ├── query.zig                               # Manages memory for Zig AST nodes, freeing resources after processing queries and analysis.
│       ├── staged.zig                              # staged.zig — Staged explain pipeline for `guidance explain`.
│       ├── structure.zig                           # Generates structured Markdown from Zig AST by merging guidance comments with existing file annotations.
│       ├── sync.zig                                # Handles Zig file parsing, AST processing, and comment management for guidance generation.
│       ├── synthesize.zig                          # synthesize.zig — LLM-based synthesis for the staged explain pipeline.
│       ├── tests.zig                               # [gof-patterns] Tests JSON Store merge logic and query engine behavior in Zig guidance.
│       ├── triage.zig                              # Generates TRIAGE.md from a TODO.md by analyzing files, assessing risk, and outlining steps; tracks lifecycle stages a...
│       ├── types.zig                               # Defines file type classification for Zig source files, mapping extensions and patterns to predefined types for proces...
│       └── utils.zig                               # Extracts and filters Zig source lines up to 80, identifying public declarations.
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
├── STRUCTURE.md
└── TEST_EXPLAIN_PROMPT.md
```
