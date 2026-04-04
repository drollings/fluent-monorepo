---
name: ast-indexing
description: Parses Zig and Python source files via AST to extract structured metadata (functions, structs, enums, types) into per-file JSON guidance documents under .guidance/src/.
anchors:
  - AstParser
  - parseFile
  - GuidanceDoc
  - Member
---

# AST Indexing

Converts source code into queryable structured JSON metadata for codebase navigation and LLM context building.

## What it does

- **Zig**: `AstParser` uses `std.zig.Ast` to walk the syntax tree, extracting `fn_decl`, `struct`, `enum`, `const`, `type` declarations with signatures, visibility, line numbers, and doc comments.
- **Python**: `guidance-py` uses the `ast` module to extract classes, functions, and top-level constants.
- **Output**: One `.guidance/src/<path>.json` per source file with `meta`, `comment`, `members[]`, `used_by[]`, `skills[]`, and `capabilities[]` fields.

## Key files

- `src/guidance/ast_parser.zig` — Zig AST traversal and JSON emission
- `bin/guidance-py` — Python AST provider
- `src/guidance/json_store.zig` — Reads/writes guidance JSON files
- `src/guidance/types.zig` — `GuidanceDoc`, `Member`, `Skill` types

## Incremental sync

Each member has a `match_hash` (SHA-256 of signature). On re-sync, only members whose hash changed are re-processed, making LLM comment infill cheap.

## Query relevance

When searching for "how does X work", the `comment` field of the corresponding `fn_decl` or `struct` is the primary semantic signal. Members without comments still surface via name/signature matching.

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (30 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/guidance/types.zig` | 1.0 | defines_anchor |
| `src/common/json_parser.zig` | 1.0 | defines_anchor |
| `src/guidance/ast_parser.zig` | 1.0 | defines_anchor |
| `src/guidance/comment_sync.zig` | 0.9 | used_by |
| `src/guidance/deps.zig` | 0.9 | used_by |
| `src/guidance/main.zig` | 0.9 | used_by |
| `src/guidance/sync.zig` | 0.9 | used_by |
| `src/common/llm.zig` | 0.9 | used_by |
| `src/common/repl.zig` | 0.9 | used_by |
| `src/common/root.zig` | 0.9 | used_by |
| `src/guidance/comment_checker.zig` | 0.9 | used_by |
| `src/guidance/comment_inserter.zig` | 0.9 | used_by |
| `src/guidance/document_indexer.zig` | 0.9 | used_by |
| `src/guidance/hash.zig` | 0.9 | used_by |
| `src/guidance/header_generator.zig` | 0.9 | used_by |
| `src/guidance/json_store.zig` | 0.9 | used_by |
| `src/guidance/line_verify.zig` | 0.9 | used_by |
| `src/guidance/llm_filter.zig` | 0.9 | used_by |
| `src/guidance/llm_filter_batch.zig` | 0.9 | used_by |
| `src/guidance/pattern.zig` | 0.9 | used_by |
| `src/guidance/plugin.zig` | 0.9 | used_by |
| `src/guidance/query_engine.zig` | 0.9 | used_by |
| `src/guidance/query_strategy.zig` | 0.9 | used_by |
| `src/guidance/ralph.zig` | 0.9 | used_by |
| `src/guidance/scanner.zig` | 0.9 | used_by |
| `src/guidance/schema_validator.zig` | 0.9 | used_by |
| `src/guidance/stage_builder.zig` | 0.9 | used_by |
| `src/guidance/staged.zig` | 0.9 | used_by |
| `src/guidance/sync_engine.zig` | 0.9 | used_by |
| `src/guidance/synthesize.zig` | 0.9 | used_by |

