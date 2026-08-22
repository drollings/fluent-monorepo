# Needle — the cheapest structured rung in Coral Router (design)

Date: 2026-08-20 (updated 2026-08-22). Status: **landed + enhanced** — the
single-worker queue, FFI wiring, routing-window gate, `output_template` direct
responses, general-category classifier fallback, bounded tool plans with real
`ToolLookup` resolvers, and the closed-loop eval harness are all live (see §11
and §12). The DAG workflow library remains a deferred follow-up roadmap (see
§7).

This document is the design for integrating **Needle 2** — a 45M-parameter /
~28MB-RAM native-engine tool-calling model — as the cheapest structured rung
of Coral Router.

---

## 1. Decisions locked in this design

| # | Decision |
|---|---|
| D1 | **`libneedle.so` built from `libneedle.a`, FFI in-process. No sidecar, no OpenAI-oid endpoint for Needle.** Tight integration, zero IPC. |
| D2 | **Single worker fed by a queue.** Needle is a single global engine with sticky weights; exactly one completion runs at a time. A dedicated worker thread owns the FFI, fed by a bounded queue. |
| D3 | **Stateless single-shot adjudication.** Needle is consulted once per bounded choice point (route, chart, provider `NarrowOne`, argument binding). Rust owns the DAG and the rounds. Needle is never a sessionful co-process (its single global 256-token pinned-KV session would collide across concurrent requests). |
| D4 | **Needle's internal contrastive head is the tool/route shortlister.** The external `HnswToolRetriever` is removed from the default tool path (it duplicated a built-in capability and forced a second embedding model). `candidates_per_rung ≤ 5`. |
| D5 | **HNSW pre-fetch is optional, for very large ContentNode ledgers only.** It narrows a big graph to top-k; Needle then adjudicates. Engaged past a configurable size threshold, never by default for the small route catalogue. |
| D6 | **ContentNode embeddings still use the existing `embedding_model` endpoint.** Needle's head lives in its own contrastive space and cannot embed ledger text into the ledger's vector space. Two distinct retrieval spaces: tools (Needle head) vs. knowledge (ledger embedder). |
| D7 | **Fixed, bounded rounds.** DAG construction has a configurable `max_rounds` (default 3). VISION: "terminate, don't loop." |
| D8 | **All three seams share one queue.** Pre-filter rung, chart adjudicator, and the tree `backend: "needle"` classifier nodes all submit through a single cap-1 worker. |
| D9 | **Fallback is skip, never error.** Any Needle failure (unavailable, gate, decline, low confidence, queue overflow/timeout) emits `Skipped` and falls through to the classifier / more capable models. |

---

## 2. Architecture

```text
                 ┌────────────────────────────────────────────────┐
   HTTP handler ─▶ │ PipelineOrchestrator (sync WorkUnit::execute) │
                 └────────────────────────────────────────────────┘
        │                                  │                    │
        ▼                                  ▼                    ▼
   NeedlePreFilter                 chart adjudicator     tree backend:"needle"
        └───────────────┐                 │                    │
                        ▼                 ▼                    ▼
                 ┌──────────────────────────────────────┐
                 │        NeedleQueue  (cap-1 worker)    │
                 │  bounded channel ─▶ worker thread     │
                 │  oneshot reply + per-call timeout     │
                 └──────────────────────────────────────┘
                                        │
                                        ▼
                            NativeNeedleEngine (FFI)
                            libneedle.so + needle2.cact
```

Needle is **non-generative**: it answers with one grammar-constrained JSON tool
call and a calibrated `confidence`. It is the decision *engine*; ordering,
reachability, and execution stay deterministic Rust (`DependencyGraph`,
`TargetRegistry`, `TargetWorkUnit`).

### 2.1 Why a dedicated worker thread (not an async pool)

The pipeline drives stages through the **synchronous**
`StageDecisionProducer::evaluate` / `WorkUnit::execute` contract
(`fluent-wvr` purity: no blocking I/O, no `block_on`). `LlmRequestQueue` /
`ResultPool` are async and cannot be awaited from that sync seam.

