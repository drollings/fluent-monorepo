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

## The Closed Maintenance Cycle

The core purpose of guidance is a **closed incremental loop** that keeps codebase navigation indexes current with every change:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        GUIDANCE CLOSED LOOP                                  │
└─────────────────────────────────────────────────────────────────────────────┘

   Source Files (.zig, .py)
          │
          ▼
   [1] AST Parsing + Member Extraction
          │  std.zig.Ast / ast.parse
          │  match_hash = SHA-256(signature + comment)
          ▼
   [2] Incremental JSON Sync
          │  fileNeedsProcessing(src_mtime, json_mtime)
          │  match_hash unchanged → preserve existing comments
          │  json_mtime = src_mtime - 1 second (validated marker)
          ▼
   [3] .guidance/src/**/*.json   (metadata mirrors, NOT source of truth)
          │
          ▼
   [4] Database Sync (GuidanceDb.syncFromDir)
          │  Walk .guidance/src/ → upsert into .guidance.db
          │  Embed cosine similarity vectors
          │  Populate capability_sources, fts_inverted_index
          ▼
   [5] Query Explain (guidance explain "query")
          │  SQLite keyword + vector hybrid search
          │  Staged pipeline: code excerpts + prose + metadata
          ▼
   Codebase Navigation
```

**Each phase has change detection built in** — the loop runs incrementally, not from scratch:

| Phase | Change Detection | DRY Enforcement |
|--------|-----------------|-----------------|
| AST Parsing | Extract fresh match_hash | Source `///` comments not stored in JSON |
| Incremental Sync | `fileNeedsProcessing()` mtime comparison | LLM enhancement only when comment missing |
| JSON Write | Skip if `hash unchanged AND line numbers stable` | Preserve existing tags/patterns |
| Database Sync | Only upsert changed nodes (by path + mtime) | Comments extracted from source on query |
| Query Explain | Return cached results for repeated queries | Never regenerate without source change |

The loop is **complete and operational** — all phases are implemented in the current codebase. The roadmap targets performance enhancements (WordIndex O(1) lookup, git-aware snapshots) and capability extensions (centroid classification, RRF merge), not missing cycle phases.

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
- Zero API cost for deterministic queries (single keyword, no LLM)
- Local LLM synthesis only for novel queries (>1 word or question patterns)
- Full auditability through `.guidance.db` query log

### Goal 2: RALPH Loop Orchestration

The `guidance check` command enforces the RALPH loop. Each phase runs **incrementally** — only files detected stale by `fileNeedsProcessing()` are reprocessed:

```
build → test (skipped if test_passed marker current) → lint → fmt → guidance gen (stale files only) → structure → db
```

This is deterministic waterfall execution: each stage must pass before the next begins. The `guidance check` command is the pre-commit hook entry point that ensures codebase integrity before any commit.

**Key Invariants — all fully implemented:**
- **JSON mtime is the universal "all stages passed" marker**: A file's JSON mtime advances only when all phases (test→lint→fmt→guidance) have succeeded for that file
- **Source mtime > JSON mtime = stale file requiring re-sync**: `fileNeedsProcessing()` in `marker.zig:51` detects this
- **JSON mtime = source_mtime - 1 second = validated marker**: `touchFileAfter()` sets this pattern; `fileNeedsProcessing()` skips validated files
- **`match_hash` preservation**: Hash unchanged → preserve existing comments/tags/patterns (`json_store.zig:552`)
- **`guidance gen` processes only stale files**: `gen_files.zig:866` calls `fileNeedsProcessing()` per file
- **`guidance gen --force` reprocesses all**: Bypasses staleness checks
- **`comment_generated` flag tracks LLM-generated comments**: Set by enhancer, used by comment sync phase to write back to source

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
- `--output=json`: Structured JSON for local LLM output to control deterministic workflow

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

### Goal 5: Subagent Structured Output

guidance serves as a deterministic-first subagent providing structured output to frontier orchestrators:

