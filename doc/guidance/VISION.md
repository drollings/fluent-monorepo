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
- Zero API cost for deterministic queries (single keyword, no LLM)
- Local LLM synthesis only for novel queries (>1 word or question patterns)
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
┌─────────────────────────────────────────────────────────────────────────┐
│                      QUERY PROCESSING FSM                              │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────┐     ┌─────────┐     ┌─────────┐     ┌─────────┐       │
│  │ INTAKE  │────▶│ CLASSIFY│────▶│  ROUTE  │────▶│VALIDATE │────▶SYNTH │
│  └─────────┘     └─────────┘     └─────────┘     └─────────┘       │
│       │            │            │            │            │                     │
│       ▼            ▼            ▼            ▼            ▼                     │
│  Tokenize     Intent      Search      Result       Prompt                  │
│  detection   extraction   strategy    quality                     │
│                                                                  │
│  States:                                                         │
│  INTAKE   → Parse query into tokens, detect file paths, identify patterns     │
│  CLASSIFY → Classify intent: SINGLE_IDENTIFIER, CAPABILITY_KEYWORD,         │
│           FILE_PATH, CONCEPTUAL, HOW_TO, MULTI_KEYWORD                     │
│  ROUTE    → Select search primitive: WordIndex, AnchorLookup,           │
│           FTS keyword, Hybrid fallback, RRF merge                       │
│  VALIDATE → Verify results: match_hash, relevance threshold,            │
│           anchor verification, SimHash structural match               │
│  SYNTH   → LLM with grounded excerpts                                   │
│                                                                  │
└─────────────────────────────────────────────────────────────────────────┘
```

**Intent Classification Rules:**

| Intent | Detection | Search Path |
|--------|-----------|-----------|
| `single_identifier` | 1 token, matches identifier pattern (CamelCase, snake_case) | WordIndex exact match |
| `capability_keyword` | Token matches capability name in INDEX.md | AnchorLookup |
| `file_path` | Contains `/` or .zig suffix | Direct JSON lookup |
| `how_to` | Starts with question verb | Capability + skill + FTS |
| `conceptual` | Multi-word, no identifier | RRF hybrid |
| `multi_keyword` | Multiple tokens | Per-token matching |

### Component 2: Search Primitives Hierarchy

The query engine implements a **primitives hierarchy** ordered by determinism:

| Rank | Primitive | Implementation | Latency | LLM Required |
|------|-----------|--------------|---------|---------------|
| 1 | **WordIndex** | Inverted index lookup | <1ms | No |
| 2 | **AnchorLookup** | Capability anchors → WordIndex | <5ms | No |
| 3 | **FTS Keyword** | SQLite LIKE + position rank | <10ms | No |
| 4 | **SimHash Filter** | Hamming distance pre-filter | <20ms | No |
| 5 | **RRF Merge** | Reciprocal rank fusion | <30ms | No |
| 6 | **Hybrid Fallback** | Vector + keyword weighted | <100ms | Optional |
| 7 | **Vector Only** | Cosine similarity only | <200ms | Required |

**Fallback Chain:**
- Single identifier → WordIndex → FTS fallback
- Capability keyword → AnchorLookup → FTS anchor fallback → Hybrid
- Conceptual/How-to → RRF merge → Hybrid fallback → Vector only
- No results → ESCALATE with reason

### Component 3: Grounding Enforcement

**Critical Invariant:** No synthesis without source. The LLM must receive verbatim code excerpts, never file paths.

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
| Confidence < 0.3 | `low_confidence` | Use multiple classification |
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
    [Capability Anchor Extraction] ──→ Store anchors for capability lookup
        ↓
    [LLM Enhancement] ──→ Generate missing comments (comment_generated=true)
        ↓
    Guidance JSON ──→ .guidance/src/<path>.json
        ↓
    [Database Sync] ──→ .guidance.db (SQLite + inverted index)
```

**Incremental Design:**
- `fileNeedsProcessing()`: Compare source mtime vs JSON mtime
- `match_hash`: SHA-256 of `signature ++ "|||COMMENT|||" ++ comment`; unchanged = preserve comments
- Per-file processing: `guidance gen --file src/foo.zig`
- Ancor extraction: Populate capability anchors during sync

