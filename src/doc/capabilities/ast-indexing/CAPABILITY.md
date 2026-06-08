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

- **Uses tree-sitter** instead of `std.zig.Ast` — supports Zig and Python with separate tree-sitter parsers; no built-in Zig compiler integration
- **Language dispatch** by file extension (`.zig`/`.zon` → Zig, `.py` → Python)
- **Visibility** checked via `pub` keyword in Zig; Python visibility inferred from leading `_`
- **Module comment** extracted from leading doc-comment nodes (`//!` / `///` / `#`)
- **No comptime detection** — `comptime_block` variant exists in `MemberType` but no extractor walks comptime blocks
- **SmolStr** for interned strings rather than Zig's slice+arena pattern
- **serde JSON** for serialization instead of `std.json`

## Example

```rust
use std::path::Path;
use guidance_guidance::ast_parser::AstParser;

let source = r#"/// Greets the user
pub fn greet(name: []const u8) []const u8 {
    return "Hello, " ++ name;
}
"#;

let mut parser = AstParser::new();
let doc = parser.parse_file(Path::new("main.zig"), source).expect("parse");
assert_eq!(doc.meta.language.as_str(), "zig");
assert_eq!(doc.members.len(), 1);
assert_eq!(doc.members[0].name.as_str(), "greet");
assert!(doc.members[0].is_pub);
```

## Zig reference

See `../doc/capabilities/ast-indexing/CAPABILITY.md` in the Zig guidance source tree for the original Zig AST walker implementation.