```
guidance explain "query" --output=json
  ↓
{
  "intent": "IDENTIFIER",
  "confidence": 0.95,
  "results": [...],
  "summary": "...",
  "citations": [...],
  "gaps": [...]
}

guidance explain "nonexistent" --output=json
  ↓
{
  "status": "ESCALATE",
  "reason": "no_results",
  "suggested_capability": "...",
  "frontier_action": "..."
}
```

---

## Architecture

### Component 1: Query Processing FSM

The query engine implements a **finite state machine** for deterministic routing. This replaces the cosmetic TIER classification with actual state transitions:

```
┌────────────────────────────────────────────────────────────────────────────────────────┐
│                                  QUERY PROCESSING FSM                                  │
├────────────────────────────────────────────────────────────────────────────────────────┤
│                                                                                        │
│  ┌────────┐    ┌──────────┐    ┌───────┐    ┌──────────┐    ┌──────────┐    ┌───────┐  │
│  │ INTAKE │───▶│ CLASSIFY │───▶│ ROUTE │───▶│ VALIDATE │───▶│ ASSEMBLE │───▶│ SYNTH │  │
│  └────────┘    └──────────┘    └───────┘    └──────────┘    └──────────┘    └───────┘  │
│       │            │              │            │               │               │       │
│       ▼            ▼              ▼            ▼               ▼               ▼       │
│  Tokenize        Intent         Search       Result          Token        Prompt LLM   │
│  detection       + Domain      strategy      quality         budget        synthesis   │
│                   extraction                                                           │
│                                                                                        │
│  States:                                                                               │
│  INTAKE   → Parse query into tokens, detect file paths, detect identifiers             │
│  CLASSIFY → Classify intent: SINGLE_IDENTIFIER, CAPABILITY, FILE_PATH,                 │
│             HOW_TO, CONCEPTUAL, MULTI_KEYWORD; Extract domain                          │
│             via SimHash centroid matching (on WordIndex miss only)                     │
│  ROUTE    → Select search primitive: WordIndex, AnchorLookup,                          │
│             FTS keyword, Hybrid fallback, RRF merge                                    │
│  VALIDATE → Verify results: match_hash, relevance threshold,                           │
│             anchor verification, SimHash structural match                              │
│  ASSEMBLE → Token-budgeted stage collection: HEAD/BODY/TAIL with binary search         │
│  SYNTH    → LLM with grounded excerpts (only if confidence < 0.7)                      │
│                                                                                        │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

**QueryClass — Three-Dimensional Classification:**

The FSM produces three classification dimensions, not just intent:

```zig
pub const QueryClass = struct {
    intent: Intent,           // IDENTIFIER, CAPABILITY, HOW_TO, CONCEPTUAL, etc.
    domain: []const u8,       // "coral", "guidance", "dag", "common", "llm"
    confidence: f64,          // 0.0-1.0
};
```

**Domain/Subdomain Classification (via Centroid Matching):**

When WordIndex misses (no exact identifier match), the FSM computes the query SimHash and compares it against pre-computed capability centroids to classify domain:

| Centroid Match | Domain | Subdomain | Confidence |
|----------------|--------|-----------|------------|
| High proximity (hamming ≤ 3) | From centroid | From centroid | 0.85-0.94 |
| Medium proximity (hamming 4-7) | From centroid | From centroid | 0.70-0.84 |
| Low proximity (hamming 8-10) | From centroid | null | 0.50-0.69 |
| No match (hamming > 10) | Best effort | null | < 0.50 |

**Intent Classification Rules:**

| Intent | Detection | Domain/Subdomain | Search Path |
|--------|-----------|------------------|-------------|
| `single_identifier` | 1 token, CamelCase/snake_case | From centroid or null | WordIndex exact match |
| `capability_keyword` | Token matches capability name | From capability mapping | AnchorLookup |
| `file_path` | Contains `/` or .zig suffix | From path | Direct JSON lookup |
| `how_to` | Starts with question verb | From centroid or null | FTS + skill docs |
| `conceptual` | Multi-word, no identifier | From centroid or null | SimHash + FTS |
| `multi_keyword` | Multiple tokens | From centroid or null | Per-token WordIndex → RRF |

### Component 2: Search Primitives Hierarchy

The query engine implements a **primitives hierarchy** ordered by determinism, with centroid-based domain classification as the bridge between WordIndex and broader search:

| Rank | Primitive | When | Implementation | Latency | LLM Required |
|------|-----------|------|--------------|---------|---------------|
| 1 | **WordIndex** | Always tried first | Inverted index lookup (exact identifier match) | <1ms | No |
| 2 | **Centroid Classification** | On WordIndex miss | SimHash centroid matching for domain | <20ms | No |
| 3 | **AnchorLookup** | When domain classified | Capability anchors → domain-limited WordIndex | <5ms | No |
| 4 | **FTS Keyword** | Domain-limited or fallback | SQLite LIKE + position rank | <10ms | No |
| 5 | **SimHash Filter** | After FTS | Hamming distance structural validation | <20ms | No |
| 6 | **RRF Merge** | Multi-keyword | Reciprocal rank fusion | <30ms | No |
| 7 | **Hybrid Fallback** | After RRF | Vector + keyword weighted | <100ms | Optional |
| 8 | **Vector Only** | Last resort | Cosine similarity only | <200ms | Required |

**Activation Flow:**

```
Query
  │
  ▼
