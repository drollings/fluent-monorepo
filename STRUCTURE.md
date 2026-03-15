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
`.explain-gen/.skills/{skill}/SKILL.md`

So if you find a file you're looking for named file.zig:
`file.zig      # [zig-current, gof-patterns] Summary of files' contents` , 
Then you you must read

```
.explain-gen/.skills/zig-current/SKILL.md
.explain-gen/.skills/gof-patterns/SKILL.md
```

---

## Directory Tree (Git-Tracked Files Only)

```
.
├── bin
│   └── explain-gen-py
├── doc
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
│   │   ├── io.zig
│   │   └── llm.zig
│   └── explain-gen
│       ├── plugins
│       │   ├── markdown_plugin.zig
│       │   └── zig_plugin.zig
│       ├── ast_parser.zig
│       ├── config.zig
│       ├── db.zig
│       ├── deps.zig
│       ├── enhancer.zig
│       ├── gitignore.zig
│       ├── hash.zig
│       ├── json_store.zig                          # [gof-patterns]
│       ├── llm_filter.zig
│       ├── main.zig
│       ├── pattern.zig                             # [gof-patterns]
│       ├── plugin.zig
│       ├── plugin_registry.zig
│       ├── query.zig
│       ├── staged.zig
│       ├── structure.zig
│       ├── sync.zig
│       ├── synthesize.zig
│       ├── tests.zig                               # [gof-patterns]
│       ├── triage.zig
│       ├── types.zig
│       └── utils.zig
├── tests
│   └── explain_integration.sh
├── vendor
│   └── sqlite3
│       ├── sqlite3.c
│       ├── sqlite3.h
│       └── sqlite3ext.h
├── AGENTS.md
├── build.zig
├── build.zig.zon
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
