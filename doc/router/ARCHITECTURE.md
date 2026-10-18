# Coral Router — Architecture

*Current implementation of Coral Router and which pieces are load-bearing.
Aspirational goals and the finished design live in [`VISION.md`](./VISION.md).*

## Source code location

`./src/router/src/` (crate `fluent-router`); binary entry point in
`./src/bin/coral-router/`.

## Overview

Coral Router exposes an OpenAI-compatible HTTP endpoint (`POST /v1/chat/completions`
on `:8079`) and runs every request through an **ordered stage pipeline**
(deterministic pre-filter → optional NLP/overlay → classifier) before dispatching
to a model. Stages are `Arc<dyn Component>` units (the Fluent WVR uniform
interface) and the server itself is a `WorkUnit` — everything composes. The
design principles: deterministic before probabilistic, cheap before expensive,
condensed context via a ledger, and frontier as a bounded, audited exception.

**Local serving is owned, not proxied.** Coral Router spawns and supervises one
`llama-server` process per model weights file (`supervisor/`), serves the
`/instances` management contract at its own address (`server/instances_api.rs`),
and is the single routing element between those servers and every other
OpenAI-compatible endpoint. The llama.cpp router mode is never used; a local
dispatch is a direct HTTP call to the owning server.

```
POST /v1/chat/completions {model, messages, ...}  (+ query routing fields)
        │
        ▼  RouterServer (hyper on tokio; server.rs + server/handler.rs)
   merge query fields into body (body wins) → RouterRequest (normalize.rs)
   record request in ContentNodeLedger (LOD0)
   PipelineOrchestrator::execute() → PipelineResult
        │
        ├─ classifier_response  → respond directly
        └─ routing_target       → server/dispatch.rs (ChatBackend chain)
   management: /instances /v1/models /models /memory /props + model-less proxies;
   /health /stats /v1/plan /v1/rigor /metrics; POST /admin/cache/invalidate;
   admin /models/unload

PipelineOrchestrator: Vec<Arc<dyn Component>>, sequential; known stages call
StageDecisionProducer::evaluate (typed handoff, STAGE_DECISION_KEY); arbitrary
components via WorkOutput.data; short-circuits on Rejected/Error.
  1. DeterministicPreFilter — deterministic filter engine (no model call)
  2. NlpStage (opt-in)      — spacy-rs tokenize/parse/interlingua → RoutingSignals
  3. OverlayStage (opt-in)  — onnx overlays score parser residuals → route hints
  4. ClassifierStage        — single LLM call (or Classification Tree) → direct
                              response / routing target / rejection

Dispatch ─┬─ Local dispatch (ChatBackend chain)
          ├─ Escalation ladder (dispatch/escalation/)
          └─ plan / rigor routes (routes/plan.rs, routes/rigor/)

Serving layer (supervisor/ + instances/): one llama-server per weights file on a
free localhost port; direct /instances + generation calls, no llama.cpp router;
sidecar footprint-weighted eviction (weights included) over the VRAM budget;
pre-admission (weights + incoming KV window) before cold loads; resume-marked
contexts KV-snapshotted before they drop (auto mode; manual destroys without saving).
```

## Pipeline: ordered stage list

The pipeline is an **ordered list** of `Arc<dyn Component>` stages built from
config (`config/builder.rs::build_named_pipeline_*`). The two decision stages —
deterministic pre-filter and classifier — are the load-bearing core; NLP and
overlay are opt-in **enrichment** between them (they publish parse/route context,
never a verdict). The removed `PipelineStage::Router` variant no longer exists
(historical `"Router"` labels deserialize to `PipelineStage::Classifier` with a
`warn!`). Stage failures surface as `PipelineError::Stage`, naming the failing
`PipelineStage` and carrying its `WorkError`.

| Stage | File | Model call? | Produces |
|-------|------|-------------|----------|
| 1. DeterministicPreFilter | `stages/deterministic.rs` | No | Command result, PII flag, or pass-through |
| 2. NlpStage *(opt-in)* | `stages/nlp.rs` | Deterministic base `[ArcEager, Rule]` always; gated refine `[Encoder, Llm]` on confidence OR task-value via `RefinePolicy` (fallback keeps base) | `RoutingSignal`s under `"nlp_parse"`, the confidence summary, and the typed `NlpResidual` handover |
| 3. OverlayStage *(opt-in)* | `stages/overlay.rs` | Yes — onnx overlay models | Route hints the classifier merges as deterministic context |
| 4. ClassifierStage | `stages/classifier.rs` | Yes | Direct response, rejection, or routing target |

NLP/overlay are gated on config (`pipelines.<name>.nlp`, non-empty
`overlay_models` + `nlp_ordering: "deterministic_first"`), so the default pipeline
— filter → classifier — is byte-identical to the historical two-stage shape until
a deployment opts in. `deterministic_first` without `overlay_models` warns and
falls back to `llm_first`. See §"Natural-language processing spine".

The classifier has two modes. **Flat** performs one LLM call returning structured
JSON (direct response, rejection, or `RoutingTarget`). **Tree** wraps the
`ClassificationEngine` (`stages/tree/`): with a configured tree
(`config/classification.rs`) the engine walks filter / classifier / terminal /
fallback children, emitting a per-node `StageDecision` into `metadata.tree_path`
and a `kind = "tree_node"` audit record per visited node. Route-name guessing is
gone; route selection is the tree's job.

## Design Contract

Every stage implements `Component` (the Fluent WVR supertrait); the orchestrator
never branches on concrete type:

```rust
impl WorkUnit for DeterministicPreFilter { … }
impl FieldAccess for DeterministicPreFilter { … }
impl Describable for DeterministicPreFilter { … }
impl_component!(DeterministicPreFilter);
```

**Typed handoff.** The two known stages implement `StageDecisionProducer`
(`pipeline_types.rs`); the orchestrator downcasts via `component_downcast_ref` and
calls `evaluate(ctx, prior)` directly, publishing the decision to the in-process
typed store under `STAGE_DECISION_KEY` (`pipeline.rs`). Arbitrary components flow
through the `WorkOutput` channel (the genuine serialization boundary); their
serialized decision is deserialized once and published to the same store.

## Source Layout (`src/router/src/`)

### Core types

| File | Role |
|------|------|
| `types.rs` | OpenAI protocol (`RouterRequest`/`RouterResponse`/`RouterMessage`/`RouterChoice`/`Usage`). `RouterRequest` also carries the routing fields the owning llama-server reads: `instance`, `snapshot`, `id_slot`, plus the LoRA `adapter` |
| `pipeline_types.rs` | `StageDecision`, `PipelineStage`, `StageVerdict`, `StageDecisionProducer`, `StageMetadata` (typed handoff keys incl. `nlp_parse`), `NlpResidual` (typed residual under `nlp_residual`), `PiiVerdict` |
| `pipeline.rs` | `PipelineOrchestrator`, `PipelineResult`, `RoutingTarget` (url/model/group/params/filter_thinking/retry/stream/timeouts/fallbacks + `instance`/`snapshot`/`id_slot`), `STAGE_DECISION_KEY` |
| `error.rs` | `ServerError` — the single typed server error (Bind / Http / Addr / transparent `DispatchError`) |
| `config.rs` | `RouterConfig` + sub-config re-exported as submodules: `addr`, `builder` (`PipelineParams`, `NlpConfig`, `NlpOrdering`, `TargetMatchMode`), `classification` (`ClassificationTree`/`Node`/`Child`), `compiled_tree`, `escalation` (`EscalationLadderConfig`, `FrontierConfig`, `RoleMember`), `filters` (`RejectPatterns`/`PatternEntry`/`FilterAction`/`FilterScope`/`ConfidenceGate`), `fleet` (`RoleParams`), `ledger_group`, `models` (`ModelEntry`, `ModelBinding`, `InstanceProfile`), `normalize`, `refine_policy` (`RouterRefinePolicy`), `rounds`, `routing` (`RoutingConfig`, `RouteRef` with `always_route`), `runtime`, `serving`, `tree_reload`; top-level `ledger`/`session`/`review`/`overlay`. `ModelEntry` adds managed-weights source (`weights`/`hf_repo`/`hf_file`), the `snapshot` save-mode block (`SnapshotConfig`/`SnapshotMode`), `is_managed()`. In-process ONNX models are NOT `ModelEntry`s — the top-level `onnx` role section owns them (`RouterConfig::onnx`). `RoleEntry` binds serving models via `roles.<role>.models` (empty lists ride the `last` sentinel) + optional `snapshot`/`escalation`. Top-level `gguf_dir`/`onnx_dir` are the admin-CLI root + prefix for relative `models.<model>.weights`/`template` and ONNX paths; both resolve through `join_config_dir`/`resolve_weights`/`resolve_template`/`resolve_onnx_path`; `sidecar.resume_ttl_s` bounds resume snapshot lifetime |
| `normalize.rs` | Thin adapter over `fluent_llm::openai`: OpenAI JSON ↔ `RouterRequest`/`RouterResponse`, `error_response()`, `messages_to_json()`, `parse_openai_stream_delta`. Re-attaches the routing fields the shared normalizer strips |
| `routing_context.rs` | `RoutingContext` — routing fields (`model`/`instance`/`snapshot`/`id_slot`/…) read from query string and JSON body (body wins via `merge`), carried as params through normalize and dispatch |
| `target_match.rs` | `TargetMatcher` — the in-role complexity ladder (`build_self_assessment_prompt`, `parse_self_assessment`, `candidates_for_role`, `start_index`/`is_match`); DRY-shared by the flat classifier path and the classification-tree engine. `RoleRecency` — bounded (4096) `(session, role) → model` recency map with interned keys and monotonic sequence for total-ordered eviction |

### Pipeline & Stages

