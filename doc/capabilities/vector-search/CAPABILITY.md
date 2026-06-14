---
name: vector-search
description: Semantic codebase search using dense vector embeddings and cosine similarity over SQLite-stored AST node embeddings. Hybrid search fuses vector (0.65) and keyword (0.35) scores via RRF. QuantizedEmbedding provides int8 compression for memory-constrained deployments.
anchors:
  - GuidanceDb
  - vector_search
  - keyword_search
  - hybrid_search
  - cosine_similarity
  - QuantizedEmbedding
  - cosine_similarity_q8
  - SemanticAliases
  - expand_query
---

# Vector Search

Semantic codebase search using dense vector embeddings and cosine similarity, implemented in the standalone `search-vector` crate (`guidance-search-vector`) on top of SQLite (`rusqlite`) with BLOB storage.

## Architecture

```
Query text
  → (optional) SemanticAliases::expand_query()
  → EmbeddingProvider → query vector (Vec<f32>)
  → GuidanceDb::vector_search(query_vec, k)
  → cosine similarity against all stored embedding blobs
  → top-K by score → SearchResult[]
```

## Search modes

| Mode | Method | Algorithm |
|------|--------|-----------|
| `vector_search` | `GuidanceDb::vector_search` | Cosine similarity, in-memory top-K over all embedded nodes |
| `keyword_search` | `GuidanceDb::keyword_search` | SQL `LIKE` on name, signature, comment |
| `hybrid_search` | `GuidanceDb::hybrid_search` | RRF fusion: vector (0.65) + keyword (0.35) weights |

## Cosine similarity

```rust
// search-vector/src/math.rs
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot_product = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot_product += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let magnitude = norm_a.sqrt() * norm_b.sqrt();
    if magnitude == 0.0 { return 0.0; }
    dot_product / magnitude
}
```

## Quantized embeddings

`QuantizedEmbedding` compresses `Vec<f32>` to `Vec<i8>` with a scale factor (4x memory reduction):

```rust
use guidance_search_vector::QuantizedEmbedding;

let original = vec![0.5, -0.3, 0.8, -0.1, 0.0, 1.0, -1.0];
let q = QuantizedEmbedding::from_f32(&original);
let restored = q.to_f32();

// Q8 cosine similarity (i64 arithmetic, no float sqrt in loop)
let sim = QuantizedEmbedding::cosine_similarity_q8(&q_a, &q_b);
```

## Semantic alias expansion

```rust
use guidance_search_vector::SemanticAliases;

let json = r#"{"fn": ["function", "func"], "arg": ["argument", "param"]}"#;
let aliases = SemanticAliases::from_json(json)?;

// Single token
let expanded = aliases.expand("fn");
assert!(expanded.contains(&"function".to_string()));

// Multi-token with cartesian product
let queries = aliases.expand_query("fn arg");
// Yields: "fn arg", "fn argument", "function arg", "function argument", ...
```

## Example

```rust
use guidance_search_vector::GuidanceDb;

let db = GuidanceDb::open_in_memory()?;

// Insert nodes
db.insert_node("hello", "src/test.zig", Some("fn hello() void"), Some("Says hello"), "test", "zig", Some(&embedding))?;

// Search
let query_vec = vec![0.5, 1.5, 2.5, 3.5];
let results = db.vector_search(&query_vec, 5)?;
for r in &results {
    println!("{:.4} {}", r.similarity, r.name);
}

// Keyword fallback
let results = db.keyword_search("hello")?;

// Hybrid fusion
let results = db.hybrid_search("hello function", Some(&query_vec), 5)?;
```

## Key files

- `search-vector/src/db.rs` — `GuidanceDb`, `SearchResult`, `vector_search`, `keyword_search`, `hybrid_search`, `insert_node`, `sync_from_dir`
- `search-vector/src/math.rs` — `cosine_similarity`, `vec_to_bytes`, `bytes_to_vec`, `QuantizedEmbedding`
- `search-vector/src/aliases.rs` — `SemanticAliases`, `expand`, `expand_query`
- `search-vector/src/error.rs` — `DbError`
- `search-vector/src/lib.rs` — Re-exports: `GuidanceDb`, `SemanticAliases`, `QuantizedEmbedding`

## Dependencies

- `rusqlite` (bundled SQLite) for database
- `guidance-types` for shared types

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| Crate | `src/vector/` module in guidance | Standalone `guidance-search-vector` crate |
| SQLite bindings | `zqlite` wrapper | `rusqlite` crate |
| Embedding storage | BLOB via `zqlite` | BLOB via `rusqlite params![]` |
| Cosine similarity | `math.cosineSimilarity` (same algo) | `math::cosine_similarity` (same algo) |
| Quantized type | `QuantizedEmbedding` (allocator-based) | `QuantizedEmbedding` (no allocator, owned Vec) |
| Semantic aliases | `SemanticAliases.loadJson()` | `SemanticAliases::from_json()` |
| RRF weights | Vector 0.65, keyword 0.35 | Same weights |
| Embedding cache | `embedding_cache` table | Same schema |

## Zig reference

See `doc/capabilities/vector-search/CAPABILITY.md` in the Zig project for the original module design (DbSyncBuilder, embedding text format, performance notes).
