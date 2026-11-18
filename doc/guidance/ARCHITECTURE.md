# guidance — Architecture

> Map for code assistants. Goals without mechanism live in
> `VISION.md`; this document describes the mechanism as built: which
> file owns what, how the planes compose, and what already exists so
> nothing gets reimplemented. Every module named below exists under
> `src/guidance/src/` (or the stated sibling crate) — check before
> adding a row.

## What it is

`guidance-core` (`src/guidance/`, the consumer-layer crate) owns the
mirror, the query pipeline, and the maintenance loop. It is
intentionally a coupling hub: it composes domain capabilities into
application logic, and modules that become independently useful get
extracted to their own crate. The `guidance` binary
(`src/bin/guidance/`) owns the CLI surface and stays thin: it parses
argv, builds engines, prints reports, and maps exit codes.

## The three planes in code

| Plane | Entry points | Owns |
|---|---|---|
| Read | `read::engine` (`ReadEngine::explain`), `read::planes/*`, `query/*` recall | Intent-routed answer assembly, per-plane retrieval, budgets, envelopes, citations |
| Maintain | `pipeline.rs`, `waves.rs`, `fix.rs`, `index_pipeline.rs`, `sync_engine.rs` | Stage order, waves, fix rungs, gates, fragment/member indexes |
| Control | `maintenance.rs`, `orchestrator.rs`, `plans.rs`, `policy.rs`, `triage.rs`, `journal.rs`, `scorecard.rs` | Queue, contracts, calibration, ledger, review, ratchets |

## Store of record

| Store | Role | Code |
|---|---|---|
| Source tree | Truth; everything derives from it | — |
| Sidecars (`<guidance>/src/**/*.json` with a header first line) | Per-file derived facts and receipts | `sync/json_store.rs`, `sync/json_writer.rs`, header codec in `fluent-types` |
| Derived index (SQLite fragments/symbols/edges plus FTS, summaries, lemma dictionary, embedding status) | Disposable; refused on generation mismatch, rebuilt from sidecars | `search-vector` tables (`fragments` + FTS over content/symbol/doc, `fragment_summaries` + FTS, `fragment_lemmas` integer keys + `lemma_terms`, `fragment_embedding_status`, symbols/edges, `nodes` shared document store, `capabilities` + FTS over title/purpose/keywords + lemma postings, `project_cards` + FTS), writers in `query/ingest.rs`, `query/db_storage.rs`, `index_pipeline.rs`, card importers in `read/doc_cards.rs` + `read/project_map.rs`, guard in `index_generation.rs` |
| Control state (queue snapshots, calibration registry, intent records, journal, escalation ledger, run reports) | Durable and not derivable; back it up | `maintenance.rs`, `journal.rs`, `orchestrator.rs`, `plans.rs` |

Mtime is a cache over this layout; content hashes decide. The
fragment index never reprocesses clean files (`reprocessed_clean`
stays zero on converged passes), and the member-JSON path never
rewrites identical bytes without a completion stamp. The generation
marker (`index.generation`: index schema 9 plus sidecar schema 1)
refuses reads after any sidecar/schema change so callers rebuild
instead of serving a mixed pair. Fragment rows carry the writer
schema (v3); answers carry the envelope schema (v1). Decision specs
are versioned per point (`intent` v2, the rest v1); changing the
model, prompt, or question version returns the point to shadow.

## Module map (`guidance-core`)

### Read plane