| File | Role |
|------|------|
| `stages/deterministic.rs` | `DeterministicPreFilter` — delegates to `DeterministicFilterEngine`; slash-command dispatch (`/help`, `/stats`, `/checkpoint`) |
| `stages/nlp.rs` | `NlpStage` — runs `spacy-rs` on inbound text and publishes per-sentence `RoutingSignal`s under `"nlp_parse"`, plus the confidence summary and typed `NlpResidual`. Enrichment, never a gate. Deterministic-first ladder; `chat_backend_to_fetch_sync` bridges a `ChatBackend` to `spacy_rs::LlmFetchSync` |
| `stages/overlay.rs` | `OverlayStage` — under `deterministic_first`, scores parser residuals with configured onnx overlays (`PromptRouterOverlay` via `ort::disambiguation_overlays`) and publishes route hints the classifier merges. Enrichment, never a gate |
| `stages/classifier.rs` + `stages/classifier/` | `ClassifierStage` — single LLM call (flat) or `ClassificationEngine` (tree); emits direct response / routing target / rejection; builds `RoutingTarget`; enforces route-level `always_route` (override `action=respond` → `route`) and auto-generates the system prompt's "Dispatch rules" section. `classifier/action.rs` (`ClassifierAction`); `classifier/prompt.rs` (route list + threshold substitutions) |
| `stages/tree/` | `ClassificationEngine` — recursive nested-tree evaluation; filter / classifier / terminal / fallback nodes; `tree_path` audit trail; `kind = "tree_node"` records. Split `mod.rs` (re-exports) / `engine.rs` (recursive walk + `cost` + the `match_interlingua` filter dispatching on parsed interlingua ids) / `verdict.rs` (`TreeClassifierVerdict` + `parse_tree_verdict`) / `decisions.rs` (`TreeOutcome`/`TreeEvaluation` + `StageDecision` builders); classifier-node prompts live in `config/classification.rs` |
| `stages/common.rs` | Shared helpers — `extract_user_message()`, `get_metadata_string()`, JSON-field ensure helpers |
| `stages/prompt_parse.rs` | `chat_json` + `PromptParseError` — router-local LLM-JSON round-trip codec over `fluent_llm::parse::parse_typed` |
| `stages/retry_classifier.rs` | `RetryClassifier` — retry-with-backoff decorator over the classifier (opt-in via `classifier_retry_max`) |
| `stages/pipeline_ref.rs` | `PipelineRefStage` — re-usable pipeline stage from named config |

### Filters (MOA_ROUTER_SPEC §2)

| File | Role |
|------|------|
| `filters/mod.rs` | `Filter` trait (`kind() → FilterKind` + `evaluate(ctx) → Option<FilterDecision>`); `FilterKind::{Regex, Whitelist, HnswSimilarity, ModelClassification}`; `FilterDecision::{HardReject, SoftRedirect, OutputFilter}`; `FilterContext` scopes (`Any`, `FrontierBound`, `ContentNodeWrite`); `DeterministicFilterEngine` — ordered chain-of-responsibility, first non-`None` wins |
| `filters/regex_filter.rs` | `RegexFilter` — compiles regexes from `PatternEntry` config; respects `FilterScope`, `ConfidenceGate` (LuhnValid), `FilterAction` (Redact/Anonymize/Omit) |
| `filters/injection_detect.rs` | `InjectionDetectFilter` — heuristic prompt-injection / system-prompt-exfiltration detection |
| `filters/luhn.rs` | Luhn validation — secondary gate for credit-card-number patterns |

### Dispatch

| File | Role |
|------|------|
| `dispatch/backend.rs` | `DispatchBackend` trait (`complete` / `stream_complete[_with_abort]` — object-safe, per-request params as args); `OpenAiChatBackend` (single-attempt HTTP via raw `reqwest`), `RetryBackend` (jittered-exponential via `common_core::retry`), `BackendChain` (ordered chain) — the single production dispatch path (D4). LLM client trait remains `fluent_llm::client::ChatBackend` |
| `dispatch/escalation/` | `Ladder` — local-chain exhaustion → `filter → question → team → turnover` modes, deterministic-first `ContextCache` short-circuit, `ResultPool`-backed parallel slots, `kind = "escalation"` audit records; `EscalationBackends`/`LocalBackend`/`FrontierBackend` role wiring; `dispatch_frontier` bypass for frontier-owned sessions. Split `mod.rs` (`Ladder` + `try_escalate`) / `modes.rs` (four modes + the single shared `frontier_complete` transport) / `assemble.rs` (parse/scorer) / `audit.rs` (`emit_audit`) |
| `dispatch/frontier.rs` | `DispatchError` + `is_retryable` (the public error type of `ChatBackend`); wire-format build/parse helpers reserved for the ladder — `OpenAiBackend` (its `parse_response` reused by `OpenAiChatBackend`), `Anthropic` Messages-API helpers, `StreamEvent` |

### Session & Orchestration

| File | Role |
|------|------|
| `session.rs` | Thin shim — re-exports `StepStatus` from `fluent-types` (canonical session node schema is `fluent_types::ContentNode`) |
| `dag_session.rs` | `DependencySession` — DAG session over `fluent_dag::dep_graph::DependencyGraph<String>` (step tracking, checkpoint/rewind, real KV snapshot restore keyed model/adapter/session, frontier-ownership flag); `SessionRegistry` — canonical server-side session home (D6), per-`session_id`, shared `SnapshotStore`, retained for process lifetime |
| `ledger.rs` | `ContentNodeLedger` — **thin facade** over the shared `ContentNodeStore`; owns LOD lifecycle (LOD0/LOD5 eager, LOD1–4 lazy from LOD0 via `Summarizer`, at most once); `CompactionStrategy`/`RecencyCompaction`; routes all writes through the write-path scrub; `write_annotation` is the tiered-annotation facade |
| `ledger/nlp.rs` | Ledger writer for the NLP parse — `record_parse_node_with_confidence` writes the parse `ContentNode` **and** the per-sentence × role `interlingua_index` rows in one transaction, so "requests whose `direct_object_id` is X" is a real SQL query |
| `ledger/overlay.rs` | `OverlayCandidateStore` over `overlay_candidates` — the durable candidate plane for async overlay outputs. The **only** surface overlays write (boot-only registration is an invariant) |
| `ledger/overlay_worker.rs` | `OverlayWorker` — background node-annotation worker composing the shared `CreditedFeedWorker<NodeId>` (the **same** feed shape as `LedgerTierWorker`, deliberately not reimplemented): per node it fans out three independent derivations (`annotation_for`/`embedding_for`/`llm_overlay_for`, each from LOD0, at-most-once, fail-open) **in parallel** on the injected `Runtime`, so a slow LLM never delays the spacy/embedding overlays. Emits a `kind = "overlay"` audit record per derived overlay; boot backfill via `node_ids_needing_overlays()`. Acceptance bar lives in the private `ledger/overlay_acceptance.rs` suite |
| `ledger/entity_promotion.rs` | `EntityPromotion` — flag-gated seam accepting worker-plane entity-link candidates through the calibrated stamp gate into parse-time `interlingua_entity_id` stamps (refused stay pending; human review owns dismissal) |
| `ledger/workflow_replay.rs` + `ledger/workflow_store.rs` | Solved question-mode runs distilled into an embedding-keyed library (`WorkflowStore`/`InMemoryWorkflowStore`, gated inserts) and replayed when a new query is close enough (`WorkflowReplay`/`WorkflowRecorder`, calibrated gate, reuse audited) — the dispatch learning loop's memory |
| `ledger/correction_index.rs` | `SqliteCorrectionIndex` — the router's `spacy_rs::CorrectionIndex` over `interlingua_index`; pattern-cache rows use `PATTERN_NODE` + `role = "correction"` so they never collide with parse-node audit rows |
| `ledger/span_cache.rs` | `SpanCache` trait + `InMemorySpanCache` (hermetic double, M2b, moved from `spacy-rs::cache`) + `SqliteSpanCache` read-through view over `interlingua_index` (`role = "span_cache"`, hex `span_key TEXT`); `span_cache_seam` adapts `Arc<dyn SpanCache>` into the dependency-free `spacy_rs::cache::SpanCacheSeam`, so no router edge enters spacy-rs |
| `ledger/annotations.rs` | `AnnotationStore` — tiered annotation ledger over `nr_annotations`, keyed `(content_hash, claim_key)`; a higher `ProvenanceTier` upsert supersedes (never deletes) the prior claim, equal/lower marks `Provisional` |
| `ledger/frame_index.rs` | `PreferredSenseIndex` trait (ledger-owned, M2a, moved from `spacy-rs::frame`) + `SqlitePreferredSenseIndex` over the `interlingua_index` pattern-cache rows (`role = "sense"`) + the wave-batched `FrameResolutionWorker` (one grammar-constrained call per `SupervisedBatch` wave returning N resolutions) |
| `ledger/node_annotation.rs` | `NodeAnnotation` — the fully-materialized immutable annotation document (moved from `spacy-rs::arcready::ArcReadyAnnotation`, renamed), with its `fluent_types::NodeOverlay` impl; built by `node_annotation(doc, result)` |
| `node_store.rs` | `ContentNodeStore` — nodes behind `Arc<RwLock<ContentNode>>`, interned `ArcIntern<str>` session/role index keys, durable `content_json` hydration (seeded `next_id`), `ensure_tier`/`lod_text`/`session_node_ids`, `knn_brute_force`, and `content_hash` stamping in the single write funnel. Overlays: `annotation_for`/`embedding_for`/`llm_overlay_for` derive lazily and **at-most-once** from LOD0 (shared `derive_overlay` re-checks under the write lock, marks `ready`/`failed`); fail-open seams injected by `set_overlay_pipeline`/`set_overlay_embedder`/`set_overlay_llm`; the `overlay_events` feed (`enqueue_if_needs_overlay`, `node_ids_needing_overlays`) |
| `ledger_guard.rs` | `scrub_for_ledger` — the irreversible write-path scrubber, decision D1; Redact/Anonymize collapse to `[REDACTED:<pattern>]`, no codeword map retained; uses the builtin filter engine with `ContentNodeWrite` scope |
| `views.rs` | `LedgerView` — reference-only view layer over `ContentNodeStore`; `Lod` (0..=5), `ParallelLedger` (one store, N views), `FilteredLedger<V>` (exclusion set + render transform); `render()` is the single text-exit; degrades to LOD0 when a lazy tier is un-derivable |
| `knowledge.rs` | `KnowledgeCapability` impl on `ContentNodeStore` behind the `RouterKnowledgeCapability` token — the cross-crate read path for embedded consumers |

### Routes

