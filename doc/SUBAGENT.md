# REVIEW_20260418_LOCAL_SUBAGENT.md

## Local Model Subagent Architecture for Frontier Orchestration

**Date:** 2026-04-18
**Subject:** Ollama-based subagent design for deterministic-first frontier model orchestration

---

## 1. Paradigm Shift: Search Engine vs. Subagent

The prior synthesis (`REVIEW_20260418_AIDER_CODEDB_COZO_SYNTHESIS.md`) assumes guidance's internal pipeline uses local computation for deterministic resolution. That's **internal search engine logic**.

A fundamentally different use case: **local model as a subagent** that returns structured output to a frontier orchestrator. The local model is not the tool — it's a **prompted function** that outputs parseable JSON.

```
Traditional:  Frontier → Local Model → Output (unstructured)
Subagent:     Frontier → Prompt + Schema → Local Model → Structured JSON → Frontier
                                           ↑
                                   Prompt includes grounding data
```

The subagent must behave like a **deterministic function**:
- Same input → same structured output
- Parseable schema guarantees downstream parsing
- Escalation protocol when stuck
- Grounding data always included in context

---

## 2. The Subagent Problem

Frontier models want to:
1. **Delegate** a subtask to a capable subagent
2. **Receive** structured output the frontier can act on
3. **Escalate** when subagent can't resolve

Local models (Ollama) want to:
1. **Receive enough context** to generate accurate output (no检索)
2. **Output in known schema** the frontier expects
3. **Know when to fail** vs. guess

The gap: Current local model prompts give the model free rein to generate prose. The subagent pattern constrains output to schemas the frontier can parse and act on.

---

## 3. Output Schema Architecture

### 3.1 Core Response Schemas

Every subagent prompt must include a JSON schema the model outputs:

```json
// Schema 1: Intent Classification
{
  "intent": "IDENTIFIER | CAPABILITY | CONCEPTUAL | HOW_TO | FILE_PATH | UNKNOWN",
  "confidence": 0.85,
  "tokens": ["filter", "stages"],
  "anchors_detected": ["filterStages", "dupeStage"]
}

// Schema 2: Retrieval Result
{
  "results": [
    {
      "path": "src/guidance/staged.zig",
      "line": 45,
      "identifier": "filterStages",
      "relevance": 0.92,
      "excerpt": "pub fn filterStages(allocator: std.mem.Allocator, stages: []const Stage) ![]Stage {"
    }
  ],
  "count": 3,
  "exhausted": false
}

// Schema 3: Validation Result
{
  "valid": true,
  "checks": [
    {"check": "match_hash_unchanged", "passed": true},
    {"check": "relevance_threshold", "passed": true, "threshold": 0.3, "actual": 0.92},
    {"check": "anchor_verification", "passed": true}
  ],
  "reason": null
}

// Schema 4: Synthesis Result
{
  "summary": "filterStages returns stages matching .code or .prose kind",
  "citations": [
    {"file": "src/guidance/staged.zig", "line": 45, "role": "definition"},
    {"file": "src/guidance/staged.zig", "line": 89, "role": "caller"}
  ],
  "gaps": ["filterStages performance characteristics not shown"]
}

// Schema 5: Escalation
{
  "status": "ESCALATE",
  "reason": "no_anchor_hits",
  "suggested_capability": "guidance-query",
  "frontier_action": "perform_hybrid_search"
}
```

### 3.2 Schema Selection Strategy

The frontier model selects which schema to request:

```
Frontier decision tree:
  │
  ├─► Need to classify query type? → Request Schema 1 (Intent)
  │
  ├─► Need to find code? → Request Schema 2 (Retrieval)
  │   Context: keyword tokens, capability name
  │
  ├─► Need to verify results? → Request Schema 3 (Validation)
  │   Context: retrieval results + match_hash
  │
  ├─► Need explanation? → Request Schema 4 (Synthesis)
  │   Context: validated results + source excerpts + skills
  │
  └─► Can't resolve? → Request Schema 5 (Escalate)
```

---