WordIndex exact match?
  │
  ├─ YES → Return result (confidence 0.95, deterministic path)
  │
  └─ NO → Compute query SimHash
             │
             ▼
        Compare against capability centroids (SimHash hamming distance)
             │
             ▼
        Domain/Subdomain classified?
             │
             ├─ YES → Route to domain-limited AnchorLookup + FTS
             │         (confidence 0.70-0.94 based on centroid proximity)
             │
             └─ NO → Fall through to FTS → RRF → Hybrid → Vector
                      (escalation candidate if confidence < 0.7)
```

**Centroid Storage:**

Pre-computed at `guidance gen` index time:

```sql
CREATE TABLE capability_centroids (
    id TEXT PRIMARY KEY,             -- capability name
    domain TEXT NOT NULL,            -- "coral", "guidance", etc.
    simhash BLOB NOT NULL,           -- 256-bit SimHash of all members
    member_count INTEGER,
    intent_domain TEXT,              -- "how-to", "conceptual", "identifier"
    computed_at INTEGER,
    version INTEGER DEFAULT 1
);
```

**Fallback Chain:**
- Single identifier → WordIndex exact match → (miss) → Centroid classification → domain-limited search
- Capability keyword → AnchorLookup → FTS anchor fallback → Hybrid
- Conceptual/How-to → Centroid classification → RRF merge → Hybrid fallback → Vector only
- No results → ESCALATE with reason

### Component 3: Grounding Enforcement

**Critical Invariant:** No synthesis without source. The LLM must receive verbatim code excerpts, never file paths.

**Escalation Threshold:** Confidence < 0.7 triggers structured escalation to the frontier orchestrator.

**Grounding Protocol:**
- Extract source excerpts for matched identifiers
- Include full function/struct body, not summaries
- Always include line numbers for citation
- Prompt enforces: "Use ONLY the provided source excerpts"
- Prompt fallback: "If information is not in excerpts, say not shown"

**Excerpt Extraction:**
- Find declaration start (beginning of function/struct)
- Find declaration end (matching brace)
- Extract signature from docstring
- Return with file:line references

### Component 4: Structured Output Schemas

guidance supports multiple output modes:

| Mode | Flag | Output Format | Use Case |
|------|------|-------------|---------|
| **Legacy** | (default) | Markdown | Human reading |
| **JSON** | `--output=json` | Structured JSON | Local LLM orchestration |
| **Compact** | `--output=compact` | JSON, abbreviated | Token savings |
| **Debug** | `--output=debug` | JSON + metadata | Development |

**Schema Definitions:**

| Schema | Fields | Use |
|--------|--------|------|
| **IntentSchema** | intent, confidence, tokens, anchors_detected | Query classification |
| **RetrievalSchema** | results[], count, exhausted | Code retrieval |
| **ValidationSchema** | valid, checks[], reason | Result verification |
| **SynthesisSchema** | summary, citations[], gaps | LLM synthesis output |
| **EscalationSchema** | status, reason, suggested_capability, frontier_action | Failure signaling |

### Component 5: Escalation Protocol

When the deterministic workflow cannot resolve, it signals failure clearly:

| Condition | Reason | Frontier Action |
|-----------|--------|---------------|
| No anchor hits | `no_anchor_hits` | Try capability keyword expansion |
| Confidence < 0.7 | `low_confidence` | Use multiple classification |
| Zero results | `no_results` | Fall back to hybrid search |
| Context overflow | `context_overflow` | Reduce tokens, retry |
| Unknown capability | `unknown_capability` | Let frontier determine |

**Escalation Output:**
```json
{
  "status": "ESCALATE",
  "reason": "no_anchor_hits",
  "query": "filterStages complexMerge",
  "suggested_capability": "guidance-staged-query",
  "frontier_action": "perform_hybrid_search"
}
```

### Component 6: Token-Budgeted Assembly

Replace greedy stage collection with budget-aware selection:

```
Stage Collection with Budget:
  1. Estimate tokens per stage: (content_len + 3) / 4
  2. Sort by relevance score
  3. Binary search for largest subset fitting budget
  4. Apply head/tail protection
  5. Return optimal subset