| File | Role |
|------|------|
| `routes/plan.rs` | `PlanRoute` — boot-loaded `ChartStore` + `ChartSelector`; Exact → server-side chart compile+execute under `SupervisedBatch`; Partial → one-round targeted interview (≤ `CHART_MAX_INTERVIEW_QUESTIONS`); Mismatch → fresh draft; `workflow_extractor` hook for the learning loop |
| `routes/rigor/` | `RigorRoute` — fixed-pass blue/red/judge; real `DependencySession` checkpoint (`rigor.blue`) + `rewind_to_checkpoint` on material rejection; red team reads through `FilteredLedger` at `LOD0` (dead ends excluded); final rejection resolves to a targeted interview (≤ 3 questions), frontier escalation only on low judge confidence; `/v1/rigor` is present-but-unconfigured without backends (explicit error, never a crash). Prompt constants/builders/tolerant parses live in the `prompts` submodule |
| `charts/` | Chart (DAG workflow) library — `store` (`ChartStore`), `binding` (`Entity`, `ENTITIES_META_KEY`), `compile`, `execute` (under `Limiter` + SupervisedBatch), `render`, `rubric`, `select` (`ChartSelector`, `ChartFit`), `extract` (`WorkflowExtractor`), `stage` (`ChartPromptStage`) — the engine consumed by `PlanRoute` and the learning loop |

### Infrastructure

