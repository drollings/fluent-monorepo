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
│   │   ├── args.zig
│   │   ├── io.zig                                # [gof-patterns]
│   │   ├── llm.zig
│   │   └── source.zig
│   └── guidance
│       ├── plugins
│       │   ├── markdown_plugin.zig
│       │   └── zig_plugin.zig
│       ├── vector
│       │   ├── embeddings.zig                    # [gof-patterns]
│       │   ├── math.zig
│       │   └── root.zig
│       ├── ast_parser.zig
│       ├── config.zig
│       ├── deps.zig
│       ├── enhancer.zig
│       ├── gitignore.zig
│       ├── hash.zig
│       ├── json_store.zig
│       ├── lance_db.zig
│       ├── llm_filter.zig
│       ├── main.zig                                # [gof-patterns]
│       ├── marker.zig
│       ├── pattern.zig                             # [gof-patterns]
│       ├── plugin.zig
│       ├── plugin_registry.zig
│       ├── provider_discovery.zig
│       ├── query.zig
│       ├── staged.zig
│       ├── structure.zig
│       ├── sync.zig
│       ├── synthesize.zig
│       ├── tests.zig                               # [gof-patterns]
│       ├── triage.zig
│       ├── types.zig
│       └── utils.zig
├── vendor
│   └── sqlite3
│       ├── sqlite3.c
│       ├── sqlite3.h
│       └── sqlite3ext.h
├── AGENTS.md
├── build.zig
├── build.zig.zon
├── EXPLAIN_BENCHMARK.md
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
└── STRUCTURE.md
```
