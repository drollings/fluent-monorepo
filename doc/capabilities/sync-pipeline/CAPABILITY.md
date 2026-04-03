---
name: sync-pipeline
description: Incremental source file synchronisation pipeline that runs test, lint, format, and AST guidance generation for each changed file, using mtime and match_hash for cheap change detection.
anchors:
  - SyncProcessor
  - syncDatabase
  - matchHash
  - fileNeedsProcessing
---

# Sync Pipeline

The `guidance gen` command walks the workspace, detects changed source files, and runs the full guidance pipeline (test → lint → fmt → AST parse → LLM infill) for each stale file.

## Incremental detection

A file is considered stale when:
1. Its guidance JSON does not exist, **or**
2. The source file's `mtime` is newer than the guidance JSON's `mtime`

The guidance JSON mtime acts as a "all phases passed" marker — it is only touched after every phase succeeds.

## Per-file pipeline

```
1. Parse AST → members[] with signatures and line numbers
2. Load existing guidance JSON (if present)
3. For each member:
   a. Compute match_hash = SHA-256(signature)
   b. If hash changed → clear LLM comment (re-infill later)
   c. If hash unchanged → preserve existing comment
4. Optional: LLM comment infill (--infill) or regen (--regen)
5. Write guidance JSON to .guidance/src/<path>.json
6. Touch mtime as "done" marker
```

## Database sync

After all source files are processed:
- `.guidance.db` is updated from `.guidance/src/` JSON files (LanceDB vector search)

## Key files

- `src/guidance/sync.zig` — `SyncProcessor`, per-file pipeline
- `src/vector/vector_db.zig` — `syncDatabase` (LanceDB-style SQLite backend)
- `src/guidance/hash.zig` — `matchHash` computation
- `src/guidance/marker.zig` — `fileNeedsProcessing` staleness check

## CLI

```bash
guidance gen                    # full workspace scan
guidance gen --file src/foo.zig # single file
guidance gen --scan src/        # directory scan
guidance gen --force            # re-process all, ignore mtime
guidance gen --infill           # fill missing LLM comments
```

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (7 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/guidance/sync.zig` | 1.0 | defines_anchor |
| `src/guidance/marker.zig` | 1.0 | defines_anchor |
| `src/vector/vector_db.zig` | 1.0 | defines_anchor |
| `src/guidance/query_engine.zig` | 0.9 | used_by |
| `src/guidance/sync_engine.zig` | 0.9 | used_by |
| `src/vector/root.zig` | 0.9 | used_by |
| `src/guidance/comment_sync.zig` | 0.9 | used_by |