| File | Role |
|------|------|
| `server.rs` | `RouterServer` (`WorkUnit`) — hyper HTTP/1.1 accept loop on tokio; assembles `ServerDeps` and fans out to `server/`; `serve_http` is `pub(crate)` for integration tests; runs each `InstanceManager`'s boot reconcile + residency task from the attached `InstancePool` (in a drained `JoinSet`, never detached) |
| `server/handler.rs` | HTTP routing + request orchestration; `ServerDeps` (pipelines, routes, models, stats, `max_payload`, classifier, cache, ledger, plan/rigor, sessions, ladders, context_cache, mock_dispatch, http_client, `instance_pool`, `api_key_env_name`, `supervisor`, `onnx`(+`onnx_llm_backend`), `fleet`, `coordinator`+`coordinator_gate`, `review_worker`+`review_fetch`, `entity_link_worker`, `roles`/`groups`, `workflow_recorder`, `frame_gate`). Merges query routing fields into the body (`RoutingContext`: body wins), resolves the `<model_id>[:<instance\|group\|latest>]` grammar in `resolve_pipeline`, routes management/model-less endpoints |
| `server/cors.rs` | Permissive CORS header helpers (`CORS_ALLOW_*`, `add_cors_headers`) |
| `server/instances_api.rs` | Public `/instances` management facade: aggregate envelope across models, `POST /instances`, per-instance ops (delete/pin/unpin/resume/no-resume/resize/snapshot), `/memory` compat reshape, `/v1/models`, `/props`, model-less proxies (`/tokenize`/`/detokenize`/`/apply-template`/`/control`); management API-key enforcement; query parse/percent-decode helpers |
| `server/dispatch.rs` | `handle_dispatch`/`dispatch_real` — primary + `fallbacks` chain through `ChatBackend` (each wrapped in `RetryBackend`), short-circuit on non-retryable errors, response cache read/write, workflow extraction, allocate-on-503 via `InstancePool` |
| `server/responses.rs` | OpenAI-completion response builders, SSE/CORS headers, `ServerStats` counters |
| `server/admin.rs` | Admin endpoints for the CLI — `POST /models/unload` (stop a managed model's `llama-server`; spec stays registered) and `GET /metrics` (aggregates managed llama-servers' Prometheus expositions, `?model=`-filterable, deduplicated `# HELP`/`# TYPE`) |
| `server/review.rs` | `ReviewWorker` — async parse-review boundary: a credit-gated `WorkerPool<ReviewJob>` via the shared `CreditGatedPool` (the ledger-tier-worker pattern) running `spacy_rs::review_prompt` (taxonomy-grounded) + `apply_corrections` against `SqliteCorrectionIndex`, writing back with `HumanReview` provenance. Served at `POST`/`GET /v1/sessions/{id}/review-parse`; the onnx LLM serves it with the `ParseReview` grammar; an optional PII pre-filter flags candidates when `review.auto_enqueue` is on |
| `server/entity_link.rs` | `EntityLinkWorker` — async entity-link overlay: a credit-gated `WorkerPool<EntityLinkJob>` scoring unresolved PROPN spans against the boot-baked ColBERT `EntitySimilarityIndex` (via the injected `EntityLinkScorer` seam) and writing **candidates only** to `overlay_candidates` — never a parse-time `interlingua_entity_id` write |
| `streaming.rs` | `StreamingHandler` — SSE delta formatting for OpenAI-compatible chunks; cross-chunk think-block filtering via `StreamingThinkFilter` |
| `kv_cache.rs` | Two-tier snapshot **metadata index** (the router never reads/writes raw KV bytes — the fork owns them): `HotSnapshotIndex` (RAM LRU over `common_core::cache::LoadCache`) + `ColdSnapshotIndex` (in-memory `(model, adapter, session)` map with TTL sweep; derives `file_path` to the fork's slot-save layout). `SnapshotIdentity` (weights fingerprint + quant + fork version); `KvSnapshot::check_compatible` rejects restores on known-vs-known drift (unknown fields never reject); `save_snapshot_for_model` requires an adapter when a LoRA is attached, so base and base+LoRA never share a key |
| `instances/` | Instance-pool grammar generation + validation (`instance_grammar_string`, `validate_instances`, `is_valid_instance_name`) and the sidecar. Split `mod.rs` (grammar/validation + test stub), `client.rs` (`InstanceClient` — one server's `/instances` over raw `reqwest`, `HttpClass`-classified; `InstanceError`/`InstanceInfo`/`InstanceTotals`/`SnapshotInfo`), `manager.rs` (`InstanceManager` — boot reconcile; per-instance `resume` map; `is_sleeping` residency probe; `list_with_fallback` synthesizing a footprint for plain models; `weights_bytes`+`load_bytes_for`; `snapshot_mode`; `ensure_instance`; `ensure_group` allocate-on-503), `pool.rs` (`InstancePool` — aggregate facade: `<model_id>:<name>` ids, 64-bit-summed `total`, eviction `Evictable::{Context, Model}`+`evict_to_fit`, admission `make_room_for` against the allocation budget, `resume` ops, `run_residency`). Public surface in `instances/api.rs` |
| `supervisor.rs` + `supervisor/` | `LlamaServerSupervisor` + `ManagedServer` (`LlamaServerSpec`, `build_server_args`, `free_port`, `resolve_llama_server`) — resolves `llama-server` from `$PATH`/`LLAMA_SERVER`, spawns one process per managed model on a free localhost port, waits for `/health`, supervises each child (logs output, restarts with capped backoff, post-boot liveness probes `/health` and kills+restarts a hung server past `liveness_failures_before_restart`). Boots only pinned-instance models (`start_all`); lazy models load via `ensure_running` (spawn-locked) and unload via `unload`. `supervisor/adopt.rs` reaps pre-existing servers at boot; `supervisor/health.rs` owns the probes. **Onnx-declared models are excluded** (served by `ort.rs`) |
| `ort.rs` | Onnx composition root (crate `fluent-onnx`). `build_onnx_registry(config)` turns the top-level `onnx` section into a boot-built `OrtSessionRegistry` (`resident: true` loads at boot, else lazily; fail-open to empty/plain-HTTP when the feature is off). `onnx_chat_backend(key)` wraps a `CausalLm` session as the generative `ChatBackend` (`OnnxRole::Llm`) — the single wiring point for the LLM rung, review, enrichment, tiering, summarization, ranking, retrieval; `grammar_from_json_schema` constrains structured decodes. Onnx CPU-RAM residency rides the shared `LlmResidencyEngine`. Encoder-role builders (`disambiguation_overlays`, `nlp_encoder_fetch`, `pii_prefilter`, `colbert_entity_scorer`, `onnx_colbert_reranker`) stay feature-gated legacy seams; no encoder-role model is configured in the reference deployment |
| `concept_store_sqlite.rs` | `SqliteConceptStore` — durable `fluent_concept::ConceptStore` over the shared ledger SQLite connection (`interlingua_concepts`, keyed `(namespace, canonical_name)` with content-addressed `id` + coral `node_id`). The **second home** of the one-loader/two-homes YaGO design; boot reconciliation locks it equal to coral's graph |
| `ledger/orchestrator.rs` (affinity) | Composes `AffinityScheduler`/`ScheduledTask` from `fluent_concurrency::affinity` directly (no router shim) — opt-in via `ledger.orchestrator.affinity_cap` |
| `summarization.rs` | `ResultScorer` + `Summarizer` — `WorkUnit`s calling an LLM (`Arc<dyn ChatBackend>`) to score/condense responses; feeds the ledger's lazy LOD tiers |
| `score_matrix.rs` | `ScoreMatrix` — multi-dimensional weighted scoring (coherence/complexity/completeness/risk) with per-route dimension bands |
| `metrics.rs` | `FailureClass` + `classify_error` — typed-first error classification with a string-regex fallback for opaque output (D10) |
| `audit.rs` | Canonical durable-audit surface — single `tracing` target `router.audit`; `AuditRecord` + `emit(kind, detail)`; kinds distinguished by the `kind` field (`route`, `filter`, `tree_node`, `escalation`, `rigor`, `chart_target`, …) |
| `logging.rs` | Two-stream `tracing` subscriber: operational JSON/console rolling file + the durable audit stream (separate retention, always JSON, gated on `router.audit=info`) |
| `frontier/modes.rs` | `EscalationMode` ladder taxonomy — `Filter`, `Question`, `Team`, `Turnover` (D8; the old `FrontierMode` is gone), serde snake_case; `FrontierResult` + `AuditEntry`. Taxonomy/audit types only — the runtime lives in `dispatch/escalation/` |
| `overlay.rs` + `overlay/canonical.rs` | Overlay plane's pure, model-free data: `CanonicalFormTable` — a versioned, offline-clustered `(surface lemma → canonical lemma)` table for predicate canonicalization, loaded like the lemmatizer blob and consulted by plain lookup (never an inference call), keeping "ids are pure functions of content" |
| `instances/traits.rs` | Unified fleet adapters — `LlamaWeights`/`LlamaContext`/`LlamaKVCache` implement the shared `fluent-llm::runtime` traits (`LlmWeights`/`LlmContext`/`LlmKVCache`), composed by `LlmFleet` (llama + onnx implementors) so `/instances`, `/v1/models`, and unload drive both fleets uniformly. `LlamaContext::evict` snapshots resume contexts only in `auto` mode (`manual` destroys without saving and audits `evict_skip_snapshot`); `LlamaKVCache` carries adapter + expected identity and rejects on drift |
| `telemetry.rs` | Structured telemetry with a controlled vocabulary (`ToolName`/`ProviderCategory`/`FeatureName`), the `TelemetryEvent` enum, and `TelemetrySink` (`TracingSink`/`NoopSink`) — no free strings, no PII |
| `transforms/` | `TransformStrategy` trait + `rewrite_text_messages`/`insert_string_map` helpers: `NoTransform`, `PiiAnonymize`, `DecomposeToAnonymizedHypothetical`, `DecomposeToSubtasks` (bespoke `subtasks` object), `CodewordAnonymizer`, `Sanitize`, `SecretMask` |
| `ranking.rs` | Ranking pipeline — `SalienceSignal`/`SalienceScorer` (weighted, pure), `pagerank` (minimal power-iteration over the `interlingua_index` co-reference + `parent_id` graph), `SalienceSource`/`LedgerSalienceProvider` (read-only snapshot), `rank_candidates` (salience prefilter to top-K then **one** grammar-constrained call over the shortlist; degrades to salience order), `SalienceRanker` (the composable step the retrieval service composes) |
| `retrieval.rs` | Live dispatch seam + subagent tool owner: `NodeRetrievalService` parses a candidate node's LOD0, runs spacy-rs' pure `lemma_grep` + the router-owned fuzzy axis (`EmbeddingProvider`/`FuzzyRetrieval`/`InMemoryFuzzyIndex`) + the `cross_check` combiner, and pre-filters the pool through `SalienceRanker`; exposed on `LedgerAgentCoordinator` via `with_retrieval`/`retrieve_nodes`. Reports carry per-sentence `predicate_ids`; `filter_by_interlingua` drops candidates whose predicates are neither equal to nor a subclass of the query predicate (fail-open; never consults parse confidence; default-off) |
| `cli/` | Admin CLI support backing `coral-router` `list`/`ps`/`pull`/`scan`/`rm`/`show`/`stop`/`speedtest` (ported from `gguf_tool.py`): `gguf.rs` (GGUF scanning + `models.json` cache), `preset.rs` (`models-preset.ini`), `commands/` (`filesystem.rs`, `server.rs`), `CliContext`/`CliError` |
| `testing/` | `TranscriptProvider`, `MockTranscriptEntry`, `MockDispatchContext` — transcript-driven integration-test harness for E2E/golden tests |
| `test_stubs.rs` | `StubChatBackend`, `HashEmbedder` — test-only backends (cfg(test)) |
| `test_support.rs` | Test-only log capture (`capture_logs`) — installs a global subscriber and rebuilds the callsite-interest cache so parallel tests assert logs deterministically |

### Adapter architecture (`dispatch/`)

The **production dispatch path** runs through `dispatch/backend.rs`, which defines
the object-safe `DispatchBackend` trait every server dispatch site depends on (the
single dispatch trait, D4); the LLM client trait remains
`fluent_llm::client::ChatBackend`. `DispatchBackend` exposes `complete`,
`stream_complete`, and `stream_complete_with_abort` (per-request params as args,
`abort: Option<StreamAbort>`). Concrete backends: `OpenAiChatBackend`
(single-attempt HTTP through a raw `reqwest::Client`; non-2xx classified via
`HttpClass` into `DispatchError::RateLimited` vs `Http`), `RetryBackend`
(jittered-exponential via `common_core::retry::retry_async` — the single backoff
helper), `BackendChain` (ordered chain, short-circuiting on terminal 4xx).
`dispatch_real` iterates the primary `RoutingTarget` plus its `fallbacks` (target
candidates from `RoutingConfig::all_dispatch_targets`, ordered by intelligence
proximity — not classifier backups). Streaming flows through `StreamingHandler`
over an `http_body_util` channel, wrapped in a `StreamBody` whose `Drop` fires a
`StreamAbort`. `RetryBackend`/`BackendChain` **forward** the abort token unchanged,
so a downstream abort reaches the transport through any retry/fallback layer.
Request bodies use the canonical `fluent_llm::openai::build_openai_chat_body`
(carrying the `chat_template_kwargs: {"enable_thinking": false}` default).

**Abort propagation (streaming).** When the client stops consuming, hyper drops
the `StreamBody` and the `StreamAbort` fires; the forwarding task `select!`s its
upstream read *and* `send_data` against the signal, so a downstream disconnect
(a) drops the upstream `reqwest::Response` — the llama-server fork reads the
closed connection as a slot interrupt — and (b) finalizes the `StreamAnswer` with
whatever streamed, so the ledger records the partial answer. Because Coral Router
owns the fleet, `dispatch_to_single_target` also arms a watcher that issues an
explicit `POST /abort` (`InstanceClient::abort`) — belt-and-suspenders, best-effort.

`dispatch/escalation/` owns the ladder runtime: after every local model in a
`model_group` chain fails, `try_escalate` consults the deterministic
`ContextCache` first (short-circuit before any frontier call), then runs the
configured modes in order. Each mode's frontier transport reuses
`dispatch/backend.rs` — no third HTTP path — and every interaction emits a
`kind = "escalation"` audit record. Turnover marks the session frontier-owned
(`DependencySession::set_frontier_owned`); later requests bypass the pipeline via
`dispatch_frontier`.

Terminal dispatch failure is an honest HTTP error, never a 200 carrying an
`ERROR:` body: context overflow (parsed from the fork's 400 refusal or an
OpenAI-style `context_length_exceeded` payload into `DispatchError::ContextOverflow`)
returns 400 naming the window and token counts; anything else returns 502. The
overflow never retries its own target but never stops the chain (`continues_chain`).
Failed dispatches record the diagnostic label as *rejected* history, never an
accepted answer. Before dispatch, the pre-dispatch guard
(`server/compaction.rs`) estimates prompt tokens against the role window and, on
projected overflow, rebuilds the request from the ledger's eager LOD5 per-turn
labels (zero model calls), fail-open. The ladder's cache is ledger-backed
(`ledger/context_cache.rs`): an exact normalized-question match over accepted
answers only, consulted strictly post-failure.

`dispatch/frontier.rs` owns the wire-format build/parse logic reserved for the
ladder: `DispatchError`/`is_retryable`, `OpenAiBackend` (its `parse_response`
reused by `OpenAiChatBackend`), the `Anthropic` helpers, and `StreamEvent`. The
old `DispatchBackend` trait, `LlmDispatcher`, `ProviderConfig`, and
`OpenAiCompatBackend` were deleted by the dispatch collapse (D4).

### Filter engine architecture (`filters/`)

Filters follow the **Chain of Responsibility** pattern (GoF): the `Filter` trait
declares `kind()` (`FilterKind::{Regex, Whitelist, HnswSimilarity, ModelClassification}`)
and `evaluate(ctx) → Option<FilterDecision>`. `DeterministicFilterEngine` holds
`Vec<Box<dyn Filter>>` and returns the first non-`None` decision. Built-ins:
`RegexFilter` and `InjectionDetectFilter`. Filters gate on `ConfidenceGate` (Luhn)
and scope themselves via `FilterContext` — `FrontierBound` (only frontier-bound
traffic) and `ContentNodeWrite` (always on the ledger write path, D1).
`FilterDecision::OutputFilter` carries `RegexMatch` structs with position data so
`CodewordAnonymizer` can substitute consistently. The same engine backs both the
pipeline pre-filter and the write-path scrubber (`ledger_guard.rs`).

## Key Compositions & Reusable Primitives

| Primitive | Source | Used by router at |
|-----------|--------|-------------------|
| `Component` / `WorkUnit` | `fluent-wvr` | Every pipeline stage, `PipelineOrchestrator`, `RouterServer`, `ResultScorer`/`Summarizer` |
| `DependencyGraph<K>` | `fluent-dag::dep_graph` | `DependencySession` |
| `ResultPool` | `fluent-concurrency::pool` | `dispatch/escalation/modes.rs` — parallel classifier slots (team) and hypotheticals (question) |
| `PriorityResultPool` | `fluent-concurrency::pool` | `AffinityScheduler` — priority dispatch with aging |
| `CreditGatedPool<J>` | `fluent-concurrency::credit_pool` | `ReviewWorker`, `EntityLinkWorker` |
| `CreditedFeedWorker<Item>` | `fluent-concurrency::feed_worker` | `LedgerTierWorker` + `ledger/overlay_worker.rs` |
| `Limiter` | `fluent-concurrency::pool` | `ClassifierStage` cap; `charts/{compile,execute}.rs`; `PlanRoute` |
| `WorkContext` | `fluent-wvr` | Carries request, caps, runtime through every stage |
| `Runtime` trait | `fluent-wvr` | Plugged via `fluent_concurrency::tokio_runtime()` |
| `LoadCache<K,V,E>` | `common-core::cache` | `HotSnapshotIndex` — bounded get-or-load LRU |
| `ArcIntern<str>` | `internment` | `ContentNodeStore` index keys; work-unit/graph asset names |
| `LatencyHistogram` | `common-core::metrics` | `Instrumented::with_metrics` |
| `retry_async` | `common-core::retry` | `RetryBackend`, `RetryClassifier`, `SupervisedBatch` retries |
| `AdaptiveHnsw`/`HnswIndex`/`knn_brute_force` | `fluent-db::{hnsw,vector}` | `ContentNodeStore` KNN (`AdaptiveHnsw`: brute-force below the corpus threshold, HNSW above); `charts/store.rs` chart index |
| `HttpClass` | `guidance-llm` | `dispatch/backend.rs` status classification (streaming + buffered) |
| `DispatchError::is_retryable()` | `fluent-router` (`dispatch/frontier.rs`) | retry/fallback decisions in `dispatch/backend.rs` and `server/dispatch.rs` |
| `LlmError::is_retryable()` | `fluent-concurrency::llm_queue` | `guidance-llm` client error classification |
| `parse_json_response` | `fluent_llm::parse` | `routes/rigor/`, `dispatch/escalation/assemble.rs` |
| `Decomposer` | `fluent_llm` | `dispatch/escalation/modes.rs` — question-mode decomposition |
| `first_accept_in_order[_sync]` | `fluent-concurrency::ladder` | spacy-rs annotation ladder, `Ladder::try_escalate`, `BackendChain`, `dispatch_real`. **Exception:** `TargetMatcher::match_target` (start-index skip + last-member-always-wins + per-rung audit) |
| `CachedEmbeddingProvider::with_adapter` | `fluent-llm::embeddings` | The embedding cache is namespaced by `(model, adapter)`; default keys byte-identically to before |
| `NlpPipeline`/`AnnotationRung`/`CorrectionIndex`/`lemma_grep` | `spacy-rs` (+ `fluent-concept`) | `NlpStage`; `SqliteConceptStore`/`SqliteCorrectionIndex` implement the `ConceptStore`/`CorrectionIndex` traits; `PreferredSenseIndex` is ledger-owned (M2a); `match_interlingua` tree filters; `FrameResolutionWorker`; `retrieval` composes `lemma_grep` |
| `OrtSessionRegistry`/`OrtLlmSession`/`grammar` | `fluent-onnx`, `fluent-llm::onnx_session` | `ort.rs` builds the registry; `onnx_chat_backend` wraps a `CausalLm` as `ChatBackend`; onnx residency on `LlmResidencyEngine`; `JsonObjectGrammar`/`JsonArrayGrammar` constrain decodes. Legacy encoder seams: `nlp_encoder_fetch`, `pii_prefilter`, `colbert_entity_scorer`, `onnx_colbert_reranker`, `disambiguation_overlays` |

## HttpClass: where it lives and why

`HttpClass` (`HardReject`, `TransientFailure`, `EscalationRequired`,
`UpstreamFailure`) is defined in `guidance-llm/src/http_class.rs`, re-exported via
`fluent_llm::HttpClass`, and consumed in two layers: (1) **`LlmClient`** in
`guidance-llm`, which checks HTTP status before parsing the body (non-2xx
short-circuits with `LlmError::RateLimited` or `LlmError::Api`); (2) **router
dispatch backends** in `dispatch/backend.rs`, which dispatch with a raw
`reqwest::Client` (not `LlmClient`) and so apply `HttpClass` directly —
`HttpClass::from_status` → `is_retryable()` → `DispatchError::RateLimited` (retry)
vs `DispatchError::Http` (permanent), on both the buffered and streaming paths.
`DispatchError::is_retryable()` returns `true` for `Http(_)`/`RateLimited`;
`LlmError::is_retryable()` (`fluent-concurrency::llm_queue`) treats `Http(_)`/
`RateLimited` as retryable and `Api(_)`/`NoResponse` as permanent.

## Import Boundaries (enforced)

`fluent-router` imports from `common-core`, `fluent-wvr`, `fluent-concurrency`,
`guidance-llm`, `fluent-types`, `fluent-dag`, `fluent-concept`, `spacy-rs`,
`fluent-onnx`, and std/`tokio`/`reqwest`. It does NOT import from `guidance`,
`coral`, `wasm_ipc`, `knowledge`, `ontology`, or `rdf`. `knowledge.rs` gives
coral's Context a reachable read path without the router importing coral.
Dependency direction is preserved: `fluent-onnx`, `fluent-concept`, and `spacy-rs`
never import the router (or each other's domain types); the router implements
their `ConceptStore`/`CorrectionIndex`/`EmbeddingProvider`/`SessionLoader`/
`LlmFetch` seams. The **binary** crate (`src/bin/coral-router/`) is the
composition root and additionally depends on `coral-context` and
`guidance-ontology` — its `boot.rs` runs the one-loader/two-homes YaGO load into
both the coral graph and the router's `SqliteConceptStore`.

## Pipeline data flow detail

1. **Server:** hyper reads the request; `server/handler.rs` collects the body
   (enforcing `max_payload`), merges query-string routing fields into the body
   when absent (body wins), and normalizes JSON → `RouterRequest` (`normalize`).
2. **Ledger (pre-pipeline):** `record_request()` writes the full request at LOD0
   before any filter runs (through the write-path scrub).
3. **Pipeline:** `WorkContext.structured["request"]` = serialized `RouterRequest`;
   the orchestrator calls each stage via `StageDecisionProducer` (typed) or the
   `WorkOutput` channel.
4. **Stage 1** (`DeterministicPreFilter`): extracts the user message, runs the
   filter engine, emits a `StageDecision` — command result, hard reject,
   output-filter flag (PII), or pass-through.
5. **Stage 2** (`ClassifierStage`): calls the LLM (or the tree engine), parses the
   verdict (action/target/scores/reason), checks coherence/safety thresholds.
   **Route-level `always_route`** overrides a classifier `action=respond` to
   `action=route` toward that route, and the same routes are advertised as
   "Dispatch rules". Resolves the route via `RoutingConfig::resolve_route()` with
   complexity-gated model selection + optional score-matrix ranking, or — when the
   pipeline opts in (`target_match: "self_assess"`) — via `TargetMatcher`. Emits a
   `StageDecision` carrying `metadata.response`, `metadata.routing_target`, or a
   rejection.
6. **Server (post-pipeline):** a `classifier_response` responds directly; a
   `routing_target` calls `handle_dispatch`, walking the primary target + its
   `fallbacks` through `ChatBackend`s (each `RetryBackend`-wrapped),
   short-circuiting on non-retryable errors. The client's explicit
   `instance`/`snapshot`/`id_slot` are overlaid onto the target; a 503 group-miss
   asks the `InstancePool` to allocate fresh KV before one retry. With no target,
   the handler dispatches to the classifier's own model as a fallback *target* (or
   a canned response). On dispatch+escalation failure the per-group `Ladder`
   (`try_escalate`) runs, short-circuiting on a `ContextCache` hit. Every local
   dispatch is a direct HTTP call on the owning spawned `llama-server`.
7. **Ledger (post-pipeline):** `record_result()` updates acceptance score +
   metadata, and — on routed and classifier-fallback branches — records the
   answer text into the ledger node (LOD0) and the session step
   (`SessionStepHandle::complete`; streaming records whatever is available at
   finalization). With a `session_id`, the request is tracked as a step in the
   session registry's `DependencySession`.

## Config-driven pipeline assembly

Pipelines are defined in `env/coral-router.json` under `pipelines`; each entry
controls the decision stages and their knobs:

```json
{
    "pipelines": {
        "default": {
            "deterministic_prefilter": true, "classifier": true,
            "classifier_model": "swarm", "coherence_threshold": 0.70,
            "blacklist": "env/pii-patterns.json", "score_matrix": { },
            "target_match": "self_assess", "target_match_timeout_ms": 300000
        }
    },
    "routes": {
        "prose":   { "group": "prose",   "description": "Write a story…", "always_route": true },
        "science": { "group": "science", "description": "Physics…",       "always_route": true },
        "local":   { "group": "default", "description": "General Q&A…",   "always_route": false }
    },
    "gguf_dir": "/app/ai/models/gguf"
}
```

The opt-in NLP/ONNX stages are enabled the same way
(`build_named_pipeline_with_backend_onnx_and_nlp`): `nlp: true` inserts the
`NlpStage` (needs a concept store; without one it warns and fails open to
`en_default`) running the deterministic-first ladder — base `[ArcEager, Rule]`
unconditional, then gated refine `[Encoder, Llm]` on confidence OR task-value via
`RefinePolicy` (fallback keeps base). Non-empty `overlay_models` +
`nlp_ordering: "deterministic_first"` inserts the `OverlayStage`
(`deterministic_first` without `overlay_models` warns and falls back to
`llm_first`), and `classifier_retry_max` wraps the classifier in a
`RetryClassifier`. `nlp_ordering` (`"llm_first"` default ⇒ `RefinePolicy::Always`,
`"deterministic_first"` ⇒ `RefinePolicy::OnUncertain`) and the explicit
`refine_policy` knob (`mode`/`min_overall`/`min_role_coverage`/`refine_on_ties`/
`min_token_score`/`refine_on_unresolved_critical_role`/`refine_on_unresolved_propn`/
`refine_on_collision_note`, each independently tunable) control when the refiner is
consulted. The default pipeline without these knobs is byte-identical to
filter → classifier.

The generative ONNX LLM (the `onnx/llm` role) is wired once and reached everywhere
through `onnx_llm_backend()` — LLM rung, review (`ParseReview` grammar),
enrichment, tiering, summarization, ranking, retrieval. `encoder_model` and
`overlay_models` remain feature-gated but are not configured in the reference
deployment. `target_match` (`"self_assess"` default | `"static"`) selects the
in-group policy (§"Model-group target selection"); `target_match_timeout_ms`
defaults to `DEFAULT_TOTAL_TIMEOUT_MS`. `routes.<name>.always_route` forbids direct
classifier answers on that route. `gguf_dir` feeds the admin CLI's weights
resolution and prefixes every relative `models.<model>.weights`/`template`
declaration at every setup seam (`resolve_weights`/`resolve_template`); `onnx_dir`
does the same for ONNX paths. `RouterConfig::build_named_pipeline_with_backend()`
constructs the pipeline from config, optionally injecting a mock `ChatBackend`.
The deterministic pre-filter uses `DeterministicPreFilter::from_config()` when a
blacklist path is present, else `::new()` (built-in PII patterns).

## Model-group target selection: an in-group target-matching ladder

Every model has an `intelligence` score (0–10) and every `model_group` an ordered
list of model keys. Selection within a group is complexity-gated, in one of two
modes (`pipelines.<name>.target_match`):

- **`"self_assess"`** (default) — the VISION ladder. At route resolution,
  `TargetMatcher` (`target_match.rs`) climbs the group: each candidate
  self-assesses the request's complexity via its own `ChatBackend` call (bounded by
  `target_match_timeout_ms` under the shared `Limiter`). The first whose assessed
  complexity does not exceed its `intelligence` — or the last member — becomes the
  matched target. The classifier's own estimate only seeds the *start* index. The
  ladder is DRY-shared between the flat classifier path and the tree engine.
  Singleton groups flow through assessment-free (no LLM call) but still resolve
  through the ladder (the expansion pick is residency-aware; the static rule is
  residency-blind). Every self-assessment and the final match emit a
  `kind = "target_match"` audit record.
- **`"static"`** — `RoutingConfig::resolve_route` picks the cheapest model in the
  route's group whose `intelligence` meets the classifier's `complexity` score; if
  none qualifies, the cheapest in the group.

In both modes `RoutingConfig::routing_target` populates `RoutingTarget.fallbacks`
via `all_dispatch_targets` (every model across groups, ordered by intelligence
proximity, primary group first, cost as tie-break). The ladder reorders the
primary/first fallbacks: the matched target becomes primary and its more
intelligent group tail `G[i+1..=n]` leads the fallback list (mechanical-failure
walk), followed by cross-group models. These are *target* candidates; a `fallback`
tree child resolves through the same path. `dispatch_real` walks the primary +
`fallbacks`, short-circuiting on non-retryable 4xx; only after the local chain is
exhausted does the per-group `Ladder` engage.

Every model in the chain is a candidate to answer — a fallback *target*; none backs
up the classifier. The classifier stage runs on the `classifier` role's head
candidate, and with no target the handler dispatches to that classifier key as a
fallback target. The matched target's answer is recorded in the session ledger and
step after dispatch (step 7).

**Always-route domains.** Routes with `always_route: true` (reference deployment:
`prose`, `code`, `translation`, `science`, `legal`, `medical` — single-member
groups backed by a stronger model) never let the classifier answer directly; step
5 overrides `action=respond` into `action=route` deterministically. This keeps a
small overconfident classifier from "writing" prose or "answering" legal/medical
itself. Routes without the flag (`local`, `extract`, `summarize`) keep the
direct-answer path. All config: the prompt's "Dispatch rules" section is generated
from the same flags.

## Instance pools, the serving layer, and the sidecar

**Coral Router owns the serving processes.** A model entry is *managed* when it
declares a weights source or an instance pool: `ModelEntry.weights` (local GGUF),
`hf_repo`/`hf_file` (on-demand HF load), or `instances`. An `onnx` block is a
separate serving path (never a llama-server). At boot `main.rs` builds a
`LlamaServerSupervisor`, finds `llama-server` on `$PATH`/`LLAMA_SERVER`, and uses
**on-demand residency**: only models declaring at least one pinned instance spawn
at boot (free localhost port, awaited `/health`); the rest load lazily by the
dispatch path (`supervisor.ensure_running`, spawn-locked so concurrent dispatches
never double-spawn) and unload when the sidecar evicts their last context
(`supervisor.unload`; the spec stays registered). Each spawned model's `endpoint`
is rewritten to `http://127.0.0.1:<port>/v1/chat/completions`, so every classifier,
dispatch target, and backend points at the owned server. The supervisor supervises
each child for the process life — logging output, restarting with capped backoff,
and probing `/health` post-boot to kill+restart a hung server. In `--mock` mode
supervision is skipped.

The llama.cpp router mode is never used: each spawned server loads its model's
weights once and allocates many named contexts ("instances") from them (separate
KV + compute buffers sharing weights). A `count: N` profile expands to N siblings
`<key>-0`..`<key>-{N-1}` in the shared `group` (each its own full-size window;
`parallel` slots share one window). `num_ctx`/`parallel`/`sleep_idle_seconds` are
declaration-only (coral-router strips them from bodies); `sleep_idle_seconds`
survives as an eviction-priority hint. The grammar generator
(`instances::instance_grammar_string`) emits the `--instance` flags —
`name[:group=G][:ctx=N][:parallel=M][:pinned][:default]` (reference `swarm` pool:
`swarm-0`/`swarm-1` 8192 pinned, `ledger` 65536 pinned default, `scratch` 131072).

**All-lazy pools register `/instances` too.** Only pinned instances are declared at
spawn (unpinned are created on demand). A pool with **no pinned instance** declares
its full grammar at spawn — the fork registers `/instances` only with `--instance`
grammar, so an all-unpinned pool that declared nothing would be invisible to
`/v1/models`, `/instances`, and `coral-router ps`. It stays lazy (`boot` false).

**Model id translation.** Config keys are public ids; llama.cpp names are internal
(`llama_model_name` resolves `--alias`; dispatch sends `<llama-name>[:<instance>]`).
Roles are the routing vocabulary: `roles.<role>.models` lists the keys serving the
role (fanned out per request; the `last`/`any` sentinels apply when a role declares
none), while `models.<model>.roles` is a params-only override map (never
membership). Each role owns its run block (`roles.<role>.params`; the fleet block
is `roles.default.params`, which also supplies spawn defaults) and its named pool
(`roles.<role>.instances`). Sampling composes role-base ← pool profile ← binding ←
entry top-level (entry wins). One qualifier precedence serves every path
(`resolve_inference_point`): explicit qualifier, else the role window
(`<model>:<role>`), else a bare key. Special-purpose roles ride the same machinery:
`classifier` (`classifier_role_key` is the single classifier-key source — no
top-level `classifier_model`) and `embedding`. Backend construction
(`llama_chat_backend_for_key`/`for_instance`) and dispatch wire ids
(`RoutingTarget::from_model_entry`) agree.
`RoutingTarget::from_model_entry_instance` targets a named point (`<base>:ledger`);
`local_backend_for_instance` overlays the entry `params` onto the named profile's
`params` (entry wins) and strips declaration-only keys. `resolve_pipeline` resolves
`model: "<id>:<instance>"` (or `:<group>`, `:latest`) directly, bypassing the route
table. Routing fields `instance`/`snapshot`/`id_slot` are read from body + query
(body wins), preserved through `normalize`, overlaid in `dispatch.rs`.

**Public `/instances` API.** `InstancePool` (`instances/pool.rs`) is the aggregate
facade over every managed server, served at Coral Router's own address
(`server/instances_api.rs`) as the single sidecar entry point — `GET/POST
/instances`, `DELETE /instances/:name`, pin/unpin, snapshot endpoints, and resume
control (`POST /instances/<model>:<name>/resume` and `/no-resume`) — mirroring the
llama-server contract under `<model_id>:<name>` ids with 64-bit-summed `total`. The
aggregate envelope carries the router-side `resume` flag per instance and the
synthesized plain-model footprints. `GET /v1/models` lists one entry per instance
plus aliases; `/props` proxies the default server and adds `total_slots` + an
`instances` array; `/memory` is a compat reshape; the model-less endpoints
(`/tokenize`, `/detokenize`, `/apply-template`, `/control`) proxy the default
server. All management endpoints require the API key when `sidecar.api_key_env`
names a variable. Managed servers bind to `127.0.0.1` only.

**Sidecar residency.** Each `InstanceManager` talks directly to its server's
`/instances`. At boot the pool validates every grammar (`validate_instances`,
fail-fast on duplicate names / group collisions) and the server runs each
manager's reconcile (create missing, resize `n_ctx` drift, tolerate a 409) and
residency loop. The loop **always** polls the aggregate `GET /instances` envelope
(`total` is the VRAM signal) and logs free/used; eviction is gated on the
allocation budget `device_total - minimum_remaining_vram`.

**The onnx context pool.** `fluent-onnx`'s `OnnxContextPool` wraps one
`OrtLlmSession` (single `Arc<Mutex<Session>>` + tokenizer) with a
`ConcurrentRegistry` of named `OnnxContext`s, each owning its own KV — created on
demand (`ensure_context`), interleaved on the shared session, honoring the same
lifecycle knobs (`n_ctx`, `max_ctx`, `pinned`, `resume`, `last_used`). Both fleets
present through the shared `fluent-llm::runtime` traits, so the aggregate
`/instances` facade, `/v1/models`, `coral-router ps`, `POST /models/unload`, and
`POST /instances/:name/resize` drive llama and onnx uniformly (llama rows
byte-identical; onnx rows appear only when configured).

**Plain models are resident too.** A managed model declaring only `weights` (no
`instances`) is served by a plain `llama-server` whose `/instances` 404s. The
manager's `list_with_fallback` synthesizes a single `<model>:default` entry from
`/props` (`n_ctx`, `is_sleeping`) plus the configured weights file size, so
`/instances`, `/v1/models`, the residency budget, and `coral-router ps` account
for a plain model's resident weights — 0 bytes while the fork reports
`is_sleeping`, the file size once loaded. `ps` reports resident `model_bytes`,
never the on-disk size of a sleeping model.

**Footprint-weighted eviction.** Over budget, `evict_to_fit` evicts units from
`gather_residency` — each either a single unpinned context (`Evictable::Context`,
frees KV + compute) or a whole model with no pinned instances (`Evictable::Model`,
frees weights *and* every context). A model with any pinned context keeps its
weights resident; `pinned` instances are never candidates. Units are ordered by a
`freed_bytes × idle-time` score (largest coldest footprint first), so a 10.5 GB
weight pool is a real eviction target while a just-used model stays. After the
pass, zero-context models are unloaded (`unload_empty_models`).

**Load-time admission control.** `ensure_target_ready` (called on every dispatch)
never spawns/wakes a cold model without making room: `make_room_for(model_key,
required_bytes)` runs the same eviction until `used + required ≤ budget`, where
`required_bytes` is the full incoming footprint — weights *plus* the KV window the
new context needs (`load_bytes_for`: its own profile's window, else the largest
configured, else weights alone). The budget is the pre-admission hard gate
`allocation_limit()` (`total − minimum_remaining`); the
`vram_low_watermark_bytes` floor belongs to the reactive residency loop only and
never gates admission. Residency is judged by actual resident state, not the
process flag — a plain model whose fork slept its weights out (`is_sleeping =
true`) is treated as not resident, so waking it reloads weights only after room is
freed (preventing the OOM abort a naive wake causes). The shared engine carries
the two budgets separately (reactive at construction, admission via
`with_admission_budget`); a missing budget disables that half.

**Resume (preserve-on-evict).** An instance can be marked `resume` (config profile,
`POST /instances` body, or the resume op). On eviction a `resume` context is first
KV-snapshotted as `<name>-resume` (best-effort; a failed save logs and eviction
proceeds) — **only in `auto` snapshot mode**; `manual` destroys without saving and
audits `evict_skip_snapshot`. The mode resolves per model from
`models.<model>.snapshot`, else its serving roles' `snapshot` blocks (first sorted
wins), else `Auto` (unset ⇒ today's behavior). The transcript is already durable in
the ledger, so KV + text log together preserve the workload. A later dispatch with
`snapshot=<name>-resume` and the same `session_id` restores it, subject to the
identity gate (known weights hash/quant/fork-version drift rejects; unknown fields
never reject). The router clears the flag and deletes the snapshot via `/no-resume`
or automatically once idle past `sidecar.resume_ttl_s`. `resume` is moot on pinned
instances. A 503 `"no free instance in group"` triggers `ensure_group` (a fresh
`<group>-<uuid>`) before one retry. `sidecar.slot_save_path` is created at boot,
feeds `--slot-save-path`, and drives `KvSnapshot::file_path` so router metadata and
server layout agree. The management client reuses `OpenAiChatBackend`'s
raw-reqwest/`HttpClass` pattern.

**Admin CLI (`coral-router ps`/`list`/`show`/`pull`/`rm`).** Reads the aggregate
`/instances` + `/v1/models` envelopes and the config to render the live residency
picture as one borderless grouped table (`cli::ps_table` over `cli::tables`,
terminal-width aware, no ANSI): a weights row per model, one row per instance with
its `VRAM`/`SYS RAM` split, `CTX`, weights `QUANT`, KV `KV` (from `cache_type_k/v`
defaults), and `LIVE` idle-until-sleep; a per-model `TOTAL` row; `unloaded` for
models with no instance detail; a `total … resident · models · instances` footer.
`list` renders the same format over the resolved config (`ROUTES`/`ROLES`) plus a
`gguf_tool ls`-shaped MODELS table from `gguf_dir`. Both take `--markdown`. The
default config resolves by walking up from PWD (`env/coral-router.json` →
`coral-router.json` per level → `<HOME>/.config/coral-router/…`); a relative
`gguf_dir` resolves against the loaded config's directory. Resident weights bytes
come from the envelope's `model_bytes` (0 while slept out); the router overlays
weights identity (`short_id`/`arch`/`quant`) on each `/instances` row so `ps` needs
no local weights file. The GGUF root for size fallbacks is `--gguf-dir`, else the
config's `gguf_dir`, else the built-in default.

**KV snapshot round-trip.** `SnapshotStore` may hold an optional `InstanceClient`
(`with_fork_io`); `save_snapshot` then POSTs to the owning server
(`POST /instances/:name/snapshot`) via `common_core::runtime::block_on` and
records metadata locally; `list_snapshots`/`delete_snapshot` delegate. Rigor's
blue-pass completion triggers the save, so the blue→rewind→red flow sends a real
`snapshot`/`instance`/`id_slot` on the next dispatch. Without the handle these
degrade to metadata-only no-ops.

**Boot composition (`ledger`/`session` sections).** When `config.ledger` is
present, `main.rs` opens a `ContentNodeLedger` (path, or in-memory with a `warn!`)
and attaches the DIP `Summarizer` backend via `summarizer_for_ledger()` (targeting
`<base>:ledger`). When `config.session` is present it builds a `SessionRegistry`
mapped to `session.root`. Both attach via `with_ledger`/`with_sessions`, so rigor
rewind and LOD derivation exist at runtime. Both sections are default-absent —
existing deployments are byte-identical until they opt in.

## Ledger: condensed context architecture

`ContentNodeLedger` is a thin facade over the shared `ContentNodeStore`: every
request is stored at full detail (LOD0) before the pipeline runs, results recorded
afterward. This separates durable storage from live working context (stages read
from `WorkContext`, not the ledger; the orchestrator/session reads condensed
summaries via `LedgerView::render`).

Key load-bearing properties:

- **Write path is checked.** Every write reaches `ContentNodeStore` only after
  `ledger_guard::scrub_for_ledger` (the builtin engine with `ContentNodeWrite`
  scope); PII text is irreversibly replaced (`[REDACTED:<pattern>]`), no codeword
  map retained. Direct store writes are the documented bypass.
- **LOD lifecycle.** LOD0 (full text) + LOD5 (label) are eager; LOD1–LOD4 derive
  lazily, always from LOD0 only (never chained), via `Summarizer`, cached at most
  once. `CompactionStrategy`/`RecencyCompaction` demote older nodes (setting
  `active_lod`).
- **Views never own text.** `LedgerView::render` is the single text-exit;
  `ParallelLedger` gives independent default-LOD views over one shared store;
  `FilteredLedger<V>` is a reference overlay (exclusion set + optional render
  transform) used by the PII frontier view and the rigor red-team view. Rendering
  degrades to LOD0 when a lazy tier is un-derivable.
- **Shared store.** Nodes live once behind `Arc<RwLock<ContentNode>>` with
  interned `ArcIntern<str>` index keys and durable `content_json` hydration
  (seeded `next_id` from `MAX(node_id)`).
- **Tiered annotations keyed to content.** Every annotation carries a
  `ProvenanceTier` (`Deterministic < LocalModel < Frontier < HumanReview`);
  `write_annotation` routes a claim to `AnnotationStore` under the node's current
  `content_hash`; a higher tier supersedes (never deletes) a lower one, and
  mutating LOD0 changes the hash, invalidating cached claims by keying — no
  staleness scheduler.

### Background tier derivation (`ledger/tiering.rs`) — opt-in

`LedgerTierWorker` derives LOD1–LOD4 (and upgrades LOD5) in the background: boot
backfills underivable nodes, then drains newly-recorded nodes from the
`ContentNodeStore` tier-event feed. Concurrency is bounded by a shared `Limiter`;
writes are at-most-once (re-checked under the node write lock); summarizer
failures degrade to a higher LOD rather than erroring; a successful higher-tier
derivation downgrades back (never repeats work). Enabled only when
`ledger.background_tiering = true`. The feed mechanics compose the shared
`CreditedFeedWorker<NodeId>` (bounded feed, `CreditFlow` gate, `Limiter`, drain
loop, boot backfill). The feed is a bounded `mpsc` (`queue_capacity`, default 1024)
plus a `CreditFlow` gate: the async producer acquires a credit token before
forwarding a `NodeId` (blocking while exhausted) and the worker releases one per
processed node, so a burst cannot grow the feed without bound. Knobs:
`ledger.tier_credit_limit` (256), `ledger.tier_credit_more_after` (8). Synchronous
write paths enqueue via non-blocking `try_send` (skipping on a full feed; agent
nodes stay covered by the credit-gated enqueue, boot backfill catches stragglers).

### Background node-annotation overlays (`ledger/overlay_worker.rs`) — opt-in

`OverlayWorker` derives three overlays in the background — the spacy
`NodeAnnotation` (`annotation`), the LLM enrichment summary
(`metadata["llm_overlay"]`), and the dense `embedding` — each lazily and
**at-most-once** from LOD0 only (an overlay never reads another overlay). It is
the **second consumer** of `CreditedFeedWorker<NodeId>` and deliberately composes
that primitive rather than reimplementing `LedgerTierWorker`'s shape. Per node it
**fans out the three derivations in parallel** on the injected `Runtime` (no
ambient `tokio::spawn`) and joins them, so a slow/down LLM never delays the spacy
or embedding overlays. Each derivation goes through the store's at-most-once,
fail-open `annotation_for`/`embedding_for`/`llm_overlay_for` (a missing/failing
seam leaves that overlay off, `status: failed`, never retried). Emits a
`kind = "overlay"` audit record per derived overlay. Opt-in via
`overlay.arc_ready.enabled` (config-absent is byte-identical); requires a ledger;
each seam attaches only when its model/pipeline resolves; drained on graceful
shutdown.

### Prompt assembly (`ledger/prompt.rs`) — pure function object

`LedgerPromptAssembler` builds a fidelity-budget-fit prompt from a session's cached
LOD tiers: first/last LOD0 anchors guaranteed, intermediate nodes relevance-ranked
(lexical + recency) and demoted to the highest LOD that fits; only *cached* tiers
are rendered (never triggers summarization cost). Pure (no I/O), unit-tested, and
shared by the coordinator and the plan route.

### Agent coordinator (`ledger/orchestrator.rs`) — default-on wherever a ledger exists

`LedgerAgentCoordinator` runs the agent loop over a session's shared ledger: restore
a KV snapshot (`KvSnapshotPolicy`: never / always / restore-if-same-model) or
assemble the prompt → execute the LLM call (on a blocking task, session guard
dropped — never held across the call) → record the exchange → snapshot KV
(`advance_and_snapshot` → `SnapshotStore`) → enqueue a tier derivation
(credit-gated) → complete the step. `last_resident_model` is session-scoped. A
present `ledger` section attaches it unless the operator opts out
(`ledger.orchestrator.enabled = false`) or rolls back at runtime
(`POST /admin/coordinator/rollback`, `coordinator_gate`). `node_plan` metadata on
recorded nodes feeds workflow-learning replay. Two opt-in layers: KV-affinity
scheduling (`ledger.orchestrator.affinity_cap = <cap>` attaches an
`AffinityScheduler` via `build_affinity_scheduler`; `run_agent` sessions become
KV-affine, starved sessions age up) and live retrieval (compose a
`NodeRetrievalService` via `with_retrieval` to expose `retrieve_nodes`, pre-filtered
through `SalienceRanker`). Both reuse the shared primitives (`ContentNodeStore`,
`SnapshotStore`, `DependencySession`/`CheckpointedStepGraph`, `Limiter`,
`AffinityScheduler`, `CreditFlow`) rather than a parallel store.

## Natural-language processing spine (spacy-rs + fluent-onnx)

The deterministic NLP layer composes `spacy-rs` (`src/spacy-rs/`) and
`fluent-onnx` (`src/fluent-onnx/`), giving a unit-testable, model-free
understanding of text *before* any model is consulted and a content-addressed
**interlingua id** on every token that is a pure function of the text and the
YaGO 4.5 taxonomy. Detail lives in `doc/spacy-rs/{VISION,ARCHITECTURE}.md` and
`doc/skills/interlingua/SKILL.md`; this section maps router consumption.

**Annotation ladder (`spacy-rs`).** `NlpPipeline` runs a **deterministic-first,
two-phase** ladder: base `first_accept_in_order` over `[ArcEagerRung, RuleRung]`
(always a validated parse), then — when `RefinePolicy` says so — refine
`first_accept_in_order` over `[EncoderRung, LlmRung]` (`LlmRung` via
`stages/nlp.rs::chat_backend_to_fetch_sync` + the onnx LLM default; `EncoderRung`
feature-gated, unconfigured). Refinement is gated on confidence (low
`overall`/`role_coverage`/near-tie `oracle_margins`) **OR** task-value
(routing-critical role unresolved, PROPN with no entity id, surfaced
`CollisionNote` — each behind its own `refine_on_*` flag) and adopted only when it
passes the 7-check gate **and** does not regress `frame_coverage`. `Always`
preserves today's `LlmFirst`; `OnUncertain` consults models only on
uncertainty/incompleteness; the rule rung guarantees a parse even when a model is
unavailable. The builder threads a resolver (`NlpDeps`): with `nlp: true` and a
concept store it builds `en_default_with_strings` + a live `InterlinguaResolver`
(over `SqliteConceptStore`), else `warn!`s and uses `en_default()` (fail-open).
`NlpStage` carries the effective `RefinePolicy` (from `nlp_ordering` unless
`refine_policy` overrides, via the `RouterRefinePolicy` DTO mirror in
`config/refine_policy.rs` — `spacy-rs` never deserializes router config, F5);
`NlpStage::new` defaults to `Off`, the builder is the single source of
`Always`/`OnUncertain` (F1/R6). Span-cache keys are fixed-width hex `span_key
TEXT` (`{:016x}`), not `i64` (F7); the trait + hermetic double live in
`ledger/span_cache.rs` (M2b), reaching the ladder only via `span_cache_seam`.

**The NLP stage.** `NlpStage` publishes per-sentence
`spacy_rs::routing::RoutingSignal`s under `"nlp_parse"`. It is **enrichment, never
a gate** — a parse problem must not reject the request. The classification tree's
`match_interlingua` filter nodes (`stages/tree/engine.rs`) dispatch on parsed ids
directly (same phrasing → same ids → same route, zero tokens) and gate on the
signal's `confidence_min`.

**Frame resolution.** `spacy-rs::frame::FrameStage` sits between `attach` and
`resolve`, deriving per-predicate `Frame`s + a typed `AmbiguityEntry` list and
minting a **permanent** interlingua key only for an ambiguity-free frame
(provisional otherwise). `ledger/frame_index.rs` owns the `PreferredSenseIndex`
seam (M2a) over the `interlingua_index` pattern-cache rows (`role='sense'`) and runs
the wave-batched `FrameResolutionWorker` — one grammar-constrained call per
`SupervisedBatch` wave returning N resolutions, applied + promoted so a repeating
`(predicate, ambiguity_kind)` pattern replays deterministically (zero LLM cost next
time).

**Interlingua on the ledger.** `ledger/nlp.rs::record_parse_node_with_confidence`
writes the parse node + the per-sentence × role `interlingua_index` rows in one
transaction; `SqliteConceptStore` (`interlingua_concepts`) is the durable home for
the YaGO taxonomy, and `boot.rs` reconciles it id-membership equal to coral's
content-addressed graph at boot (one loader, two homes). Annotations are tiered
(`ProvenanceTier`) and keyed to `content_hash`; `AnnotationStore` supersedes —
never deletes — a lower-tier claim for the same node version.

**Async review.** `ReviewWorker` (`server/review.rs`) runs
`spacy_rs::review_prompt` + `apply_corrections` against `SqliteCorrectionIndex`,
served at `POST`/`GET /v1/sessions/{id}/review-parse`, the call served by the onnx
LLM with the `ParseReview` grammar; an optional PII pre-filter flags candidates
when `review.auto_enqueue` is on. Corrected annotations write back with
`HumanReview` provenance.

**Entity-link overlay.** `EntityLinkWorker` (`server/entity_link.rs`) scores
unresolved PROPN spans against the boot-baked ColBERT `EntitySimilarityIndex`
(`ort.rs::colbert_entity_scorer`) and writes **candidates only** to
`overlay_candidates` (`ledger/overlay.rs`) — parse-time
`interlingua_entity_id`/`concept_ids` stamping remains boot-only-registration
(future work, the VISION's Partial surface).

**Ranking pipeline (`ranking.rs`).** The local model ranks only a **deterministic
shortlist**, never the full session context. `SalienceScorer` weights four
ledger-derived signals (`frame_frequency`, `interlingua_centrality` — a minimal
`pagerank` power-iteration over the `interlingua_index` co-reference + `parent_id`
graph — `recency`, and `reference_count`); `LedgerSalienceProvider` loads them from
a read-only snapshot. `rank_candidates` pre-filters to top-K, then makes **one**
grammar-constrained onnx call over the shortlist only (degrading to salience order
on failure).

**Live retrieval seam (`retrieval.rs`).** `NodeRetrievalService` parses a candidate
node's LOD0, runs spacy-rs' pure `lemma_grep` (every hit carries `ParseConfidence`)
+ the router-owned fuzzy paraphrase axis (over an injected `EmbeddingProvider`) +
the `cross_check` combiner, and pre-filters through `SalienceRanker`. Each report
carries per-sentence `predicate_ids`; `filter_by_interlingua` (default-off)
drops topically unrelated candidates before ranking.
`LedgerAgentCoordinator` exposes it via `with_retrieval`/`retrieve_nodes`
(fail-open without it).

**Serving.** The generative ONNX LLM is a first-class serving surface alongside the
llama fleet: `main.rs` builds `onnx_llm_backend` via
`ort.rs::onnx_chat_backend(registry, OnnxRole::Llm.registry_key())` — a
`fluent_onnx::OrtLlmSession` (`CausalLm`) wrapped as a
`fluent_llm::client::ChatBackend` — and `RouterConfig::onnx_llm_backend()` is the
**single wiring point** for the LLM rung, review, enrichment, tiering,
summarization, and ranking. The checkpoint is the general-purpose LFM2.5-2.6B
export, served **in-process on CPU** (`execution_provider: cpu`, `intra_threads:
1` for deterministic decode), bounded by a `Limiter`, so it runs concurrently with
the GPU-resident llama fleet. Structured calls request `response_format.schema`;
the `ort` backend constrains decode with a structural JSON automaton
(`JsonObjectGrammar`/`JsonArrayGrammar`).

**Shared runtime base (`fluent-llm::runtime`).** Both fleets present through the
same control-plane traits — `LlmWeights`, `LlmContext` (one named context with its
own KV), `LlmKVCache`, `LlmResidencyRow` — so `/instances`, `/v1/models`,
`coral-router ps`, and `POST /models/unload` drive llama and onnx uniformly. The
**single `LlmResidencyEngine`** replaced the two sibling loops: one pass over every
weights instance with injected per-fleet budgets (VRAM for llama, CPU RAM for onnx)
and eviction orderings (`FootprintColdness` for llama, `LruLargest` for onnx — never
unified): idle release past a per-role `sleep_idle_seconds`, LRU-largest eviction
against `sidecar.onnx_working_set_budget_bytes`, and `pinned`/`Always` never
released. The onnx role serves **named contexts** from an in-process
`OnnxContextPool` (one weights load, N windows, per-context KV), and
`POST /instances/:name/resize` routes onnx contexts through `LlmContext::resize`
(bounded by `max_ctx`). There is no onnx-side residency loop of its own.

The **encoder-role world** (two-tower `zero_shot_routing` Prompt-Router overlays,
the `fill_mask`+`annotation_heads` encoder rung, the `token_classification`
PII-Detector, the Policy-Linter, the `late_interaction` ColBERT retriever) is a
feature-gated legacy seam behind `cfg(feature = "onnx")` in `ort.rs`; no
encoder-role model is configured in the reference deployment (the general-purpose
LLM does enrichment). The opt-in PII pre-filter and the entity-link ColBERT
`EntitySimilarityIndex` remain live seams where enabled.

## Logging: two-stream architecture

Operational and audit logs are separate streams with independent retention:

| Stream | Format | Retention | Filter | Writer |
|--------|--------|-----------|--------|--------|
| Operational | JSON or text (configurable) | Configurable rolling files | Standard `EnvFilter` | File + optional stderr |
| Audit | Always JSON | Longer (90-day default) | `router.audit=info` | Separate file appender |

Every audit producer emits through `audit::emit(kind, detail)` into the single
`router.audit` `tracing` target; kinds are distinguished by the `kind` field,
never a second dot-namespace. Configured via `env/coral-router.json` →
`logging.audit_log`. Uses `tracing_subscriber::fmt::Layer::boxed()` with a 4-arm
match (console yes/no × audit yes/no).

## Config format (`env/coral-router.json`)

A deployment artifact: its *normalized* shape is pinned by the byte-parity oracle
(`config_normalize_parity` snapshots under `src/router/tests/data/config_normalize/`,
re-recorded with `UPDATE_SNAPSHOTS=1` only after an intentional change and a hand
diff review), not its raw contents (operator-owned). See
`env/coral-router.json.example`.

- **Roles are the routing vocabulary.** Groups reference roles (expanded per
  request to bound models). Membership lives on the role side:
  `roles.<role>.models` lists the model keys; `models.<model>.roles` is a
  params-only per-role override map (never membership). Each role owns its run
  block (`roles.<role>.params`; the fleet block is `roles.default.params`) and its
  named pool (`roles.<role>.instances`). Non-`default` roles declare only deltas
  (sparse-merged per key at every depth). A role carries no qualifier: it resolves
  from the route (resident models first — `last` prefers the loaded model — then
  intelligence-passing, then cheapest), and every dispatch names the role's window.
- **Params compose in one order through one merge** (`overlay_params`): fleet →
  role → pool profile → per-model binding → model entry top-level. Every layer must
  be a JSON object (boot warns over non-objects). Declaration-only keys
  (`num_ctx`, `parallel`, `sleep_idle_seconds`, `rope_freq_base`) never reach the
  request body.
- **Snapshot mode is per model or role.** `models.<model>.snapshot` or
  `roles.<role>.snapshot` (`{mode: manual|auto}`, default `auto`); the model entry
  wins, else the first sorted serving role that sets one.
- **Duties name groups, never models.** `ledger.group`, `charts.selector_group`,
  and the classification root's `model_group` resolve through the groups table
  (first servable member serves, sentinels skipped), so one group rebinding moves
  every duty together. Literal model keys remain legacy fallbacks.
- **Groups are escalation heads.** Members list in escalation order; the
  `last`/`any` availability suffix is implicit (opt out per group with
  `"sentinels": false`). A group declaring no role/model members rides `default`.
- **Endpoints are supervisor-assigned.** Managed models (`weights`/`hf_repo`) omit
  `endpoint` — boot rewrites it to the spawned server's port. Only external models
  declare one (boot warns otherwise).
- **Opt-in workers.** Review, overlay, and in-process ONNX roles are absent by
  default; the router fails open without them.
- **Strict schema.** Unknown top-level and model-entry fields fail at parse
  (`deny_unknown_fields`) — typos and retired keys (`default_params`, top-level
  `classifier_model`, per-model `sessions`, `retrieval_model`,
  `review.annotation_model`, `review.pii_model`) are loud errors, not silent
  misroutes.
