---
name: explain-query
description: Natural language codebase query engine that uses LanceDB hybrid vector+keyword search with LLM synthesis to answer questions about how code works, surfacing relevant source locations, skills, and capabilities.
---

# Explain Query

The `guidance explain` command answers natural-language questions about the codebase by searching the vector index, collecting skill and capability excerpts, and optionally synthesizing an LLM answer.

## Pipeline (staged mode, default)

```
1. Expand query via SemanticAliases (if .guidance/semantic-aliases.json present)
2. Search .guidance.db for relevant AST nodes (hybrid vector + keyword)
3. Load skill excerpts from skills[] referenced in matching guidance JSON
4. Load capability excerpts from capabilities[] in matching guidance JSON
5. Load source excerpts (30 lines around each match)
6. LLM synthesis (unless --no-llm): produces a structured answer with
   file:line citations, relevant functions, and a brief explanation
7. Render: text/JSON/markdown output
```

## Semantic alias expansion

`SemanticAliases` maps query tokens to expanded synonyms at search time. For example, `"sync"` → `["synchronise", "sync", "update"]`. Aliases are loaded from `.guidance/semantic-aliases.json` by `loadSemanticAliases()` and passed to `executeStagedWithAliases()`. If the file does not exist, aliases are silently skipped.

```bash
guidance gen --aliases   # generate semantic-aliases.json from guidance JSON
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

- `src/guidance/main.zig` — `cmdExplain`, `cmdExplainStaged`, `renderExplainOutput`, `loadAliases`
- `src/guidance/staged.zig` — `executeStaged`, `executeStagedWithAliases`, `formatStaged`
- `src/vector/vector_db.zig` — `GuidanceDb`, `SemanticAliases`, `loadSemanticAliases`
- `src/vector/root.zig` — re-exports `SemanticAliases`, `loadSemanticAliases`
- `src/common/embeddings.zig` — `EmbeddingProvider` vtable