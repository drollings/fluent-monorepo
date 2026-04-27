# Benchmark queries for guidance test

Each query is followed by `---` and a rubric that defines the expected answer.
The rubric is a concise description of what a correct answer must contain.
cmdTest uses the rubric to judge whether the LLM evaluation is accurate.

---

## Short queries (fast path, deterministic AST match)

cmdExplain

- **Rubric**: Must return the `cmdExplain` function definition from `src/guidance/query_engine.zig` with signature and purpose.

---

src/coral/targets.zig

- **Rubric**: Must return information about the Coral targets module, specifically the targets.zig file with its struct/function definitions.

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

GuidanceDoc

- **Rubric**: Must return the `GuidanceDoc` struct definition from `src/guidance/types.zig` with its field descriptions (meta, comment, members, etc.).

---

JsonStore

- **Rubric**: Must return the JsonStore struct interface from `src/guidance/sync/json_store.zig` listing its 10 public functions (init, loadGuidance, saveGuidance, etc.).

---

apiHash

- **Rubric**: Must return the `apiHash` function from `src/guidance/hash.zig` used for computing SHA-256 hashes of function signatures.

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

How does the staged query pipeline work?

- **Rubric**: Must explain the 4-phase pipeline: Phase 1 exact name match, Phase 2 keyword index search, Phase 3 hybrid search, Phase 4 seeAlsoExpand; original_query for determinism, query_text for semantics.

---

How is the RALPH loop implemented?

- **Rubric**: Must explain the RalphState enum (read/ask/learn/plan/help/done) driving the loop in `src/guidance/ralph.zig`, using enhanceFunction and enhanceModuleDetail for LLM generation.

---

What capabilities does this codebase have?

- **Rubric**: Must list key capabilities: coral-database (SQLite vector DB), sync-pipeline (incremental sync), explain-query (staged pipeline), llm-client (Ollama/OpenAI), target-registry (Fluent Builder), reflection (field-level), rdf-parsing (Turtle/N-Quads).

---

How does the staged query pipeline work and how does it integrate with the RALPH loop?

- **Rubric**: Must explain both the 4-phase staged pipeline AND how ralph.zig orchestrates the loop states; Phase 1 uses original_query for exact match, then hybrid search, then seeAlsoExpand.

---

The VectorDb class implements cosine similarity search for the guidance database, but how does it handle hybrid queries?

- **Rubric**: Must explain that hybridSearch in `src/vector/vector_db.zig` combines 0.65×vector + 0.35×keyword scores; falls back to keyword-only when vector unavailable; returns results sorted by descending hybrid score.

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