| Module | Responsibility |
|---|---|
| `read/engine.rs` (`ReadEngine`) | The shipping answer path: intent-routed assembly, per-plane retrieval, budgets, claims, v1 envelope. `explain --no-llm` is evidence-only with no model pipeline build |
| `read/planes/{symbol,graph,code,docs,orientation}.rs` | The five retrieval planes. `symbol`: exact definitions via the `symbols` table (homonyms listed, unknown yields empty). `graph`: callers/callees/dependents/owners over the hydrated graph with resolution tiers; ownership reuses `component_map`, no second table. `code`: FTS + vector + lemma recall within code only plus rg normalization (fusion stays in `run_recall`). `docs`: capability cards over `capabilities_fts` plus lemma postings, live-section proof, drifted sections render as gaps. `orientation`: project/ownership/domain cards first — `Orientation` and `Ownership` queries start here, never in code recall |
| `read/{intent,vocabulary,budget,claims,decide,calibration}.rs` | Rule-table intent (replaces the word-count ladder for the read route; the legacy `FsmEngine::classify` is untouched), one query vocabulary (significant-term rule shared with `query::funnel`), token budgets over `fluent_llm::tokens`, claim support, native `DecideBackend` judge (closed labels; everything stays `Considered` until the capability point calibrates), task-value bars |
| `read/{admission,readiness,render,doc_cards,project_map}.rs` | Doc-admission measurement (reports only, promotion is an explicit `calibrate` run), readiness as coverage not confidence, card/section rendering over `query::formatter` with budget caps, the single markdown chunker plus model-free capability-corpus import, deterministic workspace/crate/domain cards from `README.md` plus manifests plus gates plus jump terms |
| `read/evidence.rs`, `source_ref.rs`, `source_scope.rs` | One deduplicated evidence set (first-seen rank order) across producers; one workspace-relative source identity for findings, citations, globs, live reads; one scope classification (product/doc/capability/test/fixture/eval/data) shared by selection, read, and graph |
| `query/strategy.rs` | Intent FSM (`Intake`/`Classify`/`Route`/`Validate`) plus the exact pre-check single home. Twelve intents with priority order: `SingleIdentifier` 0, `FilePath`/`Graph` 1, `CapabilityQuery` 2, `Ownership`/`Orientation` 3, `HowTo`/`Conceptual`/`Provenance` 4, `MultiKeyword` 5, `GeneralSearch` 6. `Graph`, `Ownership`, `Orientation`, `Provenance` are read-plane only |
| `query_engine.rs` | Retained for the `search` command surface only (`format_stages`, `summary_mode`); authorizes through the registry trio (`authorize`/`authorize_advisory`/`authorize_pinned`). No longer an answer path |
| `query/envelope.rs` | Unified answer envelope: intent, axis-labeled confidence, citations, gaps, freshness summary, resolution with tiers |
| `query/recall.rs`, `query/fusion.rs`, `query/hybrid.rs` | Fragment-index recall, RRF fusion, two-route hybrid assembly with trace provenance |
| `query/plan.rs` | Search-plan validation (closed-enum modes, filters) |
| `query/search_backend.rs`, `query/fts_backend.rs`, `query/vector_backend.rs`, `query/lemma_backend.rs`, `query/rg_backend.rs` | Intent-dispatched search rungs; index-backed rungs decline without storage so legacy doc backends serve |
| `query/identifier.rs`, `query/glob.rs` | Identifier patterns (reuses the shared string helpers); rg-style path matching |
| `query/llm_filter.rs` | Relevance scoring seam; `NoopLlmFilter` (substring hit 0.9, else 0.1) is the hermetic default |
| `query/rerank.rs` | LLM reranker output parsing (seam over the shared parse helper) |
| `query/synthesize.rs`, `query/formatter.rs` | Deterministic stage assembly; markdown/JSON/compact/debug rendering |
| `query/snapshot.rs` | Frozen file-reading snapshot type |
| `query/ingest.rs`, `query/writer.rs`, `query/db_storage.rs` | Fragment ingestion and the storage adapters: `FragmentWriter::write_items` is the single item-chunk commit (`VectorPolicy::Required` batch / `Optional` live, with `write_live` kept as the thin live shim); entity, lemma (integer dictionary keys via `lemma_terms`), symbol rows with interface/content hashes; writer schema version |
| `query/context_pack.rs` | Token-budgeted task packs with manifests, impact reports (dependents, owners, gates, tiers, tests); `tests_touching` token heuristic is the labelled fallback for callers without a hydrated graph |
| `query/decision_points.rs` | Seven pinned decision specs (`intent` v2, `sufficiency`/`excerpt`/`staleness`/`summary_index`/`scope`/`rerank` v1), fail-closed authorization plus registry-taking `authorize_point_with*` companions, route/filter/eligibility helpers, decision metrics |
| `query/shadow.rs` | Non-acting observation records (shadow agreement logging) |
| `query/calibration.rs` | Decision-point corpora; citation heuristic calibration |
| `summary.rs` | Comment summaries: fenced prompts (comment plus capped node-code snippet as inert data), strict validation, budget caps with truncation gaps, background `SummaryPass` on the background lane, sidecar plus `fragment_summaries` projection with comment-and-code-hash freshness |
| `query/concept_ground.rs` | Taxonomy groundedness as rerank-only evidence over `fluent-concept` stores |
| `grounding.rs` | Grounded-or-silent gate, hash-verified range citations with freshness verdicts, output citation checks in both shapes, requeue lists, tiered live re-verification (`verify_citations_live`) |
| `graph_index.rs` | Import/call graph with resolution tiers on every edge, closure expansion, dependent-aware invalidation, `tests_touching(item/files)` over recorded refs plus structural specifier-segment matching |

