# guidance: Vision Document

**A Deterministic-First Code Navigation Subagent for AI-Assisted Development**

---

## Executive Summary

guidance is a Zig-native AST-guided code navigation tool that produces `.guidance/src/**/*.json` metadata mirrors and `.guidance.db` SQLite vector search databases. It serves as a deterministic-first subagent for AI coders, optimizing for:
- **Token efficiency**: Minimal context required for frontier model queries
- **RALPH orchestration**: Human-in-the-loop test→lint→fmt→guidance→structure cycles
- **Sub-100ms queries**: Keyword-first with optional LLM synthesis for long queries
- **Zero marginal cost**: Cached results for repeated queries

The vision is to evolve guidance into a fully-capable subagent that offloads AI coders by maintaining comprehensive codebase knowledge, detecting staleness automatically, and providing deterministic answers where possible while escalating novel queries to local LLM inference.

---

## Core Goals

### Goal 1: Deterministic-First Code Navigation

Replace frontier model code browsing with local computation:

```
Traditional AI Coder:     Query → Frontier LLM → Response (expensive, slow)
guidance:                 Query → SQLite Search → Cached Result
                              ↓ (if miss)
                          Local LLM Synthesis → Cache for next time
```

**Key Outcomes:**
- Sub-100ms latency for cached patterns
- Zero API cost for deterministic queries (≤2 words, no question pattern)
- Local LLM synthesis only for novel queries (>2 words or question patterns)
- Full auditability through `.guidance.db` query log

### Goal 2: RALPH Loop Orchestration

The `guidance check` command enforces the RALPH loop:

```
build → test → lint → fmt → guidance gen → structure → db
```

This is deterministic waterfall execution: each stage must pass before the next begins. The `guidance check` command is the pre-commit hook entry point that ensures codebase integrity before any commit.

**Key Invariants:**
- JSON mtime is the universal "all stages passed" marker
- Source mtime > JSON mtime = stale file requiring re-sync
- `match_hash` (SHA-256 of signature + comment) enables incremental comment preservation
- `guidance gen` processes only changed files; `guidance gen --force` reprocesses all
- `comment_generated` flag tracks LLM-generated comments for source sync

### Goal 3: Token-Efficient Context for AI Coders

The staged explain pipeline produces optimal context for AI consumption:

```
Query: "how does filterStages work?"
  ↓
Stage Collection:
  ├── Prose: Module comments + member descriptions (semantic context)
  ├── Code: Source excerpts with verified line numbers (implementation)
  ├── Metadata: Keywords, skills, capabilities, used_by (discovery)
  ├── Insight: From .guidance/doc/INSIGHTS.md (recent learnings)
  └── Skill: From doc/skills/*/SKILL.md (design patterns)
  ↓
LLM Filter (if >3 words): Relevance pruning
  ↓
LLM Synthesis: Markdown summary with file:line citations
```

**Key Features:**
- `--no-llm`: Fast path for structural output only
- `--filter=auto|force|skip`: Control LLM relevance filtering
- `--staged=false`: Legacy output format (rollback safety)

### Goal 4: Cross-Language Parity

Both `guidance gen` (Zig) and `bin/guidance-py` (Python) produce identical JSON schemas:

| Field | Zig | Python | Purpose |
|-------|-----|--------|---------|
| `meta.module` | ✓ | ✓ | Module name |
| `meta.source` | ✓ | ✓ | Relative path |
| `meta.language` | ✓ | ✓ | "zig" or "python" |
| `members[].match_hash` | ✓ | ✓ | SHA-256 of signature + comment |
| `members[].comment` | ✗ | ✗ | ~~Not stored in JSON~~ (extracted from source) |
| `skills[].ref` | ✓ | ✓ | SKILL.md reference |
| `used_by[]` | ✓ | ✓ | Reverse dependencies |

**Parity Enforcement:** JSON Schema validation at sync time (planned).

---

## Architecture

### Component 1: Sync Pipeline (`SyncProcessor`)

