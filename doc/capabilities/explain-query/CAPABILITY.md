---
name: explain-query
description: Natural language codebase query engine that combines vector/FTS5 search with LLM synthesis to answer questions about how code works, surfacing relevant source locations, skills, and capabilities.
---

# Explain Query

The `guidance explain` command answers natural-language questions about the codebase by searching the index, collecting skill and capability excerpts, and optionally synthesizing an LLM answer.

## Pipeline (staged mode, default)

```
1. Search .guidance.db or .explain.db for relevant AST nodes
2. Load skill excerpts from skills[] referenced in matching guidance JSON
3. Load capability excerpts from capabilities[] in matching guidance JSON  
4. Load source excerpts (30 lines around each match)
5. LLM synthesis (unless --no-llm): produces a structured answer with
   file:line citations, relevant functions, and a brief explanation
6. Render: text/JSON/markdown output
```

## CLI

```bash
# Default (FTS5 BM25 search + LLM synthesis)
guidance explain "how does database sync work"

# Vector/hybrid search via .guidance.db
guidance explain --db-type=lance --db .guidance.db "how does database sync work"

# Skip LLM (fast, structural output only)
guidance explain --no-llm "frobnicate"

# Makefile shorthand
make explain QUERY="how does incremental sync work"
```

## Search backends

| Backend | Flag | Database | Search type |
|---------|------|----------|-------------|
| FTS5 (legacy) | default | `.explain.db` | BM25 full-text |
| LanceDB | `--db-type=lance` | `.guidance.db` | Hybrid vector+keyword |

## Key files

- `src/guidance/main.zig` — `cmdExplain`, `cmdExplainStaged`, `renderExplainOutput`
- `src/guidance/staged.zig` — Staged pipeline with LLM filter and synthesis
- `src/guidance/db.zig` — `ExplainDb` (FTS5 backend, legacy)
- `src/guidance/lance_db.zig` — `GuidanceDb` (vector search backend)