The clean resolution is a **single OS thread that owns the FFI engine**, fed by
a bounded channel with a per-request `oneshot` reply and a wall-clock timeout.
This gives the queue, backpressure, and timeout the async pool would give,
while preserving the sync `NeedleBackend` trait so all three seams and the
hermetic `MockNeedleBackend` keep working unchanged. The `ENGINE_LOCK` becomes
redundant (one thread owns the engine) and is removed from the production path.

If a later iteration makes stage evaluation async, this thread can be replaced
by a cap-1 `ResultPool`/`LlmRequestQueue`-style worker with no change to the
trait or the seams.

---

## 3. The single-worker queue (`needle::queue::NeedleQueue`)

```rust
/// A `NeedleBackend` that serializes completions through one worker thread.
pub struct NeedleQueue {
    tx: std::sync::mpsc::SyncSender<Job>,   // bounded → backpressure
    available: Arc<AtomicBool>,
}
struct Job {
    text: String,
    tools_json: String,
    max_new_tokens: i32,
    reply: std::sync::mpsc::SyncSender<Result<NeedleEnvelope, NeedleError>>,
}
```

- `NeedleQueue::new(inner: Arc<dyn NeedleBackend>, config)` spawns one worker
  thread: `recv → inner.complete → reply.send`.
- `NeedleBackend::complete` submits a `Job` and blocks on the reply `recv_timeout`
  (config `timeout_ms`). A full queue yields `NeedleError::Unavailable`
  (backpressure → skip). A timeout yields `NeedleError::Complete`.
- `is_available` reflects a shared atomic (worker alive + engine loaded).
- `reset` forwards a reset job to the worker.

Hermetic tests cover: serialization (two concurrent submits run sequentially),
backpressure (full queue → error, never block forever), timeout, and reply
propagation — all against `MockNeedleBackend`, no real engine.

---

## 4. Retrieval: internal head first, HNSW optional

### 4.1 Tool/route catalogue (default, D4)

- `candidates_per_rung` default **5**.
- At ≤5, every candidate is grammar-rendered; **Needle's internal contrastive
  head is the shortlister** (it embeds tools once, embeds the query per turn,
  renders top-5, rebuilds the grammar over that subset).
- The `HnswToolRetriever` is **not** wired to the tool path by default. If the
  route catalogue legitimately exceeds 5, the rung falls through to the
  classifier (never a silently truncated set).

### 4.2 Very large ContentNode ledger (optional, D5/D6)

- For per-session ledgers past a configurable node-count threshold, an
  `HnswIndex` over the shared node embeddings pre-fetches top-k nodes
  (reusing the existing `embedding_model` `/v1/embeddings` provider —
  `ContentNodeStore` embeddings + `fluent_db::hnsw::HnswIndex`).
- Needle then **adjudicates** the best fit from that top-k (its grammar +
  confidence gate).
- Below the threshold, `ContentNodeStore::knn_search` (brute force) remains the
  default — no HNSW machinery engaged.

The two retrieval spaces stay separate and each keeps its own acceptable error
rate and update cadence, per VISION §Ledger.

---

## 5. Role split across the components

| Component | Role | Engaged |
|---|---|---|
| **Needle internal head** | Tool/route shortlisting + structured adjudication (grammar + confidence) | small catalogue / choice points |
| **Ledger embedder** (`embedding_model`) | ContentNode embeddings | all ledger writes |
| **HNSW** (`HnswIndex`) | Optional top-k pre-fetch over very large ledgers | size-gated only |
| **`DependencyGraph`/`DependencySession`** | Deterministic DAG ordering, rounds, checkpoint/rewind | orchestrates |
| **Local LLM** | Sanity-check workflow relevance; validate the filled template at the end | expensive, last |

---

## 6. FFI wiring and the `libneedle.so` build

### 6.1 ABI (matches `vendor/needle/linux-x86_64/needle.h`, already bound)

```text
int  needle_init(const char *system, const char *tools_json, const char *tool_index_path);
int  needle_complete(const char *text, int max_new_tokens, char *buf, int buf_len);
void needle_reset(void);
int  needle_load(const unsigned char *cact, unsigned long long n);  // tuned weights
```

