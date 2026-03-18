---
name: vector-search
description: Cosine similarity search over AST node embeddings stored in .guidance.db, enabling natural-language queries that find semantically related code even without exact keyword matches.
---

# Vector Search

Semantic codebase search using dense vector embeddings and cosine similarity, implemented on top of SQLite with BLOB storage.

## Architecture

```
Query text
  → EmbeddingProvider.embed()
  → query vector ([]f32)
  → cosine similarity against all stored ast_nodes.embedding blobs
  → top-K by score
  → node_type reranking
  → SearchResult[]
```

## Search modes

| Mode | When used | Algorithm |
|------|-----------|-----------|
| `vectorSearch` | Embedder available, query has embedding | Cosine similarity, in-memory top-K over up to 2000 nodes |
| `keywordSearch` | Noop embedder or embedding fails | Multi-token SQL LIKE on name, comment, module, signature |
| `hybridSearch` | Default when embedder present | Weighted fusion of vector (0.65) + keyword (0.35) scores via RRF |

## Embedding text format

For each AST node, embedding text is constructed as prose to maximize semantic match:

```
"<module path as prose> — <node_type> <name>: <doc comment>. Parameters: <param names>. Returns: <return hint>."
```

Example: `"guidance database module — function syncDatabase: synchronises the SQLite database with JSON source files. Parameters: allocator, guidance_dir, db_path."`

## Key files

- `src/guidance/lance_db.zig` — `GuidanceDb`, `vectorSearch`, `keywordSearch`, `hybridSearch`
- `src/guidance/vector/math.zig` — `cosineSimilarity`, `vecToBytes`, `bytesToVec`, `hybridMerge`
- `src/guidance/vector/embeddings.zig` — `EmbeddingProvider` vtable

## Performance notes

- Embedding cache (`embedding_cache` table) avoids redundant API calls on re-sync
- `test_decl` nodes are excluded from vector scan to save similarity budget
- Partial index `WHERE embedding IS NOT NULL` keeps the scan fast