### Maintain plane

| Module | Responsibility |
|---|---|
| `pipeline.rs` | Fixed per-file stage order (fingerprint, index-lite, format, lint-plus-check, fix ladder, verify, index-finalize, completion) plus wave ordering, the early syntactic index, and witness rules; composes stages, never reimplements them |
| `waves.rs` | Wave loop: batch format, one clippy per package, ladder repeat to fixpoint, attempt ladder with fingerprints, strict breaker, witness wave, systemic-cause feed, plus the model-draft rung (up to three local code-model drafts per escalated file, verified before they stand) |
| `fix.rs` | D1 rustfmt on scratch with test-preserving splice-back and prose-conflict findings; D2 suggestion applier; D3 import/`mut` recipes; verified-apply with live-hash precondition |
| `gates.rs` | Model-free certification gates with stable IDs, prose-aware P2 counting (prose/test spans subtracted before token counts), plus the shared spliced-format primitive |
| `clippy.rs` | Per-package clippy driver (JSON parse, file attribution, package discovery, resource shaping, argv builder) |
| `shadow.rs` | Scratch-tree merged verification and bisection with an explicit interaction verdict |
| `index_pipeline.rs` | Fragment index pass: scan → diff → prepare → embed → commit, per-file isolation, bounded waves, sig-gated follow-up loop, clean-reprocess counter |
| `index_generation.rs` | Generation guard: header-plus-schema value, refuse-on-mismatch reads |
| `invalidation.rs` | Pure invalidation decisions: interface diffs, edge dependents, macro widening |
| `item_hashes.rs` | Normalized sig/body/doc/item hashes, interface stability, per-item work keys |
| `sync_engine.rs` | Member-JSON pipeline: parse → save with receipt → comment sync → completion stamp → optional DB sync |
| `sync/json_store.rs`, `sync/json_writer.rs` | Sidecar load/save with merge preservation; manual JSON shapes |
| `sync/comments.rs` | Certified prose write-back that never touches human text |
| `sync/staleness.rs` | I/O shells over the freshness core |
| `freshness.rs` | Single freshness home: skew tables, granularity probe, completion touch, backdating, rename adoption, verify audit |
| `diff.rs` | Five-bucket file diff with hash-tiebreak freshness authority |
| `selection.rs` | File selection over the shared walker with kind/size caps |
| `offload.rs` | Deterministic staged-file applier: the no-model tier with mechanical-class checks; enriched escalation packets (`AttemptRecord`, `normalize()` for legacy journals); test-path predicate shared by the graph |
| `component_map.rs` | Workspace file to narrowest-gate map |
| `coordinator.rs`, `scheduler.rs`, `watcher.rs` | Watch batches to ordered revisions through the coalescing driver (bespoke by design; never re-platformed onto pools) |
| `change_set.rs` | Watch-event accumulation with compaction budget |
| `runtime.rs` | Shared AST/DB pools plus thread-local resources |
| `extractor/` (`mod`, `adapter`, `code`, `markdown`, `text`, `vector_content`) | Registry-dispatched fragment extraction with chunking parity; shared text-shape core in `mod` (`byte_offset_for_units`, `overlap_lines_back[_inclusive]` backward counters, `split_long_units` window splitter) composing `common_core::string::collapse_whitespace` |
| `ast_parser.rs` | Tree-sitter grammars plus the single syntax home: `CommentBlock` collection in one parse (top-of-file plus documentable nodes, attributes, nested items), top items, test spans, prose units, pub headers, error counts, cfg-gated items, span/hash stamping; doc readers delegate to the one cleaner |

