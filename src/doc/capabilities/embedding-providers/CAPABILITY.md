---
name: embedding-providers
description: Pluggable embedding provider trait that converts text to dense float vectors for semantic search. Supports Ollama (local), OpenAI-compatible APIs, and a no-op keyword-only fallback.
anchors:
  - EmbeddingProvider
  - create_embedding_provider
  - OllamaEmbedding
  - OpenAiEmbedding
  - NoopEmbedding
  - BatchEmbedding
---

# Embedding Providers

A `dyn EmbeddingProvider: Send + Sync` trait that decouples vector search from the underlying embedding model, allowing the codebase to work offline (keyword-only) or with any OpenAI-compatible embedding API.

## Providers

| Name | Type | Default model | Dimensions |
|------|------|---------------|------------|
| `ollama` | Local HTTP | `nomic-embed-text` | 768 |
| `openai` | HTTPS API | `text-embedding-3-small` | 1536 |
| `ollama:<model>` | Ollama with custom model | configurable | configurable |
| `custom:<url>` | OpenAI-compatible | configurable | configurable |
| `none` | No-op | — | 0 (keyword fallback) |

## Key files

- `common/src/embeddings.rs` — `EmbeddingProvider` trait, `OllamaEmbedding`, `OpenAiEmbedding`, `NoopEmbedding`, `BatchEmbedding`, `create_embedding_provider`

## Semantic Deviations

- **`dyn Trait` + `Box`** replaces Zig's explicit `{ptr, vtable}` struct pattern — Rust's `Box<dyn EmbeddingProvider>` is the idiomatic trait-object dispatch
- **`Send + Sync` bounds** replace Zig's `thread_id` assertions — the Rust trait requires `Send + Sync` for safe multi-threaded use instead of runtime thread-ID checks
- **`ureq` HTTP** replaces `std.http.Client` — synchronous blocking HTTP via the `ureq` crate (no async runtime needed for embedding calls)
- **`serde_json`** replaces `std.json` — JSON serialization/deserialization for request/response bodies
- **`thiserror`** replaces Zig error unions — `EmbeddingError` is a typed error enum with `#[derive(thiserror::Error)]`
- **`lazy_static` regex** not present — the Rust version doesn't use content-hash caching; URL validation delegates to `validate_https_or_local_http` in `common/src/url.rs`
- **No `content_hash_with_model`** — the Rust version does not cache embedding results; each call hits the API

## Example

```rust
use guidance_common::embeddings::{create_embedding_provider, EmbeddingProvider};

let provider = create_embedding_provider(
    "ollama",
    Some("nomic-embed-text"),
    Some("http://localhost:11434"),
    None,
    768,
)
.unwrap();

assert_eq!(provider.name(), "ollama");
let vec = provider.embed("hello world").unwrap();
assert_eq!(vec.len(), 768);
```

## Zig reference

See `../doc/capabilities/embedding-providers/CAPABILITY.md` in the Zig guidance source tree for the original vtable-based `EmbeddingProvider` with content-hash caching.
