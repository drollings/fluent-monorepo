---
name: llm-client
description: Minimal LLM HTTP client (LlmClient) supporting OpenAI-compatible chat-completion endpoints. Provides context packing (ContextPacker) for token-budget management and regex-based PII anonymization.
anchors:
  - LlmClient
  - LlmError
  - ChatMessage
  - ContextPacker
  - anonymize
---

# LLM Client

`llm/src/client.rs` exports `LlmClient`, a thin HTTP wrapper over OpenAI-compatible `/v1/chat/completions` endpoints using synchronous `ureq`. The `ContextPacker` handles token estimation and truncation, and `anonymize` strips PII from context before it is sent to frontier LLMs.

## LlmClient

```rust
let client = LlmClient::new("http://localhost:11434/v1", "llama3");
let response = client.chat_complete(&[
    ChatMessage { role: "system".into(), content: "You are a helpful assistant.".into() },
    ChatMessage { role: "user".into(), content: "Hello!".into() },
])?;
```

## ContextPacker

A token-budget manager that estimates tokens at ~¼ of byte length and truncates context with a `"..."` suffix when it exceeds `max_tokens`.

## Anonymization

`anonymize()` applies four regex patterns in sequence to redact PII:

| Pattern | Replacement |
|---------|-------------|
| Email addresses | `[EMAIL]` |
| API keys / secrets | `[REDACTED]` |
| IPv4 addresses | `[IP_ADDRESS]` |
| Phone numbers | `[PHONE]` |

## Key files

- `llm/src/client.rs` — `LlmClient`, `ChatMessage`, `LlmError`, `chat_complete()`
- `llm/src/context_packer.rs` — `ContextPacker`, `estimate_tokens()`, `truncate_to_budget()`, `pack_context()`
- `llm/src/anonymize.rs` — `anonymize()` function with `lazy_static` regex patterns
- `llm/src/embeddings.rs` — `EmbeddingProvider` trait, `OllamaEmbedding`, `OpenAiEmbedding`, `NoopEmbedding`, `BatchEmbedding`, `create_embedding_provider`

## Semantic Deviations

- **`async-openai` not used** — the Rust `LlmClient` uses synchronous `ureq` for HTTP calls, not the `async-openai` crate; the user spec mentions `async-openai` but the actual implementation uses `ureq`
- **No `LlmConfig` struct** — configuration is passed directly as `api_base` + `model` strings to `LlmClient::new()`
- **No `think` mode** — the Rust client does not support the Ollama `think` parameter or reasoning-model post-processing (`stripThinkBlock`, `isMalformedResponse`)
- **`ChatMessage` is a plain struct** — not Zig's tagged union pattern; serialized via `serde_json`
- **`anonymize` is a free function** — replaces Zig's `anonymizeContext(allocator, raw_context, &.{...patternSelection})`; uses `regex::Regex` via `lazy_static` instead of Zig's `std.mem` scanning
- **Simpler PII patterns** — only 4 patterns vs. Zig's 11; no credit card, SSN, Bearer token, AWS key, or generic token patterns
- **No `LlmConfig.extractModelName()`** — `model()` returns the string as-is

## Example

```rust
use llm::client::{LlmClient, ChatMessage};

let client = LlmClient::new("http://localhost:11434/v1", "llama3");
let response = client.chat_complete(&[
    ChatMessage {
        role: "user".into(),
        content: "Say hello in one word.".into(),
    },
]);
assert!(response.is_ok());
```

## Zig reference

See `../doc/capabilities/llm-client/CAPABILITY.md` in the Zig guidance source tree for the original `LlmClient`, `LlmConfig`, `stripThinkBlock`, `isMalformedResponse`, and the full 11-pattern `anonymizeContext` implementation.