### 6.2 Build recipe (verified to produce a correct-symbol `.so`)

The shipped `libneedle.a` was compiled against **LLVM libc++** (`std::__1::`),
so linking requires `-lc++ -lc++abi` and a machine/container with those
runtimes. The static members must be force-included:

```sh
g++ -shared -o libneedle.so \
    -Wl,--whole-archive vendor/needle/linux-x86_64/libneedle.a -Wl,--no-whole-archive \
    -fPIC -lc++ -lc++abi -pthread
```

This yields a ~17MB `.so` exporting `needle_init`/`needle_complete`/
`needle_load`/`needle_reset`. The `.so` must be shipped with `libc++.so` on the
runtime path (or linked statically into it). `bin/build-libneedle.sh` codifies
this; `make libneedle` invokes it. A prebuilt `.so` for a supported platform
may be checked in beside the `.a`.

### 6.3 Weights

- The base model is embedded in the engine; `needle_load(needle2.cact)` is only
  for tuned weights. Loading tuned weights is **sticky for the process**
  (the engine cannot unload), so it happens exactly once at boot from
  `needle.weights`, and the base-model-first ordering constraint from the
  Python bindings applies (bind base before any tuned agent).

---

## 7. DAG workflow (deferred roadmap) — the VISION "neurosymbolic loop"

Not built this iteration; the seams it needs are:

1. **Workflow store**: successful `query → steps → solution` chains stored as
   `ContentNode`s keyed by an embedding of the query (VISION §Escalation
   post-processing `workflow_extraction`).
2. **HNSW over the knowledge cache** → top-k ContentNodes → **Needle adjudicates
   the best fit** (D5/D6), confidence-gated.
3. **`DependencyGraph`/`DependencySession`** orders the DAG and drives the
   rounds, **capped at `max_rounds`** (D7). Needle is consulted only at choice
   points; it never orders or derives reachability.
4. **Local LLM sanity checks**: (a) relevance of a DAG workflow's description
   to the needed solution, (b) validation of the filled template before using
   it as the basis for output.
5. **Promotion loop**: workflows built with frontier help become independent as
   they are tested and promoted — the "frontier-call frequency trending down"
   metric of VISION.

The current iteration ships D1–D9 and leaves the store/replay/promotion seams
as the next roadmap.

---

## 8. Config (`needle` block in `env/coral-router.json`)

```jsonc
"needle": {
  "enabled": true,
  "engine": "vendor/needle/linux-x86_64/libneedle.so",  // built per §6.2
  "weights": "data/needle-opt/needle_routes_v10.cact",   // 5-tool tuned
  "pipeline": "default",
  "min_command_chars": 4,
  "max_command_chars": 512,
  "max_input_tokens": 1024,
  "confidence_threshold": 0.7,                            // re-derived on v10 (precision-coverage)
  "decline_on_missing_confidence": true,
  "timeout_ms": 2000,
  "candidates_per_rung": 5,        // D4: exactly the 5-tool catalogue, head stays off
  "max_rounds": 3,                 // D7 + bounded tool plans
  "shortlist": { "mode": "none" },
  "schema_overrides": {
    // 5 functionally-disjoint tools + local (general, excluded from grammar)
    "code":      { "description": "Programming — write/debug/explain code" },
    "summarize": { "description": "Condense text" },
    "explore":   { "description": "Search / navigate / look up / pull a value", "output_template": "Found: {value}" },
    "explain":   { "description": "Deep answer: translate / analyze / reason / NER / graph+tool+chart lookup" },
    "prose":     { "description": "Creative writing" },
    "local":     { "general": true }
  },
  "tool_plans": {                 // live: dispatch-only + real Lookup resolvers
    // "explore": { "max_rounds": 3, "steps": ["identify_subject:dispatch", "lookup_graph:lookup(knowledge_graph)", "compose:compose"] }
    // "explain": { "max_rounds": 3, "steps": ["identify_subject:dispatch", "lookup_chart:lookup(chart)", "compose:compose"] }
  }
}
```

