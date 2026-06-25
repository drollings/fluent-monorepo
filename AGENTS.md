# Agent Bootloader — guidance

**Context**: guidance is a Rust-native, deterministic-first AST-guided vector search
database generator with local AI enhancement.  When used to search the
codebase's capabilities and code, it can save over 90% of the tokens and tool
calls compared to the orchestrating AI coder using other tools.

## Prime Directive

1. **Never guess**: use `guidance explain "<query text>"` for guidance, and
follow instructions for any queries of interest

---

## Quick Start: RALPH Loop (Discovery → Implementation)

```
1. DISCOVER (guidance):  guidance explain "<keywords or a short question>"
                         Prefer keywords: "cmdExplain"
                         Or, prefer a short question: "How do we sync guidance?"
                         Scan: module purpose, pattern type, skill list

2. UNDERSTAND (MCP):     Read the primary source file(s) from step 1
                         Grep callers: who imports this file?
                         Ask: do the listed skills actually apply?

3. DECIDE:               If skills match → read them
                         If not → proceed to implementation

4. IMPLEMENT:            Write to src/guidance/ or src/bin/ (for binary targets)
                         Follow source patterns and applicable skills only

5. VERIFY (cargo):       cargo build --workspace && cargo test --workspace
                         && cargo clippy --workspace -- -D warnings
                         && cargo run --bin guidance -- structure .guidance
```

---

## Source Layout

```
src/
  bin/
    guidance/          guidance binary (16-subcommand CLI + MCP server)
    coral/             coral binary (MCP server + ingest CLI)
  guidance/            guidance-core: AST parser, sync engine, query engine, config
  coral/               coral-context: graph DB, cache router, MCP server, WASM runtime
  dag/                 guidance-dag: executor, resolver, work_unit, adapter, middleware,
                       drift, type_inference, target, capability registry, error types
  fluent-wvr/          Fluent WVR: Component, WorkUnit, FieldAccess, Describable traits
  fluent-wvr-macros/   Proc macros for FieldAccess derive
  fluent-concurrency/  WorkerPool, Scope, Zone, Limiter, PriorityQueue, CreditFlow
  llm/                 LLM HTTP client + embeddings (CachedEmbeddingProvider, LlmRequestQueue,
                       LlmClient, url, error)
  types/               guidance-types (FileType, MemberType, Param, Member, etc.)
  common-core/         General-purpose utility crate (fluent-wvr-common)
                       Note: common-core contains no domain-specific logic;
                       no imports from dag/, coral/, or guidance/
  content-node/        guidance-content-node (lod slicing, file content annotation)
  search-vector/       guidance-search-vector (SQLite hybrid search + HNSW index)
  project-knowledge/   guidance-project-knowledge (WordIndex, TrigramIndex, CsrGraph, QueryCache)
  ontology/            guidance-ontology (entity extraction, YAGO taxonomy, capability inference)
  rdf/                 guidance-rdf (Turtle/N-Quads parser, normalization)
  wasm_ipc/            guidance-wasm-ipc (WASM IPC binary types)
  memory-plugin/       Pluggable memory tier (holographic, hindsight, honcho backends)
.guidance/
  guidance-config.json   Model / provider configuration
  .skills/          Structured skill documents (GoF, zig-current, domain-patterns)
  .doc/             Capabilities, diary, inbox
  src/              Generated guidance JSON (mirrors src/ tree)
.guidance.db        SQLite vector search database consumed by guidance explain
env/
  mk/               Shared Makefile helpers and per-language target overrides
  mise/             Language-specific mise.toml fragments
doc/
  DESIGN.md         System design reference
```

---

**DO:**
- Run `guidance explain "<query>"` and read the results
- Ask: "What capability is used here?" before consulting skills

**DON'T:**
- Assume skills apply without validating against source code
- Import from `src/guidance/` or `src/coral/` — those are consumers, not producers

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
guidance sync --debug --file src/example.rs

# View metadata + prompts
guidance sync --debug --show-prompts --file src/example.rs

# View prompts only (no metadata)
guidance sync --show-prompts --file src/example.rs
```

### Comment Management

**Source Files** (`.rs`, `.zig`, `.py`):
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
guidance sync --file src/example.rs

# View what changed (only metadata, no comment diffs)
git diff .guidance/src/example.rs.json

# Database sync extracts comments from source
guidance sync --file src/example.rs --db .guidance.db
```

### Staleness Detection

Files are processed when:
1. **JSON absent** → needs initial generation
2. **JSON newer than source** → needs processing (e.g., imported)
3. **JSON older than source by >1 second** → genuinely stale
4. **JSON = src_mtime - 1 second** → validated, skipped (no changes)

The `--force` flag bypasses staleness checks for full regeneration.