```
Source File (.zig/.py/.md/.json)
        ↓
    [Plugin Registry] ─→ Extension for each file type
        ↓
    AST Parser (Zig: std.zig.Ast, Python: ast.parse)
        ↓
    Member Extraction ──→ members[] with signatures, line numbers
        ↓
    [match_hash Check] ──→ Preserve comments if signature unchanged
        ↓
    [Pattern Detection] ──→ Auto-attach GoF/domain patterns
        ↓
    [LLM Enhancement] ──→ Generate missing comments (comment_generated=true)
        ↓
    Guidance JSON ──→ .guidance/src/<path>.json
        ↓
    [Post-Processing Phase] ──→ Write generated comments to source
        │                         ↓
        │                    [zig fmt]
        │                         ↓
        │                    [Line Number Correction]
        │                         ↓
        │                    [match_hash Recalculation]
        │
    [Database Sync] ──→ .guidance.db (SQLite vector search)
```

**Incremental Design:**
- `fileNeedsProcessing()`: Compare source mtime vs JSON mtime
- `match_hash`: SHA-256 of `signature ++ "|||COMMENT|||" ++ comment`; unchanged = preserve comments
- Per-file processing: `guidance gen --file src/foo.zig`
- `comment_generated` flag: Tracks LLM-generated comments needing source sync
- **Post-processing**: After JSON generation, scan for `comment_generated=true` members, write to source, run fmt, correct line numbers

### Component 2: Query Engine (`GuidanceDb` + `executeStaged`)

The query engine implements a **multi-level inverted index** for O(1) routing:

```
Query String
      ↓
  [Phase 1a: Embedding-based Alias Expansion]
      └── findSimilarAliasKeys(query_embedding, 0.75, 3)
      └── Expands query tokens via semantic-aliases.json embeddings
      ↓
  [Phase 1b: Capability-Guided Keyword Expansion]
      └── findCapabilityKeywordsForQuery(query_embedding, 0.45, 3)
      └── Injects AST-level keywords from capability-mapping.json
      ↓
  [Phase 2: Token-based Alias Expansion]
      └── semantic-aliases.json: token → [expanded tokens]
      └── Exact token match (case-insensitive)
      ↓
  [Phase 3: Hybrid Search]
      ├── Deterministic name match (case-sensitive AST lookup)
      ├── Vector search (SimHash pre-filter → cosine similarity)
      └── Keyword search (LIKE on name, comment, keywords)
      ↓
  [Capability Routing] ──→ findMatchedCapabilityNamesForQuery(0.45, 3)
      ↓
  [Stage Collection] ──→ Prose, Code, Metadata, Insight, Skill stages
      ↓
  [LLM Filter] ──→ (if >3 words) filterStages() relevance filtering
      ↓
  [LLM Synthesis] ──→ Markdown summary with citations
```

**Multi-Level Inverted Index:**