### Control plane

| Module | Responsibility |
|---|---|
| `maintenance.rs` | Persisted work queue (`CheckpointedStepGraph` states) with fixpoint driver, attempt ladder (`tier_for`), breaker signals, `ActOutcome::{Verified, Retryable, Terminal, Unavailable}`, queue-owned crash snapshots, run reports with efficiency and `model: unavailable` sections, ledger save/load |
| `orchestrator.rs` | Narrow inbound contract: validated posts, plan import, escalation ledger (file/claim/resolve), ledger tail, pack requests; never touches running scheduler state |
| `plans.rs` | Durable work plans: human-authored intent the loop tracks and verifies but never executes; merge-by-ID preserves completed work |
| `policy.rs` | Versioned policy plus per-section fingerprints for staleness |
| `components.rs` | Named input-component vectors plus the header-only backdate sweep |
| `triage.rs` | Static-first, grammar-direct triage and routing: closed verdicts, `ModelPolicy`-manifested context packets, budgets, suppression, hold-for-review, suspicion guard, calibration measurement over gold corpora plus harvested journal negatives |
| `journal.rs` | Write-ahead intents with hash reconcile, content-id journal entries, `RevertRecord` negatives on every revert, review groups, three-way undo |
| `model_lanes.rs` | Model-call priority lanes (interactive first, background defers); fallible `admit_checked` preserves ladder `Unavailable` intact instead of converting it to a timeout |
| `fidelity.rs` | Cfg matrix runner with per-file verification records, cfg/detached findings, conservative deletion verdicts, blind-spot gaps |
| `seal.rs` | Workspace seal: matrix clippy, full build, full tests, witness pass, contract exit codes, budget targets |
| `scorecard.rs` | Hermetic improvement instrument: fast tier with ratchet baselines that block release on regression (nav, graph, freshness, speed, decision, lanes, loop, avoidance) |
| `config.rs` | Project configuration load and precedence (providers, embed config, model roles, lane budgets) |
| `search_types.rs`, `search_constants.rs` | Tested index/search contracts, budgets, and value types |
| `eval.rs`, `jumps.rs` | Eval corpus loader (corpus lives in `doc/guidance/eval/`, outside every indexed root, never a retrieval input); doc→code jump records (crate attribution plus the funnel vocabulary, one scanner/table shared with capability ingest). Model-output parsing lives in `fluent-llm` (`parse::extract_first_json_span`/`extract_fence`, `decision::match_text_label`); guidance's `model_text.rs` shim was deleted after migration |
| `runtime.rs` | Shared AST/DB pools: per-call `ResultPool`s (`ast_pool`, index pools) over `fluent-concurrency` with backpressure; thread-local parser resources |
| `source_ref.rs`, `source_scope.rs` | Covered under the read plane above; top-level so selection, read, and graph share them without depending on `read::engine` |

The crate root `lib.rs` only declares this module tree. Behavior
tests live in `src/guidance/tests/<module>.rs` with a `#[path]`
forwarder in the owning module — never an inline test module
(the `read` tree keeps one local exception: `read/mod.rs` hosts
the `read_perf` forwarder). Checked-in fixtures the tests run
from: `corpora/` (intent, triage, route-calibration, staleness,
sufficiency, support, excerpt, lemma-ablation, summary,
nav, answer-floor, span-cap, doc-admission seed sets plus
`benchmark_tasks.jsonl`), `fixture_graph/` (caller/callee parse
fixtures with `ground_truth.json`), `fixture_read/` (minimal
workspace with `doc/`, `src/`, `README.md`), `doc/guidance/eval/`
(held-out NL cases, never a retrieval input), and
`scorecard_baseline.json` (ratchet pins, updated only via
`guidance scorecard --update-baseline --reason`). The binary's
own suites live in `src/bin/guidance/tests/` (CLI e2e, goldens,
`--rg` parity, freshen, strict-verify, prune-surface).

