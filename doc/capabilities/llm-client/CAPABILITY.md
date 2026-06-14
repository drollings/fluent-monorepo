---
name: llm-client
description: Minimal LLM HTTP client (LlmClient) supporting OpenAI-compatible chat-completion endpoints via reqwest. Provides context packing (ContextPacker) for token-budget management and regex-based PII anonymization with 12 patterns.
anchors:
  - LlmClient
  - LlmError
  - ChatMessage
  - LlmConfig
  - ContextPacker
  - anonymize
  - LlmRequestQueue
---

# LLM Client

`llm/src/client.rs` exports `LlmClient`, a thin HTTP wrapper over OpenAI-compatible `/v1/chat/completions` endpoints using `reqwest::blocking::Client`. The `ContextPacker` handles token estimation and truncation, and `anonymize` strips PII from context before it is sent to frontier LLMs.

## LlmClient

```rust
let client = LlmClient::new("http://localhost:11434/v1", "llama3");
let response = client.chat_complete(&[
    ChatMessage { role: "system".into(), content: "You are a helpful assistant.".into() },
    ChatMessage { role: "user".into(), content: "Hello!".into() },
])?;
```

### Construction

```rust
// Basic
let client = LlmClient::new(api_base, model);

// With queue for backpressure
let client = LlmClient::with_queue(api_base, model, queue);

// From LlmConfig
let client = LlmClient::with_config(config);
```

### LlmConfig

```rust
use llm::client::{LlmClient, LlmConfig};

let config = LlmConfig::new()
    .api_url("http://localhost:11434/v1".into())
    .model("llama3".into())
    .think(Some(true))     // optional: enable reasoning model mode
    .timeout_ms(2000)      // default: 2000ms
    .debug(false)
    .show_prompts(false)
    .build();

let client = LlmClient::with_config(config);
```

## Queue-based execution

`LlmClient::chat_complete()` delegates to `LlmRequestQueue` for bounded MPMC concurrency. A `DefaultQueue` singleton creates a 2-thread tokio runtime with the queue if none is provided:

```rust
// Preferred: queue-based (backpressure, retry)
let client = LlmClient::with_queue(api_base, model, queue);
let response = client.chat_complete(&messages)?;

// Fallback: direct HTTP (no queue)
// When no queue is set, uses DefaultQueue singleton
```

## ContextPacker

A token-budget manager that estimates tokens at ~¼ of byte length and truncates context with a `"..."` suffix when it exceeds `max_tokens`:

```rust
use llm::context_packer::ContextPacker;

let packer = ContextPacker::new(4000);
let tokens = ContextPacker::estimate_tokens("hello world");  // ~3
let truncated = packer.truncate_to_budget(long_text);
```

## Anonymization

`anonymize()` applies 12 regex patterns in sequence to redact PII:

| Pattern | Replacement |
|---------|-------------|
| Email addresses | `[EMAIL]` |
| Credit card numbers | `[CREDIT_CARD]` |
| US SSN | `[SSN]` |
| UK NINO | `[NINO]` |
| Canadian SIN | `[SIN]` |
| Bearer tokens | `[BEARER_TOKEN]` |
| AWS access keys | `[AWS_KEY]` |
| Generic API keys (32+ chars) | `[API_KEY]` |
| IPv6 addresses | `[IPv6]` |
| IPv4 addresses | `[IP_ADDRESS]` |
| US phone numbers | `[PHONE]` |
| API key assignments (`key=value`) | `[REDACTED]` |

```rust
use llm::anonymize::anonymize;

let safe = anonymize("Contact user@example.com from 192.168.1.1");
// "Contact [EMAIL] from [IP_ADDRESS]"
```

Uses `std::sync::LazyLock` (not `lazy_static`) for one-time regex compilation.

## Key files

- `llm/src/client.rs` — `LlmClient`, `LlmConfig`, `ChatMessage`, `LlmError`, `chat_complete()`, `chat_complete_http()`
- `llm/src/context_packer.rs` — `ContextPacker`, `estimate_tokens()`, `truncate_to_budget()`, `pack_context()`
- `llm/src/anonymize.rs` — `anonymize()` function with `LazyLock` regex patterns
- `llm/src/embeddings.rs` — `EmbeddingProvider` trait, `OllamaEmbedding`, `OpenAiEmbedding`, `NoopEmbedding`, `BatchEmbedding`, `create_embedding_provider`
- `llm/src/llm_queue.rs` — `LlmRequestQueue`, `LlmTask`, `LlmQueueConfig`

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| HTTP client | `std.http.Client` | `reqwest::blocking::Client` |
| Async support | N/A | `tokio` runtime with `LlmRequestQueue` for backpressure |
| Config | `LlmConfig` with `extractModelName()` | `LlmConfig` (bon::Builder) with `model()` returning raw string |
| PII patterns | 11 patterns | 12 patterns (adds credit card, NINO, SIN, AWS key, generic API key, IPv6) |
| Think mode | `stripThinkBlock`, `isMalformedResponse` | `think` field in `LlmConfig`, reasoning_content extraction |
| Error handling | Error unions | `thiserror`-derived `LlmError` enum |
| Queue | Thread pool | `LlmRequestQueue` wrapping `EventQueue<LlmTask>` with retry policies |

## Zig reference

See `../doc/capabilities/llm-client/CAPABILITY.md` in the Zig guidance source tree for the original `LlmClient`, `LlmConfig`, `stripThinkBlock`, `isMalformedResponse`, and the full PII anonymization implementation.
