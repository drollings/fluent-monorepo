---
name: sync-pipeline
description: Incremental source file synchronisation that walks the workspace, detects changed files by mtime, parses ASTs, and persists GuidanceDoc as JSON. Uses blake3 for content hashing and serde_json for I/O.
anchors:
  - SyncEngine
  - gen_if_stale
  - is_stale
  - should_generate
  - match_hash_from_signature
  - load_guidance
  - save_guidance
  - sync_comments
---

# Sync Pipeline

The `SyncEngine` walks source files, detects staleness, parses AST members, and saves/loads `GuidanceDoc` as JSON in the `.guidance/src/` directory tree.

## Incremental detection

A file is stale when:
1. Its guidance JSON does not exist, **or**
2. The source file's mtime is more than 1 second newer than the JSON's mtime

```rust
// guidance/src/sync/staleness.rs
pub fn is_stale(json_path: &Path, source_path: &Path) -> bool;
pub fn should_generate(json_path: &Path, source_path: &Path) -> bool;
```

## Per-file pipeline

```
1. SyncEngine::gen_if_stale(source_path)
2.   → staleness::should_generate(&json_path, source_path)
3.   → AST parse → GuidanceDoc with members[]
4.   → json_store::save_guidance(&json_path, &doc)
5. Return true if generated, false if up-to-date
```

## match_hash computation

Signatures are hashed with **blake3** for change detection:

```rust
pub fn match_hash_from_signature(signature: &str) -> String {
    let hash = blake3::hash(signature.as_bytes());
    hash.to_hex().to_string()
}
```

## Example

```rust
use guidance::sync_engine::SyncEngine;

let mut engine = SyncEngine::new(guidance_dir.into(), source_dir.into());

// Generate if stale — returns true if work was done
let generated = engine.gen_if_stale(&zig_file)?;

// Load existing doc
if let Some(doc) = engine.load_doc(&zig_file)? {
    println!("Module: {}", doc.meta.module);
    for member in &doc.members {
        println!("  {} {}", member.type_name, member.name);
    }
}

// Check sync status
let status = engine.status()?;
println!("Stale: {}, up-to-date: {}", status.stale_files, status.up_to_date);
```

## Key files

- `guidance/src/sync_engine.rs` — `SyncEngine`, `SyncStatus`, `SyncEngineError`
- `guidance/src/sync/json_store.rs` — `load_guidance`, `save_guidance`, `JsonError`
- `guidance/src/sync/json_writer.rs` — `doc_to_json`, `doc_to_json_string`
- `guidance/src/sync/staleness.rs` — `is_stale`, `should_generate`, `match_hash_from_signature`
- `guidance/src/sync/comments.rs` — `sync_comments`, `insert_comments`

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| Content hash | SHA-256 for match_hash | **blake3** for match_hash |
| JSON I/O | Custom `StringBuilder` / `json.Writer` | `serde_json::to_string_pretty` / `from_str` |
| AST parse phase | Test → lint → fmt → AST → LLM infill pipeline | AST parse only (test/lint/fmt/LLM not yet wired) |
| Staleness window | Filesystem-duration-based | 1-second mtime tolerance |
| Comment sync | `comments/sync.zig` handles re-insertion | `sync/comments.rs` with `insert_comments()` |

## Zig reference

See `doc/capabilities/sync-pipeline/CAPABILITY.md` in the Zig project for the original pipeline design (SyncProcessor, full pipeline phases, database sync).