| Level | Content | Location | Activation |
|-------|---------|----------|------------|
| **Token** | token → expanded tokens | semantic-aliases.json | Phase 2 (exact token match) |
| **Embedding Alias** | query → similar alias keys | semantic-aliases.json | Phase 1a (0.75 cosine) |
| **Capability Keyword** | capability → AST keywords | capability-mapping.json | Phase 1b (0.45 cosine) |
| **File** | file → capabilities | capability-mapping.json | Post-search routing |
| **Member** | member → skills, used_by | .guidance/src/*.json | Metadata stage |
| **Code** | node_id → embedding | ast_nodes.embedding | Vector search |

**Performance Tiers:**

| Query Type | Path | Latency | Token Cost |
|------------|------|---------|------------|
| Single keyword (≤2 words) | Deterministic name match + keyword | <50ms | 0 |
| Question pattern | Hybrid + LLM filter + synthesis | 200-800ms | Varies |
| Semantic search | SimHash → cosine + LLM | 500-1500ms | Varies |

### Component 3: RALPH Loop (`cmdCheck`)

```zig
pub fn cmdCheck(allocator, args) !void {
    // 1. Build (zig build)
    // 2. Test (zig build test)
    // 3. Lint (zig fmt --check)
    // 4. Format (zig fmt)
    // 5. Guidance gen (all languages, incremental)
    // 6. Structure (regenerate STRUCTURE.md)
    // 7. Database (sync .guidance.db)
}
```

**Failure Modes:**
- `error.TestFailed`: Exit 1, print test output
- `error.LintFailed`: Exit 1, print lint violations
- `error.ParseError`: Continue with warning (unguidable file)

### Component 4: Registry Layer (shared with Coral)

```
src/common/
├── registry.zig    ──→ TargetRegistry, TargetBuilder (Fluent Builder)
├── target.zig      ──→ Target, TargetSchema, DynamicEditable
├── interner.zig    ──→ StringInterner (RwLock-protected bitset index)
├── embeddings.zig  ──→ EmbeddingProvider VTable
└── llm.zig         ──→ LlmClient (Ollama/OpenAI HTTP client)
```

**Fluent WVR Integration:**
- All `Target` instances use `TargetSchema` for DynamicEditable access
- `StringInterner` provides thread-safe string→bitset conversion
- `EmbeddingProvider` vtable enables provider swapping (Ollama, OpenAI, Noop)

---

## Data Model

### GuidanceDoc (`.guidance/src/**/*.json`)

```zig
pub const GuidanceDoc = struct {
    meta: Meta,                    // module, source, language
    comment: ?[]const u8,          // Module-level doc comment (//!)
    detail: ?[]const u8,           // Comprehensive documentation (<800 words)
    keywords: []const []const u8,  // Discovery keywords (fast model)
    skills: []const Skill,         // SKILL.md references
    capabilities: []const []const u8, // CAPABILITY.md references
    hashtags: []const []const u8,   // Discovery hashtags
    used_by: []const []const u8,    // Reverse @import dependencies
    members: []const Member,        // Extracted declarations
};
```

### Member (`.guidance/src/**/*.json` members[])

```zig
pub const Member = struct {
    type: MemberType,              // fn_decl, struct, enum, etc.
    name: []const u8,               // Member name
    match_hash: ?[]const u8 = null, // SHA-256(signature ++ "|||COMMENT|||" ++ comment)
    signature: ?[]const u8 = null,   // Function/struct signature
    params: []const Param = &.{},   // Function parameters
    returns: ?[]const u8 = null,     // Return type
    comment: ?[]const u8 = null,     // in-memory: extracted from source (not in JSON)
    line: ?u32 = null,              // 1-based line number
    comment_generated: bool = false, // True if LLM-generated (not from source)
    // ... other fields
};
```

**Comment Storage Strategy:**
- **Module-level comments (`//!`)**: Stored in JSON's top-level `comment` field
- **Member comments (`///`)**: Extracted from source on-demand, never stored in JSON
- **Rationale**: Keeps JSON minimal, reduces diff noise, aligns with Zig's design
- **Backward compatibility**: Old JSON files with member comments still load correctly
- **`match_hash`**: Still tracks comment changes for staleness detection

### Stage (Staged Query Pipeline)

```zig
pub const StageKind = enum {
    prose,      // Human-readable explanation
    code,       // Verbatim source excerpt
    metadata,   // Keywords, see_also, skills
    insight,    // INSIGHTS.md/CAPABILITIES.md bullet
    skill_doc,  // SKILL.md excerpt
};

pub const Stage = struct {
    kind: StageKind,
    content: []const u8,  // Owned
    source: []const u8,   // Owned: "src/foo.zig" or "src/foo.zig:functionName"
    line: ?u32,           // Source line number
};
```

### SearchResult (Database Row)

```sql
CREATE TABLE ast_nodes (
    id INTEGER PRIMARY KEY,
    file_path TEXT NOT NULL,
    source TEXT NOT NULL,        -- Relative path
    name TEXT NOT NULL,          -- Member name
    node_type TEXT NOT NULL,     -- "fn_decl", "struct", "enum", "module"
    signature TEXT,
    comment TEXT,                -- Extracted from source on sync
    detail TEXT,                 -- Comprehensive documentation
    keywords TEXT,               -- JSON array
    line INTEGER,
    embedding BLOB,              -- Float32 vector
    match_hash TEXT,             -- SHA-256 of signature + comment
    used_by TEXT,                -- JSON array of paths
    skills TEXT,                 -- JSON array of skill refs
    capabilities TEXT            -- JSON array of capability names
);
```

