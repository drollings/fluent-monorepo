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

5. VERIFY (cargo):       cargo build --workspace && cargo test --workspace
                         && cargo clippy --workspace -- -D warnings
                         && cargo run --bin guidance -- structure .guidance
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
  common/           General-purpose utility crate (fluent-wvr-common)
                    Note: fluent-wvr-common contains no domain-specific logic;
                    no imports from dag/, coral/, or guidance/
  types/            guidance-types (FileType, MemberType, Param, Member, etc.)
   traits/           guidance-traits (serde-based replacement for reflection)
   content-node/     guidance-content-node (lod slicing, file content annotation)
   vector-math/      guidance-vector-math (cosine_similarity, QuantizedEmbedding, try_bytes_to_vec)
   vector-aliases/   guidance-vector-aliases (SemanticAliases, expand, expand_query)
   concurrency-queue/ guidance-concurrency-queue (EventQueue<T>, LlmRequestQueue wrapper)
   dag/              guidance-dag: executor, resolver, work_unit, adapter, middleware,
                     drift, type_inference, target, capability registry, error types
   guidance/         Updated consumer
   coral/            Updated consumer (MCP server, cache router, KNN ingest)
   llm/              LLM HTTP client + embeddings (CachedEmbeddingProvider, LlmRequestQueue,
                     LlmClient, url, error)
   ontology/         Ontology types (entity, mapper, triple store)
   rdf/              RDF/triple handling
   wasm_ipc/         Wasm tooling
bin/
   guidance          Updated binary (removed show/ingest, serve→mcp; added structure subcommand)
   guidance-py       Python AST provider (Python files → .guidance/ JSON)
   coral             New binary (coral mcp)
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
guidance sync --debug --file src/example.zig

# View metadata + prompts
guidance sync --debug --show-prompts --file src/example.zig

# View prompts only (no metadata)
guidance sync --show-prompts --file src/example.zig
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
guidance sync --file src/example.zig

# View what changed (only metadata, no comment diffs)
git diff .guidance/src/example.zig.json

# Database sync extracts comments from source
guidance sync --file src/example.zig --db .guidance.db
```

### Staleness Detection

Files are processed when:
1. **JSON absent** → needs initial generation
2. **JSON newer than source** → needs processing (e.g., imported)
3. **JSON older than source by >1 second** → genuinely stale
4. **JSON = src_mtime - 1 second** → validated, skipped (no changes)

The `--force` flag bypasses staleness checks for full regeneration.
