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
├── doc
│   ├── capabilities
│   │   ├── ast-indexing
│   │   ├── config-system
│   │   ├── coral-cache
│   │   ├── coral-database
│   │   ├── coral-ingestion
│   │   ├── coral-mcp
│   │   ├── embedding-providers
│   │   ├── explain-query
│   │   ├── llm-client
│   │   ├── local-model-decomposition
│   │   ├── ontology
│   │   ├── plugin-system
│   │   ├── rdf-parsing
│   │   ├── reflection
│   │   ├── sync-pipeline
│   │   ├── target-registry
│   │   ├── vector-search
│   │   └── wasm-tools
│   ├── coral
│   ├── guidance
│   │   └── schemas
│   ├── prompts
│   ├── reviews
│   └── skills
│       ├── fluent-wvr
│       ├── gof-patterns
│       └── zig-current
├── env
│   ├── mise
│   └── mk
│       └── targets
├── src
│   ├── common
│   │   └── vaxis_stub
│   ├── concurrency
│   ├── coral
│   │   └── algorithms
│   ├── dag
│   ├── guidance
│   │   ├── codehealth
│   │   ├── comments
│   │   ├── core
│   │   ├── plugins
│   │   ├── query
│   │   └── sync
│   ├── legacy_concurrency
│   ├── llm
│   ├── ontology
│   ├── rdf
│   ├── reflection
│   ├── testing
│   ├── vector
│   └── wasm
├── vendor
│   └── sqlite3
└── zig-crt
    ├── libc.a
    ├── libc.so.6
    ├── libdl.a
    ├── libm.a
    ├── libm.so.6
    ├── libpthread.a
    ├── libpthread.so.0
    ├── librt.a
    ├── librt.so.1
    ├── libutil.a
    └── libutil.so.1
```
