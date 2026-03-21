---
name: explain-query
description: Natural language codebase query engine that uses LanceDB hybrid vector+keyword search with LLM synthesis to answer questions about how code works, surfacing relevant source locations, skills, and capabilities.
---

# Explain Query

The `guidance explain` command answers natural-language questions about the codebase by searching the vector index, collecting skill and capability excerpts, and optionally synthesizing an LLM answer.

## Pipeline (staged mode, default)

```
1. Search .guidance.db for relevant AST nodes (hybrid vector + keyword)
2. Load skill excerpts from skills[] referenced in matching guidance JSON
3. Load capability excerpts from capabilities[] in matching guidance JSON  
4. Load source excerpts (30 lines around each match)
5. LLM synthesis (unless --no-llm): produces a structured answer with
   file:line citations, relevant functions, and a brief explanation
6. Render: text/JSON/markdown output
```

## CLI

```bash
# Default (hybrid vector + keyword search + LLM synthesis)
guidance explain "how does database sync work"

# Skip LLM (fast, structural output only)
guidance explain --no-llm "frobnicate"

# Makefile shorthand
make explain QUERY="how does incremental sync work"
```

## Search Backend

| Backend | Database | Search type |
|---------|----------|-------------|
| LanceDB (default) | `.guidance.db` | Hybrid vector + keyword |

The hybrid search combines:
- **Vector search**: Cosine similarity on embeddings stored as BLOBs in SQLite
- **Keyword search**: SQL LIKE queries (fallback when no embedding provider)
- **Weighted fusion**: 65% vector / 35% keyword scores

## Key files

- `src/guidance/main.zig` — `cmdExplain`, `cmdExplainStaged`, `renderExplainOutput`
- `src/guidance/staged.zig` — Staged pipeline with LLM filter and synthesis
- `src/vector/lance_db.zig` — `GuidanceDb` (hybrid vector + keyword search)
- `src/vector/` — Embedding providers and cosine similarity math