# Agent Bootloader — guidance

**Context**: guidance is a Zig-native, deterministic-first AST-guided vector search
database generator with local AI enhancement.  When used to search the
codebase's capabilities and code, it can save over 90% of the tokens and tool
calls compared to the orchestrating AI coder using other tools.

## Prime Directive

1. **Never guess**: use `guidance explain "<query text>" for guidance, and
follow instructions for any queries of interest

---

## Quick Start: RALPH Loop (Discovery → Implementation)

```
1. DISCOVER (guidance):  guidance explain "<keywords or a short question>"
                         Prefer keywords: "cmdExplain"
                         Or, prefer a short question: "How do we sync guidance?"
                         Scan: module purpose, pattern type, skill list

2. UNDERSTAND (MCP):     Read the primary source file(s) from step 1
                         Grep callers: who @import's this file?
                         Ask: do the listed skills actually apply?

3. DECIDE:               If skills match → read them
                         If not → proceed to implementation

4. IMPLEMENT:            Write to src/guidance/ or bin/ (for Python or
                         other languages apart from Zig, i.e.  guidance-py)
                         Follow source patterns and applicable skills only

5. VERIFY (make):        make pre-commit
                         build → test → lint → guidance gen → STRUCTURE.md
```

---

## Source Layout

```
src/
  guidance/      Zig core engine (AST parser, sync, lance_db, structure, deps)
  common/           Zig shared LLM HTTP client and arg helpers
bin/
  guidance       Compiled binary — zig-out/bin/guidance (via zig build)
  guidance-py    Python AST provider (Python files → .guidance/ JSON)
.guidance/
  guidance-config.json   Model / provider configuration
  .skills/          Structured skill documents (GoF, zig-current, domain-patterns)
  .doc/             Capabilities, diary, inbox
  src/              Generated guidance JSON (mirrors src/ tree)
.guidance.db        SQLite vector search database consumed by NullClaw explain tool
env/
  mk/               Shared Makefile helpers and per-language target overrides
  mise/             Language-specific mise.toml fragments
doc/
  DESIGN.md         System design reference
```

---

**DO:**
- Run `guidance explain "<query>"` and read the results
- Ask: "What capabilitity is used here?" before consulting skills

**DON'T:**
- Assume skills apply without validating against source code
- Write any code in Zig without reading `doc/skills/zig-current/SKILL.md` first
