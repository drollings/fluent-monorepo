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
## Sources (42 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/guidance/types.zig` | 1.0 | defines_anchor |
| `src/guidance/ast_parser.zig` | 1.0 | defines_anchor |
| `src/guidance/comments/sync.zig` | 0.9 | used_by |
| `src/guidance/plugins/zig_plugin.zig` | 0.9 | used_by |
| `src/guidance/query_engine.zig` | 0.9 | used_by |
| `src/guidance/sync.zig` | 0.9 | used_by |
| `src/guidance/comments/core.zig` | 0.9 | used_by |
| `src/guidance/comments/core_tests.zig` | 0.9 | used_by |
| `src/guidance/comments/header.zig` | 0.9 | used_by |
| `src/guidance/comments/header_tests.zig` | 0.9 | used_by |
| `src/guidance/comments/inserter.zig` | 0.9 | used_by |
| `src/guidance/comments/sync_tests.zig` | 0.9 | used_by |
| `src/guidance/core/excerpt.zig` | 0.9 | used_by |
| `src/guidance/core/format.zig` | 0.9 | used_by |
| `src/guidance/core/metadata.zig` | 0.9 | used_by |
| `src/guidance/document_indexer.zig` | 0.9 | used_by |
| `src/guidance/document_indexer_tests.zig` | 0.9 | used_by |
| `src/guidance/main.zig` | 0.9 | used_by |
| `src/guidance/pattern.zig` | 0.9 | used_by |
| `src/guidance/plugin.zig` | 0.9 | used_by |
| `src/guidance/plugins/markdown_plugin.zig` | 0.9 | used_by |
| `src/guidance/plugins/markdown_plugin_tests.zig` | 0.9 | used_by |
| `src/guidance/plugins/treesitter_extractor.zig` | 0.9 | used_by |
| `src/guidance/plugins/treesitter_plugin.zig` | 0.9 | used_by |
| `src/guidance/query/llm_filter.zig` | 0.9 | used_by |
| `src/guidance/query/llm_filter_batch.zig` | 0.9 | used_by |
| `src/guidance/query/strategy.zig` | 0.9 | used_by |
| `src/guidance/query/synthesize.zig` | 0.9 | used_by |
| `src/guidance/schema_validator.zig` | 0.9 | used_by |
| `src/guidance/skeleton.zig` | 0.9 | used_by |
| `src/guidance/stage_builder.zig` | 0.9 | used_by |
| `src/guidance/stage_builder_tests.zig` | 0.9 | used_by |
| `src/guidance/staged.zig` | 0.9 | used_by |
| `src/guidance/staged_tests.zig` | 0.9 | used_by |
| `src/guidance/sync/gen_files.zig` | 0.9 | used_by |
| `src/guidance/sync/json_store.zig` | 0.9 | used_by |
| `src/guidance/sync/json_writer.zig` | 0.9 | used_by |
| `src/guidance/sync/line_verify.zig` | 0.9 | used_by |
| `src/guidance/sync/line_verify_tests.zig` | 0.9 | used_by |
| `src/guidance/sync_engine.zig` | 0.9 | used_by |
| `src/guidance/types_tests.zig` | 0.9 | used_by |
| `src/guidance/sync/fast_snapshot.zig` | 0.4 | path_heuristic |