## CLI surface (`guidance` binary, `src/bin/guidance/`)

Thin shells over `guidance-core`: each command module owns flag
parsing and rendering only. `search.rs` builds a `SearchPlan` and
runs `run_recall` over `GuidanceDbStorage` (`--rg` runs the
standalone L0 `RgBackend`, never fused); `index_cmd.rs` is path
scoping over `SyncEngine::gen_scoped`; `freshen.rs` converges the
files behind an answer then re-runs once (sidecars plus index,
never fix waves); `embed.rs` resolves the `models.embed` backend
offline (unresolved degrades through embedder-absent paths);
`benchmark.rs` compares shipping `ReadEngine` vs targeted grep vs
wholesale load per query (deterministic, model-free);
`summarize_cmd.rs` runs the opt-in background comment pass;
`structure.rs` prints the mirror tree; `mcp.rs` serves JSON-RPC 2.0 over stdio.

Commands: `explain` (with `--no-llm`, `--freshen`, `--strict`,
`--require-answer`, `--trace`, `--format md|json`), `search`
(`fts`/`vector`/`rg`/`fuse`, `--glob`, `--prefer-symbol`), `sync`
(`--watch`; `run` is the alias), `index`, `status`, `check`,
`structure`, `commit` (removed: points at `check`/`status`), `clean`, `init`, `mcp`, `component-gate`,
`scorecard` (fast tier; `--deep`, `--live` prints model-gated
commands without running them), `seal`, `benchmark` (tasks file
required, checked-in battery at
`src/guidance/corpora/benchmark_tasks.jsonl`), `summarize`,
`impact`, `verify` (`--deep` prints hashes), `pack`, `calibrate`
(stores only on passing bars), `test` (harness self-check),
`kb` (machine-readable mirror summary), `queue-post`,
`escalation-file`/`claim`/`resolve`, `plan-import`,
`ledger-tail`, plus a global `--strict` flag that refuses
non-fresh. A bare `guidance <query>` searches.

## Decision layer

Two layers, different trust. The query points (`intent`,
`sufficiency`, `excerpt`, `staleness`, `summary_index`, `scope`,
`rerank`) run rule, cache, model, fallback through the shared
ladder and stay shadow-only without a passing calibration entry
in the loaded registry; a busy or unreachable backend surfaces
`Unavailable`, which the lanes preserve so background items wait
without consuming attempts and the report lists them under
`model: unavailable` (interactive keeps priority). Triage and
routing run grammar-direct: the static table decides first, the
model may only emit closed-enum verdicts and plans (`triage.rs`
owns `dp.triage_residual` and `dp.route_query` under the
`static-table` fingerprint), and every packet manifests its
decoding policy (`temperature`, `seed`) as evidence of what was
asked — honored by endpoint configuration, never enforced here. Unclassified work holds for review, and
precision/recall over golden sets gates auto-apply. Route
calibration is measurement-only (no verdict scorer yet). Both
layers record through the shared decision metrics; the deterministic
result stays authoritative until measurement says otherwise.

## Shared infrastructure (compose, never copy)