## 4. Prompt Chaining Architecture

### 4.1 Three-Stage Chain

The subagent uses **prompt chaining** where each stage's output feeds the next:

```
┌─────────────────────────────────────────────────────────────────┐
│  STAGE 1: CLASSIFY INTENT                                       │
│  Input: raw query string                                       │
│  Prompt: Classify this query into intent type                  │
│  Output: Schema 1 (Intent)                                     │
│  Next: If confident → STAGE 2; else → ESCALATE                 │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  STAGE 2: RETRIEVE GROUNDING                                    │
│  Input: intent + tokens                                        │
│  Prompt: Find source excerpts matching tokens                   │
│  Output: Schema 2 (Retrieval)                                  │
│  Next: If results → STAGE 3; else → ESCALATE                    │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  STAGE 3: SYNTHESIZE                                            │
│  Input: validated results                                      │
│  Prompt: Summarize using ONLY the provided excerpts              │
│  Output: Schema 4 (Synthesis)                                   │
└─────────────────────────────────────────────────────────────────┘
```

### 4.2 Per-Stage Prompt Templates

**Stage 1: Intent Classification**

```
You are a query classifier. Output ONLY valid JSON.

Query: {query}

Classify into one of: IDENTIFIER, CAPABILITY, CONCEPTUAL, HOW_TO, FILE_PATH, UNKNOWN

Output JSON with fields:
- intent: the classification
- confidence: 0.0-1.0 your confidence
- tokens: array of query tokens
- anchors_detected: any capability anchors you recognize

Output:
```

**Stage 2: Retrieval**

```
You are a code retrieval subagent. Output ONLY valid JSON.

Context (you cannot access files, only use this context):
{excerpts}

Query tokens: {tokens}
Intent: {intent}

Find all matching code excerpts for these tokens.
Output each match with:
- path: file path
- line: line number
- identifier: the matched symbol name
- relevance: 0.0-1.0 relevance score
- excerpt: the actual source line (max 80 chars)

Output JSON:
{{
  "results": [
    {{"path": "...", "line": N, "identifier": "...", "relevance": 0.N, "excerpt": "..."}}
  ],
  "count": N,
  "exhausted": true|false
}}

Output:
```

**Stage 3: Synthesis**

```
You are a code explanation subagent. Output ONLY valid JSON.

Query: {query}
Intent: {intent}

Source excerpts (use ONLY these, do not hallucinate):
{excerpts}

Capability context:
{capability_description}

Related skills:
{skill_names}

Output a summary and citations.
Use field names: summary, citations[], gaps[]

Output JSON:
{{
  "summary": "...",
  "citations": [{{"file": "...", "line": N, "role": "definition|caller|reference"}}],
  "gaps": ["any information not in excerpts"]
}}

Output:
```

**Stage 4: Escalation**

```
You are a subagent that failed to resolve. Output ONLY valid JSON.

Query: {query}
Intent detected: {intent}
Why you failed: {reason}

Suggest what the frontier model should do next.
Output:
{{
  "status": "ESCALATE",
  "reason": "...",
  "suggested_capability": "...",
  "frontier_action": "..."
}}

Output:
```

---

## 5. Grounding Data Protocol

### 5.1 The Critical Problem

Local models cannot检索 — they don't have database access. The prompt must include all grounding data:

```
WRONG:  "Explain how filterStages works"
        (model hallucinates from training data)

RIGHT: "Explain this code using ONLY the excerpts below:

        Excerpt 1 (src/guidance/staged.zig:45):
        pub fn filterStages(allocator: std.mem.Allocator, stages: []const Stage) ![]Stage {
            var result = std.ArrayList(Stage).init(allocator);
            for (stages) |stage| {
                if (stage.kind == .code or stage.kind == .prose) {
                    try result.append(stage);
                }
            }
            return result.toOwnedSlice();
        }

        Explain what this does."
```

### 5.2 Grounding Data Injection

The subagent prompt includes **verbatim** source, not embeddings or file names:

```zig
fn injectGrounding(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    tokens: []const []const u8,
) ![]const u8 {
    var grounding = std.ArrayList(u8).init(allocator);

    for (tokens) |token| {
        const hits = try db.wordIndexSearch(token);
        for (hits[0..@min(3, hits.len)]) |hit| {
            const excerpt = try extractSourceExcerpt(allocator, hit.path, hit.line);
            try grounding.writer.print(
                "Excerpt {d} ({s}:{d}):\n{s}\n\n",
                .{ hit.index, hit.path, hit.line, excerpt }
            );
        }
    }

    return grounding.toOwnedSlice();
}
```

**Extraction rules:**
- Maximum 80 characters per excerpt line
- Maximum 5 excerpts per token
- Include line numbers for citation
- Include file path for reference
- No ellipsis — include actual code

### 5.3 Capability Context Injection

```zig
fn injectCapabilityContext(
    allocator: std.mem.Allocator,
    capability: []const u8,
) ![]const u8 {
    // Load from capability-mapping.json
    const cap = try getCapability(capability);

    return try std.fmt.allocPrint(
        allocator,
        \\ Capability: {s}
        \\ Description: {s}
        \\ Anchors: {s}
        \\ Skills: {s}
        ,
        .{ cap.name, cap.description, cap.anchors, cap.skills }
    );
}
```

---

## 6. Escalation Protocol

### 6.1 When to Escalate

The local model must know when it **cannot** resolve:

| Condition | Escalation Reason |
|-----------|-----------------|
| No anchor hits | `no_anchor_hits` |
| Confidence < 0.3 | `low_confidence` |
| Zero retrieval results | `no_results` |
| Multi-file query exceeds context | `context_overflow` |
| Unknown capability | `unknown_capability` |

### 6.2 Escalation Output

```json
{
  "status": "ESCALATE",
  "reason": "no_anchor_hits",
  "query": "filterStages dupeStage complexMerge",
  "tokens_extracted": ["filterStages", "dupeStage"],
  "suggested_capability": "guidance-staged-query",
  "frontier_action": "perform_hybrid_search",
  "why_frontier_should_handle": "Query requires multi-token matching beyond word-index capacity"
}
```

### 6.3 Frontier Handling of Escalation

The frontier model interprets escalation:

```
When receives {"status": "ESCALATE", "reason": "X"}:
  │
  ├─► reason == "no_anchor_hits" →
  │      Try capability keyword expansion, retry
  │
  ├─► reason == "low_confidence" →
  │      Use multiple intent classifications, pick dominant
  │
  ├─► reason == "no_results" →
  │      Fall back to hybrid vector search
  │
  ├─► reason == "context_overflow" →
  │      Reduce token count, re-prompt with subset
  │
  └─► reason == "unknown_capability" →
         Let frontier determine capability from description
```

---

## 7. Ollama-Specific Considerations

### 7.1 Model Selection

| Model | Use Case | Latency | Context |
|-------|---------|--------|--------|
| llama3:8b | Classification, light synthesis | ~500ms | 8K |
| mistral:7b | Retrieval, validation | ~400ms | 8K |
| codellama:7b | Code synthesis | ~600ms | 4K |
| phi3:3b | Fallback, simple queries | ~200ms | 4K |

**Recommendation:** Use model cascade:
- Classification → phi3 (fast, ~200ms)
- Retrieval → mistral (balanced)
- Synthesis → llama3 (quality)

### 7.2 Context Window Limits

Ollama models have limited context. The grounding injection must respect:

```
Prompt = {classifier_prompt} + {schema_instructions} + {grounding_data}
                              ↑
                        Budget: 2000 tokens max for grounding
```

**Token budgeting per stage:**
- Stage 1 (classify): 200 tokens
- Stage 2 (retrieve): 2000 tokens grounding
- Stage 3 (synthesize): 2000 tokens grounding + 500 tokens context

### 7.3 Structured Output Enforcement

Ollama models produce JSON but need enforcement:

```
Technique 1: JSON mode (if supported)
  ollama run phi3 --json ...

Technique 2: Pydantic-style prompt
  Output a JSON object with these exact fields:
  - field_name: field_type  // description

Technique 3: Output parsing with fallback
  1. Parse model output as JSON
  2. If parse fails, extract between { and }
  3. If still fails, return escalation
```

---

## 8. Frontier Orchestration Flow

### 8.1 The Orchestrator Pattern

The frontier model maintains **subagent orchestration state**:

```
┌─────────────────────────────────────────────────────────────────┐
│  FRONTIER MODEL                                                 │
│  State: query, intent, tokens, results, stage                     │
│                                                                 │
│  function call_subagent(prompt, schema):                         │
│    // 1. Build prompt with grounding                             │
│    // 2. Call Ollama with prompt                                │
│    // 3. Parse output as JSON                                 │
│    // 4. Check for escalation                                 │
│    // 5. Update state                                        │
│    // 6. Return result or escalate                            │
└─────────────────────────────────────────────────────────────────┘
```

### 8.2 State Machine in Frontier

```python
def orchestrate(query: str) -> dict:
    state = {"query": query, "stage": "classify", "results": []}

    # Stage 1: Classify
    prompt = build_classify_prompt(query)
    output = call_subagent(prompt, "Intent")
    state["intent"] = output["intent"]
    state["tokens"] = output["tokens"]

    if output["confidence"] < 0.3:
        return escalate(state, "low_confidence")

    # Stage 2: Retrieve grounding
    grounding = inject_grounding(state["tokens"])
    prompt = build_retrieve_prompt(state, grounding)
    output = call_subagent(prompt, "Retrieval")
    state["results"] = output["results"]

    if output["count"] == 0:
        return escalate(state, "no_results")

    # Stage 3: Synthesize
    prompt = build_synthesize_prompt(state)
    output = call_subagent(prompt, "Synthesis")

    return {
        "summary": output["summary"],
        "citations": output["citations"],
        "query": query,
        "stages": state
    }
```

### 8.3 Frontier Decision Points

The frontier model decides when to iterate vs. escalate:

| Local Output | Frontier Action |
|-------------|----------------|
| `intent: IDENTIFIER, confidence: 0.9` | Continue to retrieval |
| `intent: UNKNOWN, confidence: 0.2` | Re-classify with different prompt |
| `results: [], exhausted: false` | Try expanded tokens/escalate |
| `results: [...], exhausted: true` | Continue to synthesis |
| `status: ESCALATE` | Handle escalation, maybe do hybrid |

---

## 9. Implementation Architecture

### 9.1 Module Design

```
src/
  guidance/
    subagent/
      schema.zig         // Output schema definitions
      prompt_builder.zig // Per-stage prompt templates
      caller.zig        // Ollama HTTP client
      parser.zig         // JSON output parsing
      escalation.zig     // Escalation handling
      orchestrator.zig  // Frontier orchestration
```

### 9.2 Schema Definition

```zig
pub const IntentSchema = struct {
    intent: Intent,
    confidence: f64,
    tokens: []const []const u8,
    anchors_detected: []const []const u8,
};

pub const RetrievalSchema = struct {
    results: []const RetrievalHit,
    count: usize,
    exhausted: bool,
};

pub const SynthesisSchema = struct {
    summary: []const u8,
    citations: []const Citation,
    gaps: []const []const u8,
};

pub const EscalationSchema = struct {
    status: EscalationStatus,
    reason: []const u8,
    suggested_capability: ?[]const u8,
    frontier_action: []const u8,
};
```

### 9.3 Prompt Builder

```zig
pub fn buildClassifyPrompt(query: []const u8) []const u8 {
    return std.fmt.comprint(
        \\ You are a query classifier. Output ONLY valid JSON.
        \\
        \\ Query: {s}
        \\
        \\ Classify into: IDENTIFIER, CAPABILITY, CONCEPTUAL, HOW_TO, FILE_PATH, UNKNOWN
        \\ Output: {{"intent": "...", "confidence": 0.N, "tokens": [...], "anchors_detected": [...]}}
        \\
        \\ Output:
        , .{query}
    );
}
```