```

**Context Hierarchy:**

| Level | Content | Tokens |
|--------|---------|--------|
| **HEAD** | Capability intro + skill refs | 500 |
| **BODY** | Source excerpts + metadata | Budget - head - tail |
| **TAIL** | Citations + see-also | 300 |

### Component 7: Sync Pipeline (`SyncProcessor`)

The sync pipeline is a **closed incremental loop** — each source file is processed independently, and only stale files are touched:

```
Source File (.zig/.py/.md/.json)
        │
        ▼
    [Plugin Registry] ─→ Extension for each file type
        │
        ▼
    AST Parser (Zig: std.zig.Ast, Python: ast.parse)
        │
        ▼
    Member Extraction ──→ members[] with signatures, line numbers
        │
        ▼
    [match_hash Check] ──→ Hash unchanged? Preserve existing comments/tags/patterns
        │
        ▼
    [Pattern Detection] ──→ Auto-attach GoF/domain patterns
        │
        ▼
    [Capability Anchor Extraction] ──→ Mark `is_anchor` on members matching anchor names
        │
        ▼
    [LLM Enhancement] ──→ Generate missing comments (comment_generated=true)
        │
        ▼
    Guidance JSON ──→ .guidance/src/<path>.json
        │
        ▼
    [Database Sync] ──→ .guidance.db (SQLite + vector embeddings)
```

**Incremental Design — fully implemented:**
- `fileNeedsProcessing(src_abs, json_abs)`: JSON absent → stale; JSON mtime < source mtime → stale; JSON mtime = source_mtime - 1s → validated, skip
- `match_hash`: SHA-256 of `signature ++ "|||COMMENT|||" ++ comment`; unchanged = preserve existing comments/tags/patterns (json_store.zig:552)
- Per-file processing: `guidance gen --file src/foo.zig` processes only the named file
- Line number correction: `lines_corrected` counter when AST reports different line than stored (json_store.zig:557)
- Comment source of truth: `///` doc comments live in source, NOT in JSON (sync.zig:217-220)

**Key Invariant — No DRY Violation:**
Member-level doc comments (`///`) are extracted from source at query time, never stored in JSON. The JSON is a metadata mirror — the source file is the authoritative record. This means:
- `guidance gen` never overwrites source comments
- LLM enhancement writes back to source via the comment sync phase, not the JSON phase
- JSON remains minimal (no comment redundancy, cleaner diffs)

### Component 8: RALPH Loop (`cmdCheck`)

The RALPH loop is the **human-in-the-loop gate** before commits. It runs incrementally — only files detected stale by `fileNeedsProcessing()` are reprocessed:

```
pub fn cmdCheck(allocator, args) !void {
    // 1. Build (zig build)
    // 2. Test (zig build test --summary all)  — skipped if test_passed marker is current
    // 3. Lint (zig fmt --check)
    // 4. Format (zig fmt)
    // 5. Guidance gen (ONLY stale files, via fileNeedsProcessing check)
    // 6. Structure (regenerate STRUCTURE.md)
    // 7. Database (sync .guidance.db via syncFromDir)
}
```