See §12 for the live tool-plan surface: `Lookup` steps are backed by read-only
`ToolLookup` resolvers (`dag` / `knowledge_graph` / `chart` / `entity_tool` /
`data_store`) installed at boot from the stores the deployment configures; a
plan whose `Lookup` kind has no resolver is declined to plain group dispatch,
never executed with placeholder text. The shipped config carries the
`explain`/`explore` plans above.

Route taxonomy is **functional, not subject-matter**: `code` / `summarize` /
`explore` / `explain` / `prose` plus `local` (general Q&A, classifier
territory, never in the engine grammar). Subject routing (translation, NER,
science/medical/legal, search, API, data-store, knowledge-graph, charts) is
handled as bounded subagent plans dispatched after tool selection, not as
separate top-level routes.

---

## 9. Testing

- **Hermetic** (`make router-test`): `NeedleQueue` serialization/backpressure/
  timeout against `MockNeedleBackend`; stage gate/decline/route/fall-through;
  no real engine, no network.
- **Live** (`make test-live`, `#[ignore]` + `live-ai`): loads the built
  `libneedle.so` (via `NEEDLE_LIB_PATH`), runs one grammar-constrained
  completion, asserts structural invariants. **Requires libc++** on the runtime
  path; skips cleanly when unavailable.

---

## 10. Milestones for this iteration

- M1: `libneedle.so` build recipe + `make libneedle` + config `engine`/`weights`
  docs. *(D1, §6)*
- M2: `NeedleQueue` single-worker primitive + hermetic tests. *(D2, §3)*
- M3: Wire the queue into all three seams (pre-filter, chart, tree), share one
  worker. *(D8)*
- M4: Internal-head tool path; `candidates_per_rung` default 5; HNSW out of
  the default tool path. *(D4)*
- M5: `max_rounds` config scaffold. *(D7)*
- M6: Full gate + live `.so` test on a libc++ machine. *(§9)*

---

## 11. Enhance roadmap (2026-08-21): Needle as the primary router

Status: **landed** (see
[`ROADMAP_20260821_NEEDLE_ENHANCE.md`](../../ROADMAP_20260821_NEEDLE_ENHANCE.md)).
This iteration turns the rung into the **primary source of routing between
models**, with a defined decision window, direct (template) tool responses, a
classifier fallback for the general category, full audit parity with LLM
routing, and an end-to-end evaluation loop driven by `make router-mock` and
`make router-benchmark`.

### 11.1 The decision window (Milestone 1)

Needle decides on the **routing window**, not the whole prompt: the first
sentence **or** first paragraph, up to `ROUTING_WINDOW_MAX_CHARS` (200 chars),
whichever ends first — char-boundary safe and trimmed
(`stages::common::routing_window`). The gate (`min/max_command_chars`,
`max_input_tokens`) and the engine completion both run over the window, so a
long prompt can never bury the actionable opening. The window is recorded on
every Needle verdict (`StageMetadata::needle_window`) for auditability.

### 11.2 Direct tool responses via `output_template` (Milestone 2)

A `schema_overrides.<route>.output_template` declares a **deterministic
direct answer**: when Needle calls that tool and the invocation is complete
(every `{arg}` placeholder bound in the envelope's `arguments` object), the
router renders the template and answers directly — no dispatch, no classifier,
no extra inference. The renderer is the pure, dependency-free
`needle::template::render_output_template`: scalars render inline (strings
as-is, numbers/booleans via `Display`), objects/arrays as compact JSON, and a
**missing argument or a malformed placeholder returns `None`** so a template
never produces a half answer. A template only ever *enables* a direct answer;
on `None` (or a tool with no template) the request falls through to the normal
route/dispatch path unchanged. Tools that are genuine direct invocations
(e.g. `extract`) carry a template; shipped writer/coder/translator categories
deliberately do not.

### 11.3 General category → classifier fallback (Milestone 3)

`schema_overrides.<route>.general: true` marks a category Needle must **never**
decide on its own (e.g. the `local` general Q&A route). A Needle `call` to a
general route is treated as *not selected*: it emits `Skipped`
("needle declined (general category — classifier fallback)") and falls through
to the classifier LLM, which classifies the whole prompt as-is. Non-general
route tools keep the authoritative `Rerouted` short-circuit. Tools without an
override are non-general.

### 11.4 Audit parity (Milestone 4 + the deciding-stage record)

Every Needle outcome emits a `kind = "route"` record on the durable
`router.audit` stream with the same shape the classifier routing uses, plus the
decision window:

| Verdict | Fields |
|---|---|
| `rerouted` | `stage=needle`, `tool`, `confidence`, `window`, `reason`, `target_route`, `target_model`, `target_url`, `bypassed_classifier_gate=true`, `prefilter_verdict` |
| `direct_response` | `stage=needle`, `tool`, `confidence`, `window`, `reason` |
| `declined` | `stage=needle`, `tool` (when named), `confidence`, `window`, `reason` |
| `action` | `stage=needle`, `tool`, `confidence`, `window`, `reason` |

The `rerouted` record marks the trust boundary explicitly (DD-5): a Needle
reroute is accepted without a classifier coherence/safety pass, so the audit
record carries `bypassed_classifier_gate: true` plus the deterministic
pre-filter's verdict (`prefilter_verdict` = `passed` for a request that reached
Needle — a pre-filter rejection would have short-circuited the pipeline
first).