---

## 10. Expected Outcomes

| Metric | Target | Mechanism |
|--------|--------|-----------|
| Subagent latency | <1500ms | 3-stage chain, each ~500ms |
| Schema parse rate | >90% | JSON mode + fallback parsing |
| Escalation rate | <15% | Capability anchoring |
| Frontier token savings | >80% | Local does retrieval |
| Hallucination rate | Zero | Grounding always included |

---

## 11. Key Differences from Internal Pipeline

| Aspect | Internal Pipeline (Prior Review) | Subagent (This Review) |
|--------|--------------------------------|------------------------|
| User | guidance CLI | Frontier orchestrator |
| Output | Markdown to stdout | JSON schemas |
| LLM involvement | Final synthesis stage | Per-stage calls |
| Validation | Internal check | Model outputs validation JSON |
| Escalation | Not applicable | Critical protocol |
| Context | Local database access | Grounding injected in prompt |

---

## 12. Current Output Analysis: cmdExplain Quality Review

### 12.1 What guidance Provides Well

Running `guidance explain "cmdExplain"` produces this output:

```markdown
# Explain: cmdExplain

This function parses command-line arguments to configure and execute an explanation tool
that analyzes code queries by routing them through capability, file, or struct classification
before performing database searches and optionally generating LLM-synthesized summaries...

## Source location: `src/guidance/query_engine.zig:482-784`

```zig
pub fn cmdExplain(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var ea: ExplainArgs = .{};
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-l") or std.mem.eql(u8, arg, "--limit")) {
            // ... full 300+ line implementation
        }
    }
    // ... complete function body
}
```

## Capability: explain-query

**Anchors**: cmdExplain, executeStaged, executeStagedWithAliases, formatStaged
**Sources**: src/guidance/query_engine.zig (1.0), src/guidance/staged.zig (1.0)...

## Knowledge Base

**READ BEFORE IMPLEMENTING**
- **gof-patterns**: Documentation of 12 Gang of Four (GoF) design patterns...

## References

- **Recommended search command**: `guidance explain "resolveLlmConfigForThinking"`
- **Other terms to search**: `isExactNameMatchPub`, `loadSkillsFromJsonPub`...
- **Matched capabilities**: `explain-query`, `sync-pipeline`, `rdf-parsing`
- **Files used most in**: `src/guidance/main.zig`, `src/guidance/mcp.zig`...
- **Skills**: `gof-patterns`
```

**Strengths of current output:**

| Aspect | Quality | Example |
|--------|---------|---------|
| **Complete code snippets** | ✅ Excellent | Full function `cmdExplain` with all lines 482-784 |
| **File:line citations** | ✅ Excellent | `src/guidance/query_engine.zig:482-784` |
| **Capability anchors** | ✅ Excellent | `Anchors: cmdExplain, executeStaged...` |
| **See-also recommendations** | ✅ Excellent | Specific search commands |
| **Skills linkage** | ✅ Excellent | `gof-patterns` with doc link |
| **Statistically-ranked files** | ✅ Excellent | `Files used most in: ...` (not just matched) |

### 12.2 Gaps for Subagent Integration

Current output is **unstructured markdown** — needs transformation for subagent use:

| Gap | Current | Subagent Needs |
|-----|---------|----------------|
| **No JSON schema** | Markdown output | `{"intent": "...", "confidence": 0.N, ...}` |
| **No explicit confidence** | Implicit (1.0, 0.9) in capability | Explicit relevance scores per hit |
| **No exhausted flag** | Implicit (all shown) | `exhausted: true|false` |
| **No gaps field** | Missing | `gaps: ["performance not shown"]` |
| **No escalation** | "Not indexed" text | `{"status": "ESCALATE", "reason": "..."}` |
| **No validation checks** | N/A | `{"checks": [{"check": "...", "passed": true}]` |
| **See-also unstructured** | Free text | `["Recommended search command": "...", ...]` |

### 12.3 Specific Improvements Needed

**1. Add formal output schema to CLI output:**