**Comment Extraction During Sync:**
- Module comments: Loaded from JSON's top-level `comment` field
- Member comments: Extracted from source files using `extractCommentAtLine()`
- Extracted during `guidance db sync` and stored in database for search
- Always in sync with source because extraction happens on every sync

---

## Current Implementation Status

### Completed ✅

1. **AST Parsing**: Zig (`std.zig.Ast`) and Python (`ast.parse`) with plugin registry
2. **Incremental Sync**: `match_hash` comparison, mtime-based staleness detection
3. **Hybrid Search**: SQLite keyword + cosine similarity vector fusion with SimHash pre-filter
4. **Staged Pipeline**: `executeStaged()`, `expandFollowUps()`, `formatStaged()`
5. **Multi-Level Inverted Index**:
   - Semantic aliases: token expansion via embedding (0.75) and exact match
   - Capability keywords: `findCapabilityKeywordsForQuery()` for AST-level routing
   - File→capability mapping: `capability-mapping.json`
   - Member enrichment: `skills[]`, `capabilities[]`, `used_by[]`, `hashtags[]`
6. **LLM Filtering**: `filterStages()` for relevance pruning on queries >3 words
7. **RALPH Loop**: `guidance check` orchestration
8. **Fluent WVR**: Registry, target, interner from `src/common/`
9. **Skills/Capabilities**: Auto-attachment during pattern detection
10. **Capability Routing**: `findMatchedCapabilityNamesForQuery()` in `executeStaged()`
11. **Comment In-filling**: Automatic LLM-generated comment sync to source files
    - `match_hash` includes comment: `SHA-256(signature ++ "|||COMMENT|||" ++ comment)`
    - `comment_generated` flag tracks LLM-generated comments
    - Post-processing phase writes generated comments to source
    - Single fmt pass after all comment insertions
    - Line number correction after fmt

### In Progress 🔄

1. **JSON Schema Validation**: Enforce parity between Zig and Python outputs
2. **Capability Keyword Expansion**: Currently uses `findCapabilityKeywordsForQuery()` with 0.45 cosine threshold; potential for tighter integration with semantic aliases

### Planned 📋