**Incremental behavior:**
- `guidance gen --all-languages` calls `processFiles()` which iterates all source files and calls `fileNeedsProcessing()` per file
- Only stale files (JSON absent or source newer than JSON) are processed
- `guidance gen --force` bypasses staleness checks and reprocesses everything

**Failure Modes:**
- `error.TestFailed`: Exit 1, print test output
- `error.LintFailed`: Exit 1, print lint violations
- `error.ParseError`: Continue with warning (unguidable file)

### Component 9: Registry Layer (shared with Coral)

The registry layer provides shared building blocks used by both guidance and coral:

```
src/common/
├── registry.zig    ──→ TargetRegistry, TargetBuilder (Fluent Builder)
├── target.zig      ──→ Target, TargetSchema, DynamicEditable
├── interner.zig    ──→ StringInterner (RwLock-protected bitset index)
├── embeddings.zig ──→ EmbeddingProvider VTable (embeddinggemma via Ollama)
└── llm.zig       ──→ LlmClient (Ollama/OpenAI HTTP client)
```

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
    anchors: []const []const u8,    // Capability anchor identifiers
    hashtags: []const []const u8,    // Discovery hashtags
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
    is_anchor: bool = false,    // True if this is a capability anchor
};
```

**Comment Storage Strategy:**
- **Module-level comments (`//!`)**: Stored in JSON's top-level `comment` field
- **Member comments (`///`)**: Extracted from source on-demand, never stored in JSON
- **Rationale**: Keeps JSON minimal, reduces diff noise, aligns with Zig's design
- **Backward compatibility**: Old JSON files with member comments still load correctly
- **`match_hash`**: Still tracks comment changes for staleness detection

### Capability Mapping

```json
{
  "name": "coral-database",
  "anchors": ["Db", "Library", "ContextNode", "knnSearch"],
  "keywords": ["database", "db", "storage", "persist"],
  "description": "SQLite-backed vector database with hybrid search",
  "skills": ["fluent-wvr"],
  "files": ["src/coral/db.zig", "src/coral/database.zig"]
}
```

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
    content: []const u8,      // Owned
    source: []const u8,     // Owned: "src/foo.zig"
    line: ?u32,          // Source line number
    relevance: f64 = 1.0, // Relevance score for ranking
};
```

### SearchResult (Database Row)

```sql
CREATE TABLE ast_nodes (
    id INTEGER PRIMARY KEY,
    file_path TEXT NOT NULL,
    source TEXT NOT NULL,
    name TEXT NOT NULL,
    node_type TEXT NOT NULL,
    signature TEXT,
    comment TEXT,
    detail TEXT,
    keywords TEXT,
    line INTEGER,
    embedding BLOB,
    match_hash TEXT,
    used_by TEXT,
    skills TEXT,
    capabilities TEXT,
    is_anchor INTEGER DEFAULT 0
);

CREATE TABLE fts_inverted_index (
    token TEXT NOT NULL,
    file_path TEXT NOT NULL,
    member_name TEXT,
    position INTEGER,
    PRIMARY KEY (token, file_path, member_name, position)
);

CREATE INDEX fts_token_idx ON fts_inverted_index(token);