```bash
--output=json  # Returns structured JSON instead of markdown
```

Add flag that changes output format:

```
guidance explain "cmdExplain" --output=json
```

Returns:

```json
{
  "query": "cmdExplain",
  "intent": "IDENTIFIER",
  "confidence": 0.95,
  "summary": "Parses command-line arguments to configure...",
  "results": [
    {
      "path": "src/guidance/query_engine.zig",
      "line_start": 482,
      "line_end": 784,
      "identifier": "cmdExplain",
      "relevance": 1.0,
      "excerpt": "pub fn cmdExplain(allocator: std.mem.Allocator, args: []const []const u8) !void {..."
    }
  ],
  "capabilities": [
    {
      "name": "explain-query",
      "anchors": ["cmdExplain", "executeStaged"],
      "sources": ["src/guidance/query_engine.zig", "src/guidance/staged.zig"]
    }
  ],
  "skills": ["gof-patterns"],
  "see_also": [
    {"type": "search_command", "query": "resolveLlmConfigForThinking"},
    {"type": "file", "path": "src/guidance/main.zig"}
  ],
  "files_used_most": ["src/guidance/main.zig", "src/guidance/mcp.zig"],
  "exhausted": true,
  "gaps": []
}
```

**2. Add explicit validation section:**

Current output doesn't show:
- `match_hash` staleness
- Confidence threshold vs actual
- Anchor verification status

Add to output:

```json
"validation": {
  "match_hash_unchanged": true,
  "relevance_threshold": {"threshold": 0.3, "actual": 1.0, "passed": true},
  "anchor_verification": {"expected": ["cmdExplain"], "found": ["cmdExplain"], "passed": true}
}
```

**3. Improve "gaps" field:**

Current output doesn't indicate what's NOT covered. Add:

```json
"gaps": [
  "LLM synthesis model configuration not shown",
  "Error handling paths not detailed"
]
```

**4. Add escalation for no-results:**

When query has no matches:

```
guidance explain "nonexistentSymbol123"
```

Currently outputs text:
```
Not indexed for 'nonexistentSymbol123'. Search the source directly:
```

Should return structured:

```json
{
  "status": "ESCALATE",
  "reason": "no_results",
  "query": "nonexistentSymbol123",
  "suggested_actions": [
    {"action": "grep", "command": "grep -ri 'nonexistentSymbol123' src/"},
    {"action": "gen", "command": "guidance gen --force"}
  ]
}
```

**5. Improve see-also to structured format:**

Current:
```markdown
## References
- **Recommended search command**: `guidance explain "resolveLlmConfigForThinking"`
- **Other terms to search**: `isExactNameMatchPub`, `loadSkillsFromJsonPub`...
```

Structured:

```json
"see_also": [
  {"type": "query", "value": "resolveLlmConfigForThinking", "priority": "recommended"},
  {"type": "query", "value": "isExactNameMatchPub", "priority": "other"},
  {"type": "file", "value": "src/guidance/main.zig", "role": "entry_point"}
]
```

---

## 13. Conclusion

The subagent pattern transforms Ollama from a **generative model** into a **deterministic function** with:

1. **Schema-selected output** — Frontier knows what to parse
2. **Prompt chaining** — Each stage prepares for next
3. **Grounding always included** — No hallucinations possible
4. **Escalation protocol** — Clear failure boundaries
5. **Model cascade** — Fast classification, quality synthesis

**Current guidance provides excellent grounding (complete code snippets, file:line citations, capability anchors)** — the subagent work is adding the JSON schema layer on top and formalizing what already exists.

The frontier model orchestrates, the local model executes. Both are needed: frontier for planning and orchestration, local for retrieval and structured synthesis within bounded context.

**Priority improvements for subagent integration:**
1. Add `--output=json` flag to `guidance explain`
2. Add explicit confidence and validation sections
3. Add formal escalation on no-results
4. Add gaps field to indicate uncovered areas
5. Structure see-also into typed array

---

*Review completed 2026-04-18. Updated with cmdExplain output analysis.*