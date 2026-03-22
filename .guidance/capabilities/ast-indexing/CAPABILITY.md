---
name: ast-indexing
description: Parses Zig and Python source files via AST to extract structured metadata (functions, structs, enums, types) into per-file JSON guidance documents under .guidance/src/.
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