In addition the orchestrator emits **one aggregate per-request record** naming
the deciding stage (`needle` vs `classifier`) and the resolved target
(`stage`, `verdict`, `route`, `group`, `model`, `url`, `window`,
`confidence`, `reason`; `stage: none` when nothing decided). For a reroute it
also carries `bypassed_classifier_gate` and `prefilter_verdict`. The scorer
keys on this record.

### 11.5 Evaluation loop (Milestone 5)

`make router-benchmark` (`bin/coral-router-test.py`) attributes each scored
request to its deciding stage from the durable audit and reports:

* **Needle coverage** — share of non-general routes decided by Needle.
* **Needle routing accuracy** — among Needle-decided routes, the share that
  dispatched through the route's configured `model_group` (a direct template
  response counts as correct).
* **Needle direct-response rate** — share of Needle-decided routes answered
  directly from an `output_template`.

General routes are excluded from every metric (the classifier decides them by
design — never a Needle miss). A hermetic `--audit-only <dir>` mode parses a
fixture audit file and reports the same metrics without a live router
(fixture: `bin/fixtures/audit_routing_fixture.jsonl`).

### 11.6 Config recap (current: 5-tool functional catalogue)

```jsonc
"needle": {
  "enabled": true,
  "candidates_per_rung": 5,      // exactly the 5-tool catalogue (head stays off)
  "schema_overrides": {
    "explore": { "...": "...", "output_template": "Found: {value}" },
    "local":   { "...": "...", "general": true }
    // + code / summarize / explain / prose — 5 functional tools total
  },
  "tool_plans": {                 // live (see §12): explore/explain plans with
    "explore": { /* dispatch → knowledge_graph lookup → compose */ },
    "explain": { /* dispatch → chart lookup → compose */ }
  }
}
```

### 11.7 Measured optimization (2026-08-21) — the engine schema is plain

The closed-loop probe harness (`bin/needle-opt/probe.py`, corpus under
`bin/needle-opt/corpus_v1.jsonl`, run log under
`doc/router/needle-opt/runs.md`) drove a 2×2 over tool descriptions × schema
format against the real `libneedle.so`:

- **The engine schema must be the plain OpenAI tool format** (`name`,
  `description`, `parameters`). Rendering the custom `examples`/`intents` keys
  into `tools_json` regressed routing: the base model was trained on the plain
  format and treats the extra keys as noise (coverage 0.27 → 0.38 with the
  format fixed; accuracy 0.87 → 0.89, ECE 0.012, refusal recall held 0.86).
  `examples`/`intents` stay in `schema_overrides` as **retrieval context** for
  the HNSW shortlister (`schema_doc_text`), never engine grammar.
- Long/re-written route descriptions hurt the base model; the original
  descriptions (including the "Always dispatched to the capable model" route
  meta-text) are the best non-finetune fit.