### Component 8: RALPH Loop (`cmdCheck`)

```
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

### Component 9: Registry Layer (shared with Coral)

```
src/common/
├── registry.zig    ──→ TargetRegistry, TargetBuilder (Fluent Builder)
├── target.zig      ──→ Target, TargetSchema, DynamicEditable
├── interner.zig    ──→ StringInterner (RwLock-protected bitset index)
├── embeddings.zig ──→ EmbeddingProvider VTable
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
2. **Incremental Sync**: `match_hash` comparison, mtime-based staleness detection
3. **Hybrid Search**: SQLite keyword + cosine similarity vector fusion with SimHash pre-filter
4. **Staged Pipeline**: `executeStaged()`, `expandFollowUps()`, `formatStaged()`
5. **Multi-Level Inverted Index**: Semantic aliases, capability keywords, member enrichment
6. **LLM Filtering**: `filterStages()` for relevance pruning
7. **RALPH Loop**: `guidance check` orchestration
8. **Fluent WVR**: Registry, target, interner from `src/common/`
9. **Skills/Capabilities**: Auto-attachment during pattern detection
10. **Capability Routing**: `findMatchedCapabilityNamesForQuery()` in `executeStaged()`
11. **Comment In-filling**: Automatic LLM-generated comment sync
12. **TIER 0/1/2/3 Query Classification**: Query routing by complexity
13. **Per-Token Exact Matching**: Multi-keyword queries with separate excerpts
14. **`--debug` flag**: Pipeline logging and cache tracking
15. **INDEX.md Generation**: Capability listing
16. **codehealth Command**: Dead code detection
17. **Hot Path Allocation Elimination**: Zero-allocation token matching in `staged.zig` via `std.ascii.eqlIgnoreCase`; eliminates O(tokens×results) heap allocations entirely
18. **Query Result Cache**: Session-scoped `QueryCache` (FNV-keyed) integrated into `cmdExplainStaged`; `--no-cache` flag to bypass; `guidance cache-stats --reset` to clear DB synthesis cache

### In Progress 🔄

1. **Intent FSM Implementation**: Replace cosmetic TIER with deterministic state machine
2. **WordIndex Primary**: O(1) inverted index for single-token lookup
3. **Capability Anchors**: Population during sync
4. **Structured JSON Output**: `--output=json` flag

### Planned 📋

1. **Grounding Enforcement**: No synthesis without verbatim source
2. **Escalation Protocol**: Clear failure signals
3. **Token-Budgeted Assembly**: Hard limits on context
4. **RRF Merge**: Replace weighted fusion
5. **Query Telemetry**: Learning from query history
6. **MCP Server**: IDE integration

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

| Cache Level | Content | Hit Rate | Latency |
|-------------|---------|---------|--------|
| L1: WordIndex | Exact identifier | ~40% | <1ms |
| L2: AnchorLookup | Capability anchors | ~20% | <5ms |
| L3: FTS Keyword | Full-text search | ~15% | <10ms |
| L4: RRF Merge | Hybrid fusion | ~10% | <30ms |
| L5: LLM Synthesis | Cached summaries | ~10% | <50ms |
| Miss | Local LLM | ~15% | 200-800ms |

---

## RALPH Loop Invariants

### Build Phase
```
zig build guidance
```
**Invariant:** Binary depends ONLY on source files.

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

### Near-Term (1-2 Sprints)

1. **Intent FSM**: Replace cosmetic TIER with deterministic state machine
2. **--output=json**: Structured JSON for local LLM orchestration
3. **Capability Anchors**: Populate during sync
4. **Grounding Enforcement**: No synthesis without source

### Mid-Term (3-4 Sprints)

1. **WordIndex Primary**: O(1) inverted index
2. **Token-Budget Assembly**: Hard limits
3. **RRF Merge**: Replace weighted fusion
4. **Escalation Protocol**: Clear failure signals
5. **Query Telemetry**: Learning from queries

### Long-Term (5+ Sprints)

1. **Skill Auto-Linking**: Continuous learning
2. **MCP Server**: IDE integration
3. **Hot Files Tracking**: Query frequency
4. **Memory Learning**: Cache optimization

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

---

*Vision Document v2.0 — April 2026*