# Benchmark queries for guidance test

## Each query is followed by `---` and a rubric that defines the expected answer.  The rubric is a concise description of what a correct answer must contain.  cmdTest uses the rubric to judge whether the LLM evaluation is accurate.

---

## Short queries (fast path, deterministic AST match)

cmdExplain

- **Rubric**: Must return the `cmdExplain` function definition from `src/guidance/query_engine.zig` with signature and purpose.

---

src/dag/target.zig

- **Rubric**: Must return information about the DAG target module, specifically the target.zig file with its struct/function definitions.

---

match_hash

- **Rubric**: Must return code locations where `match_hash` is computed (SHA-256 of signature + comment) and explain its role in staleness detection.

---

fileNeedsProcessing

- **Rubric**: Must return the exact `fileNeedsProcessing` function from `src/guidance/sync/marker.zig` showing mtime comparison logic (JSON absent → stale; source newer → stale).

---

JsonStore

- **Rubric**: Must return the JsonStore struct interface from `src/guidance/sync/json_store.zig` listing its 10 public functions (init, loadGuidance, saveGuidance, etc.).

---

GuidanceDb

- **Rubric**: Must return the GuidanceDb struct from `src/vector/vector_db.zig` with its key methods (syncFromDir, syncCapabilities, knnSearch, etc.).

---

sync

- **Rubric**: Must return the SyncProcessor or sync-related code from `src/guidance/sync.zig` showing the incremental sync pipeline workflow.

---

## Natural language queries (LLM synthesis path)

How does filterStages work?

- **Rubric**: Must explain that `filterStages` in `src/guidance/query/llm_filter.zig` uses an LLM to filter prose stages while keeping code/metadata unconditionally; askRelevant is called for each prose stage.

---

How does this code use cosine similarity?

- **Rubric**: Must explain cosine similarity is used in `src/vector/vector_db.zig` (hybridSearch, vector_similarity) to rank search results by combining vector distance with keyword matching.

---

How is the RALPH loop implemented?

- **Rubric**: Must explain the RalphState enum (read/ask/learn/plan/help/done) driving the loop in `src/guidance/ralph.zig`, using enhanceFunction and enhanceModuleDetail for LLM generation.

---

What capabilities does this codebase have?

- **Rubric**: Must list key capabilities: coral-database (SQLite vector DB), sync-pipeline (incremental sync), explain-query (staged pipeline), llm-client (Ollama/OpenAI), target-registry (Fluent Builder), reflection (field-level), rdf-parsing (Turtle/N-Quads).

---

I want to understand how match_hash and fileNeedsProcessing work together to detect staleness.

- **Rubric**: Must explain that fileNeedsProcessing checks mtime (JSON absent or source newer → stale) while match_hash (SHA-256 of signature+comment) tracks content changes; both used to determine whether to re-enhance comments.

---

## Queries that should escalate as unknown (negative tests)

How does the quantum entanglement protocol work in the coral module?

- **Rubric**: Must return "not found" or escalation status because quantum entanglement protocol does not exist in this codebase; results should be empty or show no matches.

---

Show me the implementation of the flux capacitor pattern for warp drive acceleration.

- **Rubric**: Must return "not found" or escalation status because flux capacitor pattern does not exist in this codebase; results should be empty or show no matches.

---