1. **LOD Context Packing**: Level-of-detail pyramid for token optimization (like Coral's context packing)
2. **Cross-Language Equivalents**: Link Python `bin/guidance-py` to Zig `src/guidance/` via `equivalents[]` field
3. **Usage Extraction**: Call-site snippets during sync (grep for `AstParser.init(` patterns)
4. **Performance Telemetry**: Query frequency tracking for LLM budget prioritization

---

## Comment Management

**Single Source of Truth Principle:**
Member comments (`///`) live in source files—the JSON is a derived artifact. Thiseliminates redundancy, reduces JSON file size by ~30%, and ensures comments are always in sync with code.

### Storage Strategy

| Content | Location | Rationale |
|---------|----------|-----------|
| Module comments (`//!`) | JSON `comment` field | Module-level docs rarely accessed, useful in semantic search |
| Member comments (`///`) | Source file only | Aligned with Zig's design, extracted on-demand |
| `match_hash` | JSON | Tracks signature + comment for staleness detection |
| `comment_generated` | In-memory only | Runtime flag, never persisted |

### Workflow: LLM Comment Generation

```
guidance gen --all-languages
       │
       ├─► AST Extraction → Member extraction with initial match_hash
       │
       ├─► Member Merge → Preserve existing comments, flag new members
       │
       ├─► LLM Enhancement → Generate comments for members missing them
       │                      Sets comment_generated=true, calculates match_hash
       │
       ├─► JSON Write → Save JSON WITHOUT member comments (only match_hash)
       │
       └─► Post-Processing (if any comment_generated=true):
            │
            ├─► Scan JSON for comment_generated=true members
            │
            ├─► Write comments to source files (bottom-up line order)
            │
            ├─► Run zig fmt on modified files
            │
            ├─► Extract comments from source (extractMemberCommentsFromSource)
            │
            └─► Update line numbers and match_hash in memory
```

### Key Invariants

1. **Hash Includes Comment**: `match_hash = SHA-256(signature ++ "|||COMMENT|||" ++ comment)`
   - Comment changes trigger regeneration check
   - Stable comments produce stable hashes

2. **JSON Mtime Strategy**: `json_mtime = source_mtime - 1 second`
   - Prevents spurious reprocessing of unchanged files
   - `fileNeedsProcessing()` skips when JSON mtime == source_mtime - 1

3. **Member Comments Not in JSON**: Extracted from source when JSON is loaded
   - Backward compatible: old JSON files with comments still load
   - `extractMemberCommentsFromSource()` populates in-memory structure

4. **Single Fmt Pass**: After all comment insertions, run `zig fmt` once

5. **Automatic Workflow**: No explicit `--sync-comments` flag needed when LLM is available

### Implementation

| Component | Purpose |
|-----------|---------|
| `types.Member.comment_generated` | Flag tracking LLM-generated comments |
| `hash.computeMemberHash()` | SHA-256 hash including comment with separator |
| `sync.zig` | Sets `comment_generated=true` when LLM generates comment |
| `sync_engine.postProcessCommentSync()` | Scans JSON, writes comments, runs fmt, extracts from source |
| `json_store.extractMemberCommentsFromSource()` | Reads source file and extracts `///` comments by line number |
| `comment_sync.correctLineNumbers()` | Re-parses source and updates JSON line numbers |
| `marker.fileNeedsProcessing()` | Recognizes JSON mtime pattern to skip validated files |

---

## Deterministic-First Strategy

### Query Classification

```
Input Query
      ↓
  [Word Count] ──→ ≤3 words: Fast path (keyword-only)
      ↓
  [Question Pattern] ──→ Starts with "how/what/why/when/where/does": LLM synthesis
      ↓
  [Semantic Alias Match] ──→ Exact alias: Local lookup
      ↓
  [Capability Inference] ──→ Known capability keyword: Load CAPABILITY.md excerpt
      ↓
  [Skill Match] ──→ Pattern detected: Load SKILL.md excerpt
      ↓
  [LLM Synthesis] ──→ Novel query: Local inference + cache result
```

### Cache Hierarchy

| Cache Level | Content | Hit Rate | Latency |
|-------------|---------|----------|---------|
| L1: Deterministic Match | Exact identifier lookup | ~40% | <10ms |
| L2: Semantic Alias | Token expansion | ~20% | <20ms |
| L3: Capability Keyword | AST-level routing | ~15% | <30ms |
| L4: LLM Synthesis | Cached summaries | ~10% | <50ms |
| Miss | Local LLM inference | ~15% | 200-800ms |

---

## RALPH Loop Invariants

### Build Phase
```
zig build guidance  ──→ Produces zig-out/bin/guidance
```
**Invariant:** `$(TARGET_BIN)` depends ONLY on source files — never on `STRUCTURE.md` or guidance JSON.

### Test Phase
```
zig build test --summary all
```
**Invariant:** All unit tests must pass. Exit 1 on failure.

### Lint Phase
```
zig fmt --check src/
```
**Invariant:** Zero formatting violations. Exit 1 on failure.

### Format Phase
```
zig fmt src/
```
**Invariant:** Forms all source files. Non-blocking (formatting fixes applied).

### Guidance Phase
```
guidance gen --all-languages
```
**Invariant:** All source files processed. JSON mtime updated only on success.
**Post-Processing:** When LLM generates comments:
1. Scan JSON for `comment_generated=true` members
2. Write generated comments to source files
3. Run `zig fmt` on modified files
4. Correct line numbers in JSON
5. Recalculate `match_hash` with new comments

### Structure Phase
```
guidance structure
```
**Invariant:** `STRUCTURE.md` regenerated from guidance JSON. Idempotent.

### Database Phase
```
guidance gen (implicit)
```
**Invariant:** `.guidance.db` updated from `.guidance/src/**/*.json`. Ready for `explain` queries.

---

## Integration with Coral Context

guidance and Coral Context share a common architecture foundation. This section defines **what is shared** and **what is guidance-specific**.

### Shared Modules (`src/common/`)

| Module | Origin | Status | Guidance Use |
|--------|--------|--------|--------------|
| `context_packer.zig` | Coral (P3.3) | **Extract** | Stage packing with token budget |
| `token_budget.zig` | Coral (M7.1) | **Extract** | Token estimation (1 tok ≈ 4 bytes) |
| `drift.zig` | Coral (BitSet DRIFT) | **Extract** | Deterministic follow-up generation |
| `embeddings.zig` | Shared | Done | EmbeddingProvider vtable |
| `llm.zig` | Shared | Done | LlmClient HTTP wrapper |
| `registry.zig` | Shared | Done | TargetRegistry, TargetBuilder |
| `interner.zig` | Shared | Done | StringInterner |

### Guidance-Specific Modules (`src/guidance/`)

| Module | Purpose | Notes |
|--------|---------|-------|
| `staged.zig` | Stage collection for explain | Uses ContextPacker for token budget |
| `query_engine.zig` | TIER classification, routing | Uses BitSet DRIFT for follow-ups |
| `vector_db.zig` | SQLite hybrid search | Guidance-specific |
| `sync.zig` | AST sync pipeline | Guidance-specific |
| `llm_filter.zig` | Batch LLM filtering | Replaces per-stage filtering |

### Integration Points

```
Query Classification:
  ┌─────────────────────────────────────────────────────────────┐
  │ TIER 0: Empty → Hot Files (files with most matches    <5ms  │
  │ TIER 1: Identifier → Deterministic Name Lookup        <10ms │
  │ TIER 2: Short (≤2 words) → Semantic Alias + Keyword   <50ms │
  │ TIER 3: Question → ContextPacker → Batch LLM          200ms+│
  └─────────────────────────────────────────────────────────────┘

Context Packing (from Coral):
  ┌─────────────────────────────────────────────────────────────┐
  │ ContextPacker.pack(stages, config)                         │
  │   ├── head_protect: N prose stages (module doc)            │
  │   ├── body: code stages (always) + filtered prose          │
  │   └── tail_protect: M stages (callers, used_by)            │
  │                                                             │
  │ Token Budget: (content.len + 3) / 4                        │
  │ Prose Threshold: relevance >= 0.3                          │
  └─────────────────────────────────────────────────────────────┘

Follow-Up Generation (from Coral):
  ┌─────────────────────────────────────────────────────────────┐
  │ BitSetDrift.generateFollowUps(needed, available)           │
  │   └── missing = needed & ~available                        │
  │   └── for each bit: "Provide {interner.getString(bit)}"    │
  │   └── No LLM call required                                  │
  └─────────────────────────────────────────────────────────────┘
```

---

## Success Metrics

| Metric | Current | Target |
|--------|---------|--------|
| Deterministic resolution rate | ~40% (keyword-only) | >50% |
| Query latency (cached) | <100ms | <50ms |
| Query latency (LLM synthesis) | 500-1500ms | <800ms |
| Token usage for context | Varies | <4000 tokens median |
| RALPH loop duration | ~30s | <20s |
| Staleness detection accuracy | 100% | 100% |
| Cross-language schema parity | ~95% | 100% |

---

## Future Directions

### Near-Term (1-2 Sprints)

1. **Extract Shared Modules to `src/common/`**:
   - `context_packer.zig` → `src/common/context_packer.zig`
   - `token_budget.zig` → already in `src/coral/`, move to `src/common/`
   - `drift.zig` → `src/common/drift.zig`
   - Guidance imports: `const ContextPacker = @import("common").ContextPacker;`

2. **TIER 0/1/2 Query Classification**:
   - TIER 0: Empty query → hot files (highest number of keyword matches)
   - TIER 1: Identifier pattern (`filterStages`, `cache.L1Cache`) → deterministic name lookup, no LLM
   - TIER 2: Short query (≤2 words, no question) → keyword + semantic alias
   - TIER 3: Question pattern (>2 words or `?`) → batch LLM filter + synthesis

3. **Batch LLM Filtering**:
   - Replace per-stage `askRelevant()` with single batch call
   - Gather all prose/insight/skill stages into one prompt
   - Use ContextPacker for token budget before LLM

### Mid-Term (3-4 Sprints)

1. **Hot Files Tracking**:
   - Add `last_queried` timestamp to `ast_nodes` table
   - Implement `listHotFiles(allocator, limit)` returning most recently modified
   - Empty query dispatches to hot files list

2. **Identifier Pattern Detection**:
   - Add `detectIdentifierPattern(query)` returning `?IdentifierMatch`
   - Single word, no spaces → TIER 1 (deterministic)
   - Contains `.` → method/field pattern
   - PascalCase → struct pattern

3. **Cross-Language Equivalents**: Link related implementations

### Long-Term (5+ Sprints)

1. **BitSet DRIFT for Follow-Ups**:
   - Use `drift.zig` for deterministic follow-up generation
   - `needed & ~available` produces exact capability names
   - No LLM call for follow-up expansion

2. **Subagent Protocol**: MCP server for AI coder integration
3. **Continuous Learning**: Cache LLM synthesis results, expire on signature change

---

## Conclusion

guidance is evolving from a code navigation tool into a fully-capable deterministic-first subagent. By sharing architecture with Coral Context and enforcing the RALPH loop, it provides:
- **Zero marginal cost** for cached queries
- **Token-efficient context** for AI coders
- **Human-in-the-loop orchestration** via `guidance check`
- **Transparent caching** through `.guidance.db`

The result is an AI-assisted development tool that grows more capable with every use while remaining fast, deterministic, and auditable.

---

*Vision Document v1.3 — April 2026*

---

## Appendix: Comparative Analysis Summary

### Comment Management Implementation Status

See `doc/guidance/TODO_20260404_STREAMLINE_JSON.md` for full design details.

**Streamlined JSON (v2.0):**
- ✅ Member comments excluded from JSON (`jsonifyMember()` skips comment field)
- ✅ Module-level comments still stored in JSON
- ✅ `extractMemberCommentsFromSource()` extracts comments from source on load
- ✅ `match_hash` still tracks comment changes for staleness detection
- ✅ JSON mtime set to `source_mtime - 1` after successful sync
- ✅ `fileNeedsProcessing()` skips files with validated mtime pattern
- ✅ Backward compatibility: old JSON files with comments still load
- ✅ Database sync extracts member comments from source files

**Eliminates:**
- Spurious LLM calls on unchanged files (detected by mtime pattern)
- Redundant comment storage (30% JSON size reduction)
- Diff noise from comment-only changes in JSON files

### From GraphRAG vs Coral Context Report

**Key synthesized patterns:**
- BitSet distance as unifying primitive for capability routing
- Token-budgeted context assembly before LLM synthesis
- DRIFT-style iterative refinement with deterministic follow-ups

**Applicable to guidance:**
- `ContextPacker` already implements token budget with head/tail protection
- `BitSetDrift` already implements deterministic follow-up generation
- Both are directly reusable from `src/coral/`

### From Hermes Comparison (GUIDANCE_HERMES_COMPARISON.md)

**Identified gaps:**
| Gap | Current | Fix |
|-----|---------|-----|
| No identifier detection | All queries go to LLM if >2 words | TIER 1 for `filterStages` pattern |
| Per-stage LLM filtering | N LLM calls for N prose stages | Batch into single call |
| No token budget | All stages emitted | ContextPacker from coral |
| No empty query handling | Error | Hot files list |

**Implemented fixes:**
- TIER 0/1/2/3 classification (this document)
- Batch LLM filtering (Near-Term #3)
- ContextPacker integration (Near-Term #1)

### From codedb2 (Zig)

**Applicable patterns:**
- **WordIndex**: O(1) inverted index for identifiers — guidance already has semantic-aliases
- **TrigramIndex**: Substring search acceleration — guidance uses SimHash + cosine
- **Hot files**: Recently modified tracking — adopt for TIER 0 (empty query)
- **Version store**: Append-only log — guidance uses `.guidance.db` SQLite

**Not applicable:**
- codedb2's MCP server is similar to Coral's MCP — guidance focuses on CLI/data generation

### From bloop (Rust)

**Applicable patterns:**
- **HyDE fallback**: When semantic search returns <50% results, generate hypothetical docs and retry
- **files_importing()**: Reverse dependency lookup — guidance has `used_by` field
- **Scope graphs**: File-level symbol resolution — guidance has AST parsing

**Not applicable:**
- bloop's HyDE requires frontier LLM — guidance targets local inference
- bloop's regex query planning — guidance uses keyword + vector hybrid