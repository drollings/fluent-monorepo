---
name: embedding-providers
description: Pluggable embedding provider system that converts text to dense float vectors for semantic search. Supports Ollama (local), OpenAI-compatible APIs, and a no-op keyword-only fallback.
---

# Embedding Providers

A vtable-based interface (`EmbeddingProvider`) that decouples vector search from the underlying embedding model, allowing the codebase to work offline (keyword-only) or with any OpenAI-compatible embedding API.

## Providers

| Name | Type | Default model | Dimensions |
|------|------|---------------|------------|
| `ollama` | Local HTTP | `nomic-embed-text` | 768 |
| `openai` | HTTPS API | `text-embedding-3-small` | 1536 |
| `custom:<url>` | OpenAI-compatible | configurable | configurable |
| `none` | No-op | — | 0 (keyword fallback) |

## Configuration

```json
{
  "embedding_provider": "ollama",
  "embedding_model": "nomic-embed-text",
  "embedding_dims": 768
}
```

## Content hash cache

Before calling the embedding API, the provider computes `SHA-256(model_name + "\x00" + text)` and checks `embedding_cache`. This makes incremental re-syncs fast: unchanged nodes are never re-embedded.

## Key files

- `src/common/embeddings.zig` — `EmbeddingProvider` vtable, `OllamaEmbedding`, `OpenAiEmbedding`, `NoopEmbedding`, `createEmbeddingProvider`
- `src/vector/math.zig` — `vecToBytes`, `bytesToVec` (serialization for BLOB storage)

## Security

Plain HTTP is only permitted for localhost/127.x/::1 addresses. Remote embedding endpoints must use HTTPS.
