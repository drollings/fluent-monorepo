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

- `src/vector/vector_db.zig` — `GuidanceDb`, `vectorSearch`, `keywordSearch`, `hybridSearch`
- `src/vector/math.zig` — `cosineSimilarity`, `vecToBytes`, `bytesToVec`, `hybridMerge`
- `src/common/embeddings.zig` — `EmbeddingProvider` vtable (moved from `src/vector/` in P1.3)
- `src/vector/vector_db.zig` — `SemanticAliases`, `loadSemanticAliases`, `DbSyncBuilder.withAliases`
- `src/vector/root.zig` — re-exports `SemanticAliases`, `loadSemanticAliases`

## Semantic alias expansion

`SemanticAliases` maps query tokens to synonyms before the search, broadening recall without degrading precision. Aliases are loaded from a JSON file:

```zig
const aliases = try loadSemanticAliases(allocator, ".guidance/semantic-aliases.json");
// aliases: ?SemanticAliases — null if file absent
```

`SemanticAliases.expandTokens()` deduplicates tokens (case-insensitive), inserting alias values after each matched token. The expanded token set is used in both vector and keyword search paths.

`DbSyncBuilder` exposes `withAliases(SemanticAliases)` to pass pre-loaded aliases into `syncDatabase`.

## Performance notes

- Embedding cache (`embedding_cache` table) avoids redundant API calls on re-sync
- `test_decl` nodes are excluded from vector scan to save similarity budget
- Partial index `WHERE embedding IS NOT NULL` keeps the scan fast
