---
name: explain-query
description: Natural language codebase query engine that classifies intent, matches identifiers or keywords, and synthesizes structured results with source citations.
anchors:
  - QueryEngine
  - QueryIntent
  - IdentifierPattern
  - Synthesizer
  - Stage
  - detect_identifier_pattern
  - classify_query
---

# Explain Query

The explain-query capability answers natural-language questions about the codebase by classifying query intent, dispatching to the appropriate matching strategy, and synthesizing a structured `Vec<Stage>` result.

## Pipeline

```
1. classify_query(query) → QueryIntent variant
2. dispatch to handler based on intent:
   - IdentifierLookup  → identifier::find_members_by_name / find_members_by_signature
   - CapabilityQuery   → keyword match on member name + comment
   - ConceptQuery      → LLM filter ranking (if available)
   - GeneralSearch     → substring match on name/signature/comment
3. Synthesizer::synthesize → Vec<Stage> (Code + Prose entries)
```

## QueryStrategy as an enum

`QueryIntent` is a `#[derive]` enum with a `matches()` method, not a vtable:

- `IdentifierLookup` — priority 0 (fastest, <100µs)
- `CapabilityQuery` — priority 2 (keyword match over members)
- `ConceptQuery` — priority 4 (LLM filter, ~100ms+)
- `GeneralSearch` — priority 6 (broad substring match)

## Key files

- `guidance/src/query_engine.rs` — `QueryEngine`, `explain()`, `vector_explain()`
- `guidance/src/query/identifier.rs` — `IdentifierPattern`, `IdentifierKind`, `detect_identifier_pattern`, `find_members_by_name`, `find_members_by_signature`
- `guidance/src/query/strategy.rs` — `QueryIntent` enum, `classify_query`, `matches`, `query_strategy_priority`
- `guidance/src/query/synthesize.rs` — `Stage`, `Synthesizer`, `synthesize()`
- `guidance/src/query/llm_filter.rs` — `LlmFilter`, `LlmFilterBackend` (concept query ranking)

## Semantic Deviations

- **`QueryIntent` enum** replaces Zig's `QueryStrategy` vtable — dispatch is a match arm, not an interface method call
- **`matches()` on the enum** replaces `strategy.matches(query)` vtable dispatch — a free function `strategy::matches(query, _db)` returns `QueryMatch { intent, priority, matched }`
- **Deterministic fast path** completes in <100µs (identifier + capability queries); no heap allocation except the result `Vec`
- **LLM fallback** uses `LlmFilter` (backed by `reqwest` via `LlmClient`) for `ConceptQuery`, matching the Zig concept of LLM-assisted ranking
- **No SimHash** — the Rust version does not use SimHash for vector search; `vector_explain()` calls `GuidanceDb::vector_search` directly with raw `&[f32]`
- **No semantic alias expansion** — the `SemanticAliases` module exists but `QueryEngine.explain()` does not expand aliases before matching

## Example

```rust
use guidance_core::query_engine::QueryEngine;
use guidance_types::{GuidanceDoc, Member, MemberType, Meta};

let engine = QueryEngine::new();

let doc = GuidanceDoc {
    meta: Meta {
        module: "example".into(),
        source: "src/example.zig".into(),
        language: "zig".into(),
    },
    comment: Some("Example module.".into()),
    members: vec![
        Member {
            type_name: MemberType::FnDecl,
            name: "greet".into(),
            signature: Some("fn greet(name: []const u8) []const u8".into()),
            comment: Some("Greets the user.".into()),
            is_pub: true,
            ..Member::default()
        },
    ],
    ..GuidanceDoc::default()
};

let stages = engine.explain("greet", &doc).expect("explain");
assert!(!stages.is_empty());
assert!(stages.iter().any(|s| s.content.contains("greet")));
```

## Zig reference

See `../doc/capabilities/explain-query/CAPABILITY.md` in the Zig guidance source tree for the original `executeStaged` pipeline with SimHash vector search and semantic alias expansion.
