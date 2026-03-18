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
│   │   ├── args.zig                              # Parses flat args slice into CommonArgs struct; returns sub-slice of args as positional arguments.
│   │   ├── io.zig                                # [gof-patterns] Centralises buffered I/O helpers wrapping std.fs.File with stack-allocated storage to avoid dangling p...
│   │   ├── llm.zig                               # Defines LLM API configuration (Ollama endpoints, model selection, think parameter control) and utility functions for ...
│   │   └── source.zig                            # Extracts source code excerpts from files for LLM context, documentation, and error messages.
│   └── guidance
│       ├── plugins
│       │   ├── markdown_plugin.zig               # MarkdownPlugin — extracts sections and metadata from Markdown files.
│       │   └── zig_plugin.zig                    # ZigPlugin — wraps ast_parser.zig as a LanguagePlugin.
│       ├── vector
│       │   ├── embeddings.zig                    # [gof-patterns]  Embedding providers — convert text to vectors for semantic search.
│       │   ├── math.zig                          # Vector operations — cosine similarity, normalization, hybrid merge.
│       │   └── root.zig                          # guidance vector module — cosine search, embeddings, hybrid merge.
│       ├── ast_parser.zig                          # Zig AST parser extracting public function, variable, and test declarations with signature hashing and pattern detection.
│       ├── config.zig                              # Defines configuration loading paths for guidance systems using precomputed absolute routes and fallback locations.
│       ├── deps.zig                                # Walks directory tree to extract .zig imports via string parsing, building dependency map of source files.
│       ├── enhancer.zig                            # AI Docstring Enhancer for Zig guidance generation. Mirrors Python's AIDocstringEnhancer class in guidance.py. Generat...
│       ├── gitignore.zig                           # Loads .gitignore files into memory, parsing patterns and negations to filter project paths against always-excluded di...
│       ├── hash.zig                                # Generates deterministic SHA256 hashes from API function signatures, struct definitions, and type normalizations using...
│       ├── json_store.zig                          # Loads JSON guidance docs, parses meta/skills/comments, sanitizes leaked LLM prompts via isLeakedPrompt check, stores ...
│       ├── lance_db.zig                            # guidance LanceDB-style vector search database.
│       ├── llm_filter.zig                          # llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
│       ├── main.zig                                # [gof-patterns]  guidance — AST-guided LanceDB vector search database generator.
│       ├── marker.zig                              # Mtime-based change detection for guidance's incremental RALPH loop.
│       ├── pattern.zig                             # [gof-patterns] Detects design patterns from Zig AST nodes using text-based heuristics analogous to the Python Pattern...
│       ├── plugin.zig                              # LanguagePlugin — interface for language-specific AST providers.
│       ├── plugin_registry.zig                     # PluginRegistry — maps file extensions to LanguagePlugin descriptors.
│       ├── provider_discovery.zig                  # External language provider discovery for guidance.
│       ├── query.zig                               # Manages query execution lifecycle, result memory allocation/deallocation via freeQueryResult, and engine state handling.
│       ├── staged.zig                              # staged.zig — Staged explain pipeline for `guidance explain`.
│       ├── structure.zig                           # Walks directory tree, annotates files with guidance JSON comments or preserved STRUCTURE.md entries, outputs Markdown...
│       ├── sync.zig                                # Synchronous processor that parses Zig source files, extracts members via AST, strips comments, and prepares data for ...
│       ├── synthesize.zig                          # synthesize.zig — LLM-based synthesis for the staged explain pipeline.
│       ├── tests.zig                               # [gof-patterns] Unit tests for src/guidance — json_store merge logic, query engine leaks.
│       ├── triage.zig                              # Generates TRIAGE.md from TODO.md work items by detecting affected files via regex, assessing risk deterministically, ...
│       ├── types.zig                               # Defines core data structures (Member, Skill, Meta) for parsing code elements and generating AI guidance documentation.
│       └── utils.zig                               # Extracts source code excerpts around function declarations and performs case-insensitive file grepping with comment f...
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