| Concern | Home | Used in guidance as |
|---|---|---|
| Hashing, file I/O, walking, paths, retry math, locking, histograms, citations | `common-core` (`src/common-core`) | Everywhere; the zero-domain helper crate |
| Control plane (`Component`, `WorkUnit`, ladder, capabilities, pipeline, wrappers) | `fluent-wvr` (`src/fluent-wvr`) | Sync pipeline, wave units, step units, query dispatch |
| Owned concurrency (supervision batches, pools, abort, runtimes) | `fluent-concurrency` (`src/fluent-concurrency`) | Index waves, maintenance steps, pools, watcher/coordinator plumbing |
| Ordering and proof trails | `fluent-dag` (`src/dag`) | Dependency and checkpointed step graphs |
| Tokens, packing, embeddings, rerank parsing, decision ladder | `fluent-llm` (`src/llm`, `decision/` module) | Envelopes, packs, embed cache, ladder, registry, triage metrics |
| SQLite, HNSW, FTS | `fluent-db` (`src/db`), `search-vector` (`src/search-vector`) | Fragment/symbol/edge tables, recall |
| Embedded document-node store (change-gated JSON ingest via `sync_from_dir`, vector/keyword/HNSW search) | `search-vector` (`nodes` table, node APIs) | Shared generic primitive with its own goldens (`search-vector/tests/node_sync.rs`); guidance's live path reads the fragment index and does not depend on it |
| Shared types (`GuidanceDoc`, `Member`, sidecar header codec) | `fluent-types` (`src/types`) | Mirror schema read by core, index, and binary alike |
| Concept stores | `fluent-concept` (`src/concept`) | Taxonomy/concept resolution for rerank-only groundedness evidence (`query/concept_ground.rs`); term normalization with curie/lemma variants |
| Routing signals | `spacy-rs` (`src/spacy-rs`) | `RoutingSignal` consumed by `graph_index.rs` |
| Knowledge plumbing | `fluent-knowledge` (`src/knowledge`) | Query-engine dependency |
| Test scaffolding | `fluent-wvr-testutil` (`src/fluent-wvr-testutil`) | Temp dirs and shared harnesses in tests |

When a helper exists in a shared crate, new code composes it. A
second implementation is a defect, with two documented exceptions:
the bespoke coalescing scheduler (structurally incompatible with
pools) and the per-file wave driver (batching shape the pools do
not express). The import boundary runs one way: shared library
crates never import from `guidance` (or the other tool crates).

## Conventions for agents

- Tests live in `src/guidance/tests/<module>.rs` with a `#[path]`
  forwarder in the owning module — never an inline test module
  (one exception: the `read_perf` forwarder hosted in `read/mod.rs`).
  Write the test first where practical; smoke per milestone.
- No `unsafe`; mutex poisoning recovers through the shared lock
  helper instead of panicking; locks never cross `.await`.
- Comments explain why in at most two lines, with no roadmap or
  milestone references.
- Behavior changes default to byte-identical callers; when the
  shape must change, prefer additive companions (e.g.
  `authorize_point_with`, `build_packet_with_policy`) over
  signature edits.
- Gates: `make guidance-smoke` per milestone, `make guidance-lint`
  on type changes, `make guidance-test` plus `make
  guidance-scorecard` where the milestone names them. Repo-wide
  `make smoke` runs at workflow end; repo-wide `make test` runs on
  user command only.
- Live-inference tests stay `#[ignore]`d and run only via `make
  test-live`; hermetic suites never dial a real endpoint.

## Doc map

`VISION.md` (goals, guarantees, refusals, budgets — the only
place aspirations live), `ARCHITECTURE.md` (this file: the
implementation map), `OVERVIEW.md` (user-visible behavior reference; note its §3
command list has drifted — `health`, `telemetry`, `cache-stats`,
`todo`, `diary` name commands the binary does not implement —
so the CLI surface section above is authoritative for commands),
`CAPABILITY_SUMMARIES.md` (capability-card pipeline: external
corpus plus project cards, funnel, retrieval, gaps),
`CALIBRATION.md` (live commands plus promotion bars),
`MODEL_ROLES.md` (serving topology plus config example),
`MODEL_OFFLOAD.md` (automation guardrails),
`QUARANTINE_LEDGER.md` (quarantine record), `TODO_RUN.md`
(queued work), `AB_BENCHMARKS_P6.md` (benchmark notes),
`eval/` (held-out NL cases), `schemas/` (wire schemas).
