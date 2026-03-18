---
name: fts5-search
description: Legacy BM25 full-text search backend using SQLite FTS5. Indexes name, comment, module, and signature fields from AST nodes into .explain.db. Superseded by LanceDB vector search.
---

# FTS5 Search (Legacy)

The original search backend for guidance, using SQLite's FTS5 extension to provide BM25-ranked full-text search over AST node metadata.

## Status: Legacy

FTS5 search is deprecated in favour of the LanceDB vector search backend (`.guidance.db`). It remains functional for backward compatibility and as a fallback when no embedding provider is configured.

## How it works

1. `guidance gen` walks `.guidance/src/` JSON files and upserts rows into `ast_nodes`
2. Triggers maintain an external-content FTS5 virtual table `fts_search` over `(name, comment, module, signature)`
3. `guidance explain <query>` tokenises the query, strips stop words, builds a BM25 query, and ranks results by `bm25(fts_search)`

## Limitations vs. LanceDB

| Feature | FTS5 | LanceDB |
|---------|------|---------|
| Semantic understanding | None — token match only | Vector embeddings |
| "how does X work?" queries | Poor | Good |
| Exact name lookup | Excellent | Good (hybrid) |
| Offline operation | Always | Requires embedding provider |
| Setup | Zero config | Requires Ollama or API key |

## Key files

- `src/guidance/db.zig` — `ExplainDb`, `syncDatabase`, `search` (BM25)
- `.explain.db` — The FTS5 database file

## Migration to LanceDB

```bash
# Generate .guidance.db alongside .explain.db
guidance gen --db-type=lance

# Use LanceDB for queries
guidance explain --db-type=lance "how does sync work"

# Or set in config to always generate both
# guidance-config.json: "enable_guidance_db": true
```
