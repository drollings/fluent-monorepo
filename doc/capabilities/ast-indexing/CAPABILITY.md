---
name: ast-indexing
description: Parses source files (Zig, Python) via tree-sitter into structured GuidanceDoc/Member metadata
anchors:
  - AstParser
  - parse_file
  - GuidanceDoc
  - Member
  - MemberType
---

# AST Indexing

Parses Zig and Python source files using tree-sitter to extract module-level comments, function declarations, struct/enum/union definitions, variable declarations, and test declarations. Produces a `GuidanceDoc` containing a `Meta` header and a list of `Member` items with signature, doc comments, parameters, return types, visibility, and line numbers.

## Key files

- `guidance/src/ast_parser.rs` — `AstParser` struct, `parse_file()`, tree-sitter cursor walks
- `guidance/src/sync/json_store.rs` — `load_guidance()`, `save_guidance()` JSON persistence
- `common/src/types.rs` — `GuidanceDoc`, `Member`, `MemberType`, `Param`, `Meta`

## Semantic Deviations

- **Language dispatch** by file extension (`.zig`/`.zon` → Zig, `.py` → Python)
- **Visibility** checked via `pub` keyword in Zig; Python visibility inferred from leading `_`
- **Module comment** extracted from leading doc-comment nodes (`//!` / `///` / `#`)
- **No comptime detection** — `comptime_block` variant exists in `MemberType` but no extractor walks comptime blocks
- **SmolStr** for interned strings
- **serde JSON** for serialization instead of `std.json`

## Incremental sync

Each member has a `match_hash` (SHA-256 of signature). On re-sync, only members whose hash changed are re-processed, making LLM comment infill cheap.

## Query relevance

When searching for "how does X work", the `comment` field of the corresponding `fn_decl` or `struct` is the primary semantic signal. Members without comments still surface via name/signature matching.