- The base model's ceiling on the 9 abstract route-category tools is ~0.38
  coverage / ~0.89 accuracy with sound-enough calibration (6-8 confident wrong
  calls per 138 probes, zero correct calls below threshold). The remaining
  lever is a LoRA fine-tune on `query`/`tools`/`answers`/`reasoning` data
  (`bin/needle-opt/gen_finetune_data.py`), with the tuned-weights policy
  (`decline_on_missing_confidence: true`, since tuned weights report
  `confidence: None`).
- **Large catalogues (>12 tools)**: with `shortlist.mode: "hnsw"` and the real
   `embedding_model`, the shortlist reaches **Recall@5 = 1.00** over a
   synthetic 19-tool library (routes + command tools + DAG-workflow tools),
   so reachability is not lost on overflow; end-to-end routing on the
   shortlisted subset is bounded by the engine's own tool selection (~0.57 on
   the synthetic catalogue), and the pass-all → classifier fall-through
   degradation is unit-tested (`retriever.rs`).

## 12. Functional catalogue + confident-offload + bounded tool plans

The router's engine-facing catalogue is exactly five functionally-disjoint
tools — `code`, `summarize`, `explore`, `explain`, `prose` — plus `local`
(general Q&A, excluded from the engine grammar by construction so the
contrastive retrieval head stays off and every tool is always grammar-reachable).
`explore` absorbs search, navigation, API lookup, data-store lookup,
knowledge-graph lookup, and value pulls (direct-answer `output_template`);
`explain` absorbs translation, analysis/reasoning, NER, and DAG / entity-tool /
chart lookups. Subject routing is handled as bounded, config-declared subagent
plans (`needle.tool_plans` in `env/coral-router.json`, typed as
`ToolPlan`/`ToolPlanStep` in `config.rs`) executed by `run_tool_plan` in
`server/handler.rs` — each step is dispatched via the standard `ChatBackend`
chain or a read-only lookup through an installed `ToolLookup` resolver,
recorded to the ledger, and audited (`tool_plan`, `tool_plan_step`,
`tool_plan_composed`, `tool_plan_fallback`); exceeding `max_rounds` falls back
to plain group dispatch.

**Tool-plan `Lookup` steps are real and bounded.** The shipped config carries
`tool_plans` for `explore` (dispatch → knowledge-graph lookup → compose) and
`explain` (dispatch → chart lookup → compose). Each `Lookup` step is resolved
by a read-only `ToolLookup` resolver (`server/tool_lookup/`) installed at boot
from the stores the deployment configures: `dag` (per-request session step
graph), `knowledge_graph` (embedding KNN over the shared `ContentNodeStore`),
`chart` (the shared chart store), `entity_tool` (session DAG + ledger
associations), and `data_store` (a capability-gated `fluent-db` read, wired
only when `tool_lookups.data_store_path` is set). A plan whose `Lookup` step
names a kind with no installed resolver is declined to plain group dispatch —
never executed with a placeholder; a lookup that resolves to nothing (absent)
is omitted from the composition, never synthesized (`search`/`api` kinds have
no real client yet, so plans needing them decline). The `confidence` gate
(`confidence_threshold`, re-derived empirically on the tuned weights) is
live: the eval harness reads raw engine confidence via ctypes `libneedle.so`
(mirroring the Rust FFI), and `compute_metrics` reports precision-coverage at
0.5/0.6/0.7/0.8 plus ECE.

### 12.1 Confident offload: one `confidence` vocabulary across both rungs

Needle and the classifier speak one shared "confident-offload" concept: each
rung emits a **calibrated self-assessment of its decision** in the same
0.0–1.0 `confidence` envelope. Needle's `confidence` (its engine wire format,
read verbatim — never re-derived or nulled) is the same semantic as the
classifier's `confidence` (the model's self-assessed confidence in its
`domain`), differing only in the decision each grades: Needle grades a tool
call, the classifier grades its `domain`. A low-confidence Needle outcome
(`Skipped`) falls through to the classifier; the classifier then makes its own
confidence-gated decision. The routing layer never mixes the two — the Needle
`confidence_threshold` gates reroutes, while the classifier's confidence feeds
`derive_action`'s `classifier_respond_threshold`. Keeping one vocabulary makes
the "confident-offload" posture a single concept across both rungs.
