---
name: llm-client
description: Minimal LLM HTTP client (LlmClient) supporting OpenAI-compatible chat-completion endpoints and Ollama. Handles think-mode toggle for reasoning models, response post-processing (stripThinkBlock, isMalformedResponse), and malformed-response fallback. Used by guidance synthesis, local-model-decomposition, and coral L5 fallback.
anchors:
  - LlmClient
  - LlmConfig
  - LlmError
  - stripThinkBlock
  - isMalformedResponse
---

# LLM Client

`src/llm/root.zig` exports `LlmClient` and `LlmConfig`, a thin HTTP wrapper over OpenAI-compatible `/v1/chat/completions` endpoints. Post-processing helpers live in `src/common/llm.zig` alongside string utilities.

## LlmConfig

```zig
pub const LlmConfig = struct {
    api_url: []const u8,
    model: []const u8,
    think: ?bool = null,    // null=off, true=explicit think, false=suppress think
    timeout_ms: u32 = 10000,
    debug: bool = false,
};
```

Model references use `"provider:model:name"` format (e.g., `"local:code:latest"`). `LlmConfig.extractModelName()` strips the provider prefix.

## Think-mode

The `think` field controls the Ollama `think` parameter:

| Value | Behaviour |
|-------|-----------|
| `null` | Parameter not sent (standard models) |
| `true` | `"think":true` — thinking explicitly enabled |
| `false` | `"think":false` — suppress thinking on a thinking-capable model used in non-thinking slot |

`isThinkingModel()` returns `true` only when `think == true`.

## Response post-processing

`stripThinkBlock(response)` removes `<think>…</think>` preamble that reasoning models emit before the actual answer. `isMalformedResponse(text)` detects non-JSON, empty arrays, and residual think-block garbage — used by `LocalDecomposer` before parsing the subtask array.

## Usage patterns

```zig
var client = try LlmClient.init(allocator, config);
defer client.deinit();
const raw = try client.complete(prompt, max_tokens, temperature, system_prompt);
defer allocator.free(raw);
const clean = stripThinkBlock(raw);
```

## Key files

- `src/llm/root.zig` — `LlmError`, `LlmConfig`, `LlmClient`
- `src/common/llm.zig` — `stripThinkBlock`, `isMalformedResponse`, `parseJsonArray`, string helpers
- `src/common/local_model.zig` — primary consumer (`LocalDecomposer`)
- `src/guidance/staged.zig` — guidance synthesis consumer

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (9 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/llm/root.zig` | 1.0 | defines_anchor |
| `src/llm/llm.zig` | 1.0 | defines_anchor |
| `src/common/local_model.zig` | 1.0 | defines_anchor |
| `src/vector/vector_db.zig` | 0.9 | used_by |
| `src/llm/context_packer.zig` | 0.4 | path_heuristic |
| `src/guidance/llm_filter.zig` | 0.4 | path_heuristic |
| `src/guidance/llm_filter_batch.zig` | 0.4 | path_heuristic |
| `src/llm/token_budget.zig` | 0.4 | path_heuristic |
| `src/llm/context_compressor.zig` | 0.4 | path_heuristic |