CREATE TABLE query_telemetry (
    id INTEGER PRIMARY KEY,
    query TEXT NOT NULL,
    intent TEXT NOT NULL,
    results_found INTEGER,
    escalation_reason TEXT,
    latency_ms INTEGER,
    timestamp INTEGER
);
```

---

## Current Implementation Status

### Completed ✅

1. **AST Parsing**: Zig (`std.zig.Ast`) and Python (`ast.parse`) with plugin registry
2. **Incremental Sync**: `match_hash` comparison (`json_store.zig:552`), mtime-based staleness detection (`marker.zig:51`), validated-file mtime pattern (`touchFileAfter()`)
3. **Hybrid Search**: SQLite keyword + cosine similarity vector fusion (`vector_db.zig`)
4. **Staged Pipeline**: `executeStaged()`, `expandFollowUps()`, `formatStaged()` (`staged.zig`)
5. **Multi-Level Inverted Index**: Semantic aliases, capability keywords, member enrichment (`vector_db.zig:expandTokens`)
6. **LLM Filtering**: `filterStages()` for relevance pruning (`query/llm_filter.zig`)
7. **RALPH Loop**: `guidance check` → build→test→lint→fmt→guidance→structure→db (`sync/ralph.zig:cmdCheck`)
8. **Fluent WVR**: Registry, target, interner from `src/common/`
9. **Skills/Capabilities**: Auto-attachment during pattern detection (`sync.zig:394`)
10. **Capability Routing**: `findMatchedCapabilityNamesForQuery()` in `executeStaged()` (`staged.zig`)
11. **Comment In-filling**: Automatic LLM-generated comment sync to source (`comments/sync.zig`)
12. **TIER 0/1/2/3 Query Classification**: Query routing by complexity (`query/identifier.zig`)
13. **Per-Token Exact Matching**: Multi-keyword queries with separate excerpts (`staged.zig:anyResultIsRelevant`)
14. **`--debug` flag**: Pipeline logging and cache tracking (`enhancer.zig`)
15. **INDEX.md Generation**: Capability listing (`structure.zig`)
16. **codehealth Command**: Dead code detection (`codehealth/`)
17. **Hot Path Allocation Elimination**: Zero-allocation token matching in `staged.zig` via `std.ascii.eqlIgnoreCase`; eliminates O(tokens×results) heap allocations entirely
18. **Query Result Cache**: Session-scoped `QueryCache` (FNV-keyed) integrated into `cmdExplainStaged`; `--no-cache` flag to bypass; `guidance cache-stats --reset` to clear DB synthesis cache
19. **match_hash Comment Preservation**: Hash unchanged → preserve existing comments/tags/patterns (`json_store.zig:mergeMember`)
20. **leaked_prompt Detection**: Filter LLM preambles from stored comments (`json_store.zig:164`)
21. **Line Number Correction**: `lines_corrected` counter when AST line numbers shift (`json_store.zig:557`)
22. **test_passed Marker**: Skip test phase when marker is newer than all source files (`marker.zig:testsCanBeSkipped`)
23. **GUIDANCE.md Integration**: `guidance init` updates AGENTS.md with guidance integration section (`sync_engine.zig:92-146`)

### In Progress 🔄

1. **Intent FSM Implementation**: Replace cosmetic TIER with deterministic state machine (INTAKE→CLASSIFY→ROUTE→VALIDATE→ASSEMBLE→SYNTH)
2. **WordIndex Primary**: O(1) inverted index for single-token lookup (replaces SQL LIKE table scan)
3. **Capability Anchors**: Population during sync (mark `is_anchor` on members matching anchor names)
4. **RRF Merge**: Replace weighted fusion with reciprocal rank fusion
5. **Grounding Enforcement**: Formal `canSynthesize()` check before LLM synthesis
6. **Structured JSON Output**: IntentSchema, RetrievalSchema, ValidationSchema, SynthesisSchema, EscalationSchema

### Planned 📋

1. **Token-Budgeted Assembly**: Binary search for optimal subset fitting budget, head/tail protection
2. **Escalation Protocol**: Clear failure signals with `suggested_capability` / `frontier_action` fields
3. **Query Telemetry**: Learning from query history
4. **MCP Server**: IDE integration
5. **Git-Aware Snapshot**: Snapshot persistence for <1s index loading on `guidance check` startup
6. **Persistent Query Cache**: TTL-based disk-backed cache for LLM synthesis results
7. **Centroid Classification**: SimHash centroid matching for domain routing on WordIndex miss

---

## Comment Management

**Single Source of Truth Principle:**
Member comments (`///`) live in source files—the JSON is a derived artifact. This eliminates redundancy, reduces JSON file size by ~30%, and ensures comments are always in sync with code.

### Storage Strategy

