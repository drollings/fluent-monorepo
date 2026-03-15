# Agent Bootloader — explain-gen

**Context**: explain-gen is a Zig-native, deterministic AST-guided SQLite FTS5 database
generator for NullClaw. It analyzes source files (Zig, Python, and future languages) via
AST, generates JSON metadata files in `.explain-gen/src/`, and compiles them into
`.explain.db` for its `explain` functionality.

## Prime Directive

1. **Current Zig knowledge**: Read `doc/skills/zig-current/SKILL.md` before writing any Zig
2. **Project structure**: Run `make gen-status` or inspect `.explain-gen/src/` for guidance
3. **Never guess**: use the JSON files in `.explain-gen/src/` to locate code
4. **Config**: model names and provider registry live in `.explain-gen/explain-gen-config.json`

---

## Quick Start: RALPH Loop (Discovery → Implementation)

```
1. DISCOVER (make):  make explain QUERY="<keywords or a short question>"
                     Prefer phrases: "sync guidance json" > "json"
                     Scan: module purpose, pattern type, skill list
                     Short queries (≤4 words): fast path, no LLM
                     Long queries (5+ words): LLM filter + synthesis
                     Flags: --no-llm, --filter=skip, --staged=false

2. UNDERSTAND (MCP): Read the primary source file(s) from step 1
                     Grep callers: who @import's this file?
                     Ask: do the listed skills actually apply?

3. DECIDE:           If skills match → read them
                     If not → proceed to implementation

4. IMPLEMENT:        Write to src/explain-gen/ or bin/ (for Python or other languages apart from Zig, i.e. explain-gen-py)
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
  explain-gen-py    Python AST provider (Python files → .explain-gen/ JSON)
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

**DO:**
- Read the results of `make explain` for any skills
- Ask: "What design pattern is used here?" before consulting skills
- Read skills **only** if source analysis confirms they apply

**DON'T:**
- Assume skills apply without validating against source code
- Skip source reading before implementation

---

## Capturing Knowledge

### Insights  (`.explain-gen/.doc/inbox/INSIGHTS.md`)
Append major discoveries here as markdown bullets.

### New Capabilities  (`.explain-gen/.doc/inbox/CAPABILITIES.md`)
Append major new features here as markdown bullets.  Before implementing, consult `.explain-gen/.doc/capabilities/*.md` to enforce DRY.
