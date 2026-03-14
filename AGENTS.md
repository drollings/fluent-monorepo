# Agent Bootloader — explain-gen

**Context**: explain-gen is a Zig-native, deterministic AST-guided SQLite FTS5 database
generator for NullClaw. It analyzes source files (Zig, Python, and future languages) via
AST, generates JSON metadata files in `.explain-gen/src/`, and compiles them into
`.explain.db` for NullClaw's `explain` tool.

## Prime Directive

1. **Current Zig knowledge**: Read `doc/skills/zig-current/SKILL.md` before writing any Zig
2. **Project structure**: Run `make gen-status` or inspect `.explain-gen/src/` for guidance
3. **Never guess**: use the JSON files in `.explain-gen/src/` to locate code
4. **Config**: model names and provider registry live in `.explain-gen/explain-gen-config.json`

---

## Quick Start: RALPH Loop (Discovery → Implementation)

```
1. DISCOVER (make):  make gen-status
                     Inspect .explain-gen/src/ for module JSON files
                     grep -ri "<term>" src/

2. UNDERSTAND (MCP): Read the primary source file(s) identified above
                     Grep callers: who @import's this file?
                     Ask: do the listed skills actually apply?

3. DECIDE:           If skills match → read them
                     If not → proceed to implementation

4. IMPLEMENT:        Write to src/explain-gen/ or bin/ (Python providers)
                     Follow source patterns and applicable skills only

5. VERIFY (make):    make pre-commit
                     build → test → guidance gen → lint → STRUCTURE.md
```

---

## Source Layout

```
src/
  explain-gen/      Zig core engine (AST parser, sync, db, structure, deps)
  common/           Zig shared LLM HTTP client and arg helpers
bin/
  explain-gen       Compiled binary — zig-out/bin/explain-gen (via zig build)
  ast-guidance-py   Python AST provider (Python files → .explain-gen/ JSON)
.explain-gen/
  explain-gen-config.json   Model / provider configuration
  .skills/          Structured skill documents (GoF, zig-current, domain-patterns)
  .doc/             Capabilities, diary, inbox
  src/              Generated guidance JSON (mirrors src/ tree)
.explain.db         SQLite FTS5 database consumed by NullClaw explain tool
env/
  mk/               Shared Makefile helpers and per-language target overrides
  mise/             Language-specific mise.toml fragments
doc/
  DESIGN.md         System design reference
```

---

## NullClaw Integration

`.explain.db` is the hand-off artifact to NullClaw. Run:

```bash
make gen                     # Generate .explain-gen/ JSON and .explain.db
make gen-infill              # Same, with LLM comment infill
make gen-status              # Show sync status
make gen-clean               # Remove .explain-gen/src and .explain.db
```

NullClaw reads `.explain.db` for BM25 FTS5 search. Schema version is tracked in the
`schema_version` table. Current version: **1**.

### .explain.db Schema

```sql
schema_version(version)          -- always 1
ast_nodes(id, file_path, module, node_type, name, signature, comment,
          line, used_by, language, file_type, file_hash, last_modified)
fts_search USING fts5(name, comment, module, signature)
```

### CLI

```bash
explain-gen gen [--workspace DIR] [--json-dir DIR] [-o/--db PATH] [--infill] [--regen]
explain-gen status [--json-dir DIR] [-o/--db PATH]
explain-gen clean [--json-dir DIR] [-o/--db PATH]
explain-gen structure [--json-dir DIR]
explain-gen deps [--src DIR]
```

---

## Skill Reading Strategy

**DO:**
- Read the JSON files in `.explain-gen/src/` to understand module structure
- Read the primary source file identified from JSON metadata
- Ask: "What design pattern is used here?" before consulting skills
- Read skills **only** if source analysis confirms they apply

**DON'T:**
- Assume skills apply without validating against source code
- Read `.explain-gen/**/*.json` directly for search — query `.explain.db` instead
- Skip source reading before implementation

---

## Capturing Knowledge

### Insights  (`.explain-gen/.doc/inbox/INSIGHTS.md`)
Append major discoveries here as markdown bullets.

### New Capabilities  (`.explain-gen/.doc/inbox/CAPABILITIES.md`)
Append major new features here as markdown bullets.  Before implementing,
consult `.explain-gen/.doc/capabilities/*.md` to enforce DRY.

---

## Language Provider Convention

Each language provider (`ast-guidance-py`, `explain-gen-cpp`, …) must:

1. Accept: `sync --file <path> --output <json_dir> [--infill]`
2. Accept: `sync --scan <dir>  --output <json_dir> [--infill]`
3. Emit canonical JSON at `<json_dir>/src/<rel_path>.<ext>.json`
4. Use the schema: `{ meta, comment, skills, hashtags, used_by, members[] }`
5. Read model name from `.explain-gen/explain-gen-config.json` → `models.infill`

When making function calls using tools that accept array or object parameters ensure those
are structured using JSON.