| Content | Location | Rationale |
|---------|----------|-----------|
| Module comments (`//!`) | JSON `comment` field | Module-level docs useful in search |
| Member comments (`///`) | Source file only | Aligned with Zig's design |
| `match_hash` | JSON | Tracks signature + comment |
| `comment_generated` | In-memory only | Runtime flag |

### Workflow: LLM Comment Generation

```
guidance gen --all-languages
       │
       ├─► AST Extraction → Member extraction with initial match_hash
       │
       ├─► Member Merge → Preserve existing comments, flag new members
       │
       ├─► Capability Anchor Extraction → Identify anchors
       │
       ├─► LLM Enhancement → Generate comments (comment_generated=true)
       │
       ├─► JSON Write → Save WITHOUT member comments
       │
       └─► Post-Processing (if any comment_generated=true):
            ├─► Scan for comment_generated=true members
            ├─► Write comments to source
            ├─► Run zig fmt
            └─► Update line numbers and match_hash
```

### Key Invariants

1. **Hash Includes Comment**: `match_hash = SHA-256(signature ++ "|||COMMENT|||" ++ comment)`
2. **JSON Mtime Strategy**: `json_mtime = source_mtime - 1 second`
3. **Member Comments Not in JSON**: Extracted during load
4. **Single Fmt Pass**: After all comment insertions

---

## Deterministic-First Strategy

### Query Classification FSM

```
Input Query
      ↓
[INTAKE] → Parse tokens, detect patterns
      ↓
[CLASSIFY] → Intent: SINGLE_IDENTIFIER | CAPABILITY | FILE_PATH | HOW_TO | CONCEPTUAL
      ↓
[ROUTE] → Select search primitive: WordIndex → AnchorLookup → FTS → RRF → Hybrid → Vector
      ↓
[VALIDATE] → match_hash valid? relevance >= threshold? anchors present?
      ↓
[SYNTHESIZE] → LLM with grounded excerpts
```

### Cache Hierarchy

The cache hierarchy reflects the closed loop's incremental nature — query results at each level are reused before escalating to more expensive operations:

| Cache Level | Content | Hit Rate | Latency | Source |
|-------------|---------|---------|---------|--------|
| L1: WordIndex | Exact identifier | ~40% | <1ms | TODO: word_index.zig (planned) |
| L2: AnchorLookup | Capability anchors | ~20% | <5ms | GuidanceDb (current: SQL LIKE) |
| L3: FTS Keyword | Full-text search | ~15% | <10ms | SQLite LIKE + position rank |
| L4: RRF Merge | Hybrid fusion | ~10% | <30ms | Weighted fusion (RRF planned) |
| L5: LLM Synthesis | Cached summaries | ~10% | <50ms | llm_cache table in .guidance.db |
| Miss | Local LLM | ~15% | 200-800ms | enhancer.zig |

**Key insight:** L1 WordIndex (planned) is the highest-value target — 40% of queries are exact identifier matches that can be answered in <1ms without any LLM call. This directly reduces the "Miss" row from ~15% to ~5%, making the system dramatically faster for the majority of queries.

---

## RALPH Loop Invariants

### Build Phase
```
zig build guidance
```
**Invariant:** Binary depends ONLY on source files — never on STRUCTURE.md or markers that themselves depend on the binary.

### Test Phase
```
zig build test --summary all
```
**Invariant:** All unit tests must pass.

### Lint Phase
```
zig fmt --check src/
```
**Invariant:** Zero formatting violations.

### Format Phase
```
zig fmt src/
```
**Invariant:** Forms all source files.

### Guidance Phase
```
guidance gen --all-languages
```
**Invariant:** JSON mtime updated only on success.

### Structure Phase
```
guidance structure
```
**Invariant:** `STRUCTURE.md` regenerated idempotently.

### Database Phase
```
guidance gen
```
**Invariant:** `.guidance.db` ready for queries.

---

## Integration with Coral Context

### Shared Modules

