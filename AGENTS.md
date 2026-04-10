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
  guidance/      Zig core engine
    main.zig           CLI dispatcher
    config.zig         Configuration management
    sync_engine.zig    Sync subcommands (init, gen, status, clean, commit, check, todo, diary)
    query_engine.zig   Query subcommands (explain, show, test, telemetry, cache-stats, serve)
    types.zig          Shared types (FileType, MemberType, Param, Member, etc.)
    enhancer.zig       LLM enhancement for comment generation
    staged.zig         Stage collection for explain pipeline
    vector_db.zig      SQLite hybrid search engine
    scanner.zig        Source file discovery
    codehealth/        Dead code detection (main.zig, extractor.zig)
    comments/          Comment management (core.zig, header.zig, inserter.zig, sync.zig)
    query/             Query pipeline (identifier.zig, strategy.zig, llm_filter.zig, llm_filter_batch.zig, synthesize.zig)
    sync/              Sync infrastructure (json_store.zig, json_writer.zig, line_verify.zig, marker.zig)
  common/           Zig shared utilities (string, hash, format, terminal, log, etc.)
                    Note: common/ contains no domain-specific logic; no imports from dag/, coral/, or guidance/
  dag/             DAG core (Target, TargetRegistry, DependencyResolver, DagExecutor, repl.zig, json_parser.zig)
  coral/           Coral Context (db, batch, cache, executor, targets, config, mcp, etc.)
                    Note: delegation.zig moved here from common/
  llm/             LLM HTTP client (Ollama/OpenAI)
  ontology/        Ontology types
  rdf/             RDF/triple handling
  reflection/       Reflection layer
  vector/          Vector/embedding utilities
  wasm/            Wasm tooling
  concurrency/     Concurrency primitives
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

---

## Debugging and LLM Usage

### Command-Line Flags

**`--debug` / `--verbose`**:
- Shows LLM metadata: `[enhancer] generating file doc for X`, `[enhancer] received response`
- Hides raw prompt text (use `--show-prompts` for prompts)
- Use for general debugging and progress tracking

**`--show-prompts`**:
- Shows complete raw prompt text sent to LLM
- Use when debugging prompt engineering or LLM responses
- Independent of `--debug` (can combine both)

Example:
```bash
# View metadata only
guidance gen --debug --file src/example.zig

# View metadata + prompts
guidance gen --debug --show-prompts --file src/example.zig

# View prompts only (no metadata)
guidance gen --show-prompts --file src/example.zig
```

### Comment Management

**Source Files** (`.zig`):
- Member comments (`///`) are the source of truth
- File/module comments (`//!`) also stored in JSON

**JSON Files** (`.guidance/src/**/*.json`):
- Store metadata: signatures, line numbers, match_hash
- File/module comments stored for backward compatibility
- Member comments NOT stored (smaller files, cleaner diffs)

**Database** (`.guidance.db`):
- Synced from both JSON and source files
- Member comments extracted from source during sync
- Used for semantic search via `guidance explain`

**Workflow**:
```bash
# Generate JSON without member comments
guidance gen --file src/example.zig

# View what changed (only metadata, no comment diffs)
git diff .guidance/src/example.zig.json

# Database sync extracts comments from source
guidance gen --file src/example.zig --db .guidance.db
```

### Staleness Detection

Files are processed when:
1. **JSON absent** → needs initial generation
2. **JSON newer than source** → needs processing (e.g., imported)
3. **JSON older than source by >1 second** → genuinely stale
4. **JSON = src_mtime - 1 second** → validated, skipped (no changes)

The `--force` flag bypasses staleness checks for full regeneration.
