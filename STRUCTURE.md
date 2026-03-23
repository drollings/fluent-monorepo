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
│   │   ├── embeddings.zig
│   │   ├── format.zig
│   │   ├── hash.zig
│   │   ├── interner.zig
│   │   ├── io.zig
│   │   ├── json.zig
│   │   ├── json_parser.zig
│   │   ├── llm.zig
│   │   ├── local_model.zig
│   │   ├── log.zig
│   │   ├── registry.zig
│   │   ├── repl.zig
│   │   ├── resolver.zig
│   │   ├── root.zig
│   │   ├── source.zig
│   │   ├── str.zig
│   │   ├── string.zig
│   │   ├── target.zig
│   │   ├── terminal.zig
│   │   └── url.zig
│   ├── coral
│   │   ├── batch.zig
│   │   ├── cache.zig
│   │   ├── cli.zig
│   │   ├── config.zig
│   │   ├── context_node_schema.zig
│   │   ├── db.zig
│   │   ├── main.zig
│   │   ├── mcp.zig
│   │   ├── pattern.zig
│   │   ├── schema.zig
│   │   ├── scrub.zig
│   │   ├── targets.zig
│   │   ├── triage.zig
│   │   └── verify.zig
│   ├── guidance
│   │   ├── plugins
│   │   │   ├── markdown_plugin.zig
│   │   │   └── zig_plugin.zig
│   │   ├── ast_parser.zig
│   │   ├── comment_cache.zig
│   │   ├── comment_checker.zig
│   │   ├── comment_inserter.zig
│   │   ├── comment_parser.zig
│   │   ├── comment_sync.zig
│   │   ├── config.zig
│   │   ├── deps.zig
│   │   ├── enhancer.zig
│   │   ├── git.zig
│   │   ├── hash.zig
│   │   ├── header_generator.zig
│   │   ├── json_store.zig
│   │   ├── line_verify.zig
│   │   ├── llm_filter.zig
│   │   ├── main.zig
│   │   ├── marker.zig
│   │   ├── pattern.zig
│   │   ├── plugin.zig
│   │   ├── plugin_registry.zig
│   │   ├── provider_discovery.zig
│   │   ├── staged.zig
│   │   ├── structure.zig
│   │   ├── sync.zig
│   │   ├── synthesize.zig
│   │   ├── tests.zig
│   │   ├── triage.zig
│   │   └── types.zig
│   ├── llm
│   │   └── root.zig
│   ├── ontology
│   │   ├── inference.zig
│   │   ├── mapper.zig
│   │   ├── migration.zig
│   │   ├── root.zig
│   │   └── yago.zig
│   ├── rdf
│   │   ├── lexer.zig
│   │   ├── normalize.zig
│   │   ├── nquads.zig
│   │   ├── parser.zig
│   │   └── root.zig
│   ├── reflection
│   │   ├── accessor.zig
│   │   ├── binary.zig
│   │   ├── constraint.zig
│   │   ├── enum_registry.zig
│   │   ├── permissions.zig
│   │   ├── root.zig
│   │   └── typed.zig
│   ├── vector
│   │   ├── lance_db.zig
│   │   ├── math.zig
│   │   └── root.zig
│   └── wasm
│       └── wasm.zig
├── vendor
│   └── sqlite3
│       ├── sqlite3.c
│       ├── sqlite3.h
│       └── sqlite3ext.h
├── AGENTS.md
├── build.zig
├── build.zig.zon
├── CLAUDE.md
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