| Module | Guidance Use |
|--------|------------|
| `context_packer.zig` | Stage packing with token budget |
| `token_budget.zig` | Token estimation |
| `drift.zig` | Deterministic follow-up generation |
| `embeddings.zig` | EmbeddingProvider vtable |
| `llm.zig` | LlmClient HTTP wrapper |
| `registry.zig` | TargetRegistry |
| `interner.zig` | StringInterner |

### Query Pipeline

```
Query Classification:
  ┌────────────────────────────────────────────────────────┐
  │ INTAKE   → Tokenize, detect file paths       <5ms    │
  │ CLASSIFY → Single ID / Capability / File / How  <10ms   │
  │ ROUTE   → Primitive selection             <15ms   │
  │ VALIDATE → Result verification         <20ms   │
  │ SYNTH   → Grounded LLM               200ms+ │
  └────────────────────────────────────────────────────────┘

Context Packing:
  ┌──────────────────────────────────────────┐
  │ ContextPacker.pack(stages, config)       │
  │   ├── head: prose (500 tokens)        │
  │   ├── body: code + filtered prose    │
  │   └── tail: citations (300 tokens) │
  │ Token Budget: (len + 3) / 4           │
  └──────────────────────────────────────────┘
```

---

## Success Metrics

| Metric | Current | Target |
|--------|--------|--------|
| Deterministic resolution | ~40% | >60% |
| Query latency (WordIndex) | N/A | <5ms |
| Query latency (cached) | <100ms | <50ms |
| Query latency (LLM) | 500-1500ms | <800ms |
| Token usage median | Varies | <4000 |
| RALPH loop | ~30s | <20s |
| Schema valid output | N/A | 100% |
| Escalation clarity | N/A | 100% |

---

## Future Directions

The closed loop is complete. The roadmap targets performance and capability enhancements, organized by impact priority.

### Priority 1 — Query Speed (immediate runtime impact)

1. **WordIndex O(1)**: Replace SQL LIKE table scans with inverted word index (`src/common/word_index.zig`). Target: <1ms per exact identifier lookup, eliminating LLM synthesis for 40% of queries.
2. **Persistent Query Cache**: TTL-based disk-backed cache for LLM synthesis results. Target: Zero marginal cost for repeated queries.

### Priority 2 — Startup Speed

3. **Git-Aware Snapshot**: Snapshot persistence for <1s index loading on `guidance check` startup. Target: Eliminate full filesystem scan when snapshot git HEAD matches current HEAD.
4. **Memory-Mapped Trigram Index**: `src/common/trigram_index.zig` with mmap'd storage. Target: Zero RSS after load; OS page cache serves all searches.

### Priority 3 — Routing Intelligence

5. **Intent FSM**: Replace cosmetic TIER with deterministic INTAKE→CLASSIFY→ROUTE→VALIDATE→ASSEMBLE→SYNTH state machine.
6. **Centroid Classification**: SimHash centroid matching for domain routing on WordIndex miss.
7. **Grounding Enforcement**: Formal `canSynthesize()` check before LLM synthesis.
8. **RRF Merge**: Replace weighted fusion with reciprocal rank fusion.

### Priority 4 — Future Scale

9. **Two-Tier Content**: Drawer/closet architecture for topic pointer compression. Target: Less redundant content in drawers for large codebases.
10. **Reverse Dependency Index**: `getImportedBy()` / `getTransitiveDependents()` via `src/common/dep_graph.zig`.
11. **Query Telemetry**: Learning from query history.
12. **MCP Server**: IDE integration.

---

## Conclusion

guidance evolves into a deterministic-first subagent that transforms local LLM execution from generative model into bounded computation unit:

- **FSM routing** replaces cosmetic classification with actual state transitions
- **WordIndex primary** provides O(1) lookup for common queries
- **Grounding enforcement** eliminates hallucination
- **Structured output** enables frontier orchestration
- **Escalation protocol** defines failure boundaries
- **Token-budget assembly** ensures predictable context

The architecture achieves the vision through pattern convergence:
- Primitives over models (local computation covers 80%+ of queries)
- Cached results for zero marginal cost
- Auditability through query telemetry
- Continuous learning through skill auto-linking

The result is an AI-assisted development tool that grows more capable with every use while remaining fast, deterministic, and auditable.
