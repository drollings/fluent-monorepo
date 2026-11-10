# guidance — Architecture

> Review and map for code assistants. Goals without mechanism live in
> `VISION.md`; this document describes the mechanism as built: which
> file owns what, how the planes compose, and what already exists so
> nothing gets reimplemented. Every module named below exists under
> `src/guidance/src/` (or the stated sibling crate) — the unit tests
> fail on invented names, so check before adding a row.

## What it is

`guidance-core` (`src/guidance/`, the consumer-layer crate) owns the
mirror, the query pipeline, and the maintenance loop. The `guidance`
binary (`src/bin/guidance/`) owns the CLI surface and stays thin:
it parses argv, builds engines, prints reports, and maps exit codes.

## The three planes in code

| Plane | Entry points | Owns |
|---|---|---|
| Read | `query_engine.rs` (`answer`, `explain`), `query/*` | Retrieval tiers, staged routing, envelopes, citations, packs, impact |
| Maintain | `waves.rs`, `fix.rs`, `index_pipeline.rs`, `sync_engine.rs` | Waves, fix rungs, gates, fragment/member indexes |
| Control | `maintenance.rs`, `policy.rs`, `triage.rs`, `journal.rs`, `scorecard.rs` | Queue, tiers, calibration, ledger, review, ratchets |

## Store of record

| Store | Role | Code |
|---|---|---|
| Source tree | Truth; everything derives from it | — |
| Sidecars (`<guidance>/src/**/*.json` with a header first line) | Per-file derived facts and receipts | `sync/json_store.rs`, `sync/json_writer.rs`, header codec in `fluent-types/src/sidecar.rs` |
| Derived index (SQLite fragments/symbols/edges plus FTS) | Disposable; rebuilt from sidecars, refused on marker mismatch | `search-vector/src/db.rs`, writers in `query/ingest.rs`, `query/db_storage.rs`, `index_pipeline.rs` |
| Control state (queue, calibration, intent records, journal, escalations, reports) | Durable and not derivable; back it up | `maintenance.rs`, `journal.rs` |

Mtime is a cache over this layout; content hashes decide. The
fragment index never reprocesses clean files, and the member-JSON
path never rewrites identical bytes without a completion stamp.

## Module map (`guidance-core`)

### Read plane

| Module | Responsibility |
|---|---|
| `query_engine.rs` | Tiered-escalation dispatch over registered backends; `answer` verifies citations through the in-hand doc with an optional workspace root |
| `query/strategy.rs` | Intent FSM plus the exact pre-check single home |
| `query/envelope.rs` | Unified answer envelope: intent, axis-labeled confidence, citations, gaps, freshness summary, resolution with tiers |
| `query/recall.rs`, `query/fusion.rs`, `query/hybrid.rs` | Fragment-index recall, RRF fusion, two-route hybrid assembly with trace provenance |
| `query/plan.rs` | Search-plan validation (closed-enum modes, filters) |
| `query/search_backend.rs`, `query/fts_backend.rs`, `query/vector_backend.rs`, `query/lemma_backend.rs`, `query/rg_backend.rs` | Intent-dispatched search rungs; index-backed rungs decline without storage so legacy doc backends serve |
| `query/identifier.rs`, `query/glob.rs` | Identifier patterns (reuses the shared string helpers); rg-style path matching |
| `query/llm_filter.rs` | Relevance scoring seam (synchronous; async scoring composes the shared queue when it arrives) |
| `query/rerank.rs` | LLM reranker output parsing (seam over the shared parse helper) |
| `query/synthesize.rs`, `query/formatter.rs` | Deterministic stage assembly; markdown/JSON/compact/debug rendering |
| `query/snapshot.rs` | Frozen file-reading snapshot type |
| `query/ingest.rs`, `query/writer.rs`, `query/db_storage.rs` | Fragment ingestion and the storage adapters (entity, lemma, symbol rows with interface/content hashes) |
| `query/context_pack.rs` | Token-budgeted task packs with manifests, impact reports (dependents, owners, gates, tiers, tests) |
| `query/decision_points.rs` | Pinned decision specs, authorization, route/filter/eligibility helpers, decision metrics |
| `query/shadow.rs` | Non-acting observation records |
| `query/calibration.rs` | Decision-point corpora; citation heuristic calibration |
| `query/concept_ground.rs` | Taxonomy groundedness as rerank-only evidence |
| `grounding.rs` | Grounded-or-silent gate, hash-verified range citations with freshness verdicts, output citation checks in both shapes, requeue lists |
| `graph_index.rs` | Import/call graph with resolution tiers on every edge, closure expansion, dependent-aware invalidation |

### Maintain plane

| Module | Responsibility |
|---|---|
| `waves.rs` | Wave loop: batch format, one clippy per package, ladder repeat to fixpoint, attempt ladder with fingerprints, strict breaker, witness wave |
| `fix.rs` | D1 rustfmt on scratch with test-preserving splice-back and prose-conflict findings; D2 suggestion applier; D3 import/`mut` recipes; verified-apply with live-hash precondition |
| `gates.rs` | Model-free certification gates with stable IDs plus the shared spliced-format primitive |
| `clippy.rs` | Per-package clippy driver (JSON parse, file attribution, package discovery, resource shaping, argv builder) |
| `shadow.rs` | Scratch-tree merged verification and bisection with an explicit interaction verdict |
| `index_pipeline.rs` | Fragment index pass: scan → diff → prepare → embed → commit, per-file isolation, bounded waves, sig-gated follow-up loop, clean-reprocess counter |
| `invalidation.rs` | Pure invalidation decisions: interface diffs, edge dependents, macro widening |
| `item_hashes.rs` | Normalized sig/body/doc/item hashes, interface stability, per-item work keys |
| `sync_engine.rs` | Member-JSON pipeline: parse → save with receipt → comment sync → completion stamp → optional DB sync |
| `sync/json_store.rs`, `sync/json_writer.rs` | Sidecar load/save with merge preservation; manual JSON shapes |
| `sync/comments.rs` | Certified prose write-back that never touches human text |
| `sync/staleness.rs` | I/O shells over the freshness core |
| `freshness.rs` | Single freshness home: skew tables, granularity probe, completion touch, backdating, rename adoption, verify audit |
| `diff.rs` | Five-bucket file diff with hash-tiebreak freshness authority |
| `selection.rs` | File selection over the shared walker with kind/size caps |
| `offload.rs` | Deterministic staged-file applier: the no-model tier with mechanical-class checks |
| `component_map.rs` | Workspace file to narrowest-gate map |
| `coordinator.rs`, `scheduler.rs`, `watcher.rs` | Watch batches to ordered revisions through the coalescing driver (bespoke by design; never re-platformed onto pools) |
| `change_set.rs` | Watch-event accumulation with compaction budget |
| `runtime.rs` | Shared AST/DB pools plus thread-local resources |
| `extractor/` (`mod`, `adapter`, `code`, `markdown`, `text`, `vector_content`) | Registry-dispatched fragment extraction with chunking parity |
| `ast_parser.rs` | Tree-sitter grammars plus the single syntax home: top items, test spans, prose units, pub headers, error counts, cfg-gated items, span/hash stamping |

### Control plane

| Module | Responsibility |
|---|---|
| `maintenance.rs` | Persisted work queue with fixpoint driver, attempt ladder, breaker signals, ledger save/load |
| `policy.rs` | Versioned policy plus per-section fingerprints for staleness |
| `components.rs` | Named input-component vectors plus the header-only backdate sweep |
| `triage.rs` | Static-first, grammar-direct triage and routing: closed verdicts, hashed context packets, budgets, suppression, hold-for-review, calibration measurement |
| `journal.rs` | Write-ahead intents with hash reconcile, content-id journal entries, review groups, three-way undo |
| `model_lanes.rs` | Model-call priority lanes (interactive first, background defers) |
| `fidelity.rs` | Cfg matrix runner with per-file verification records, cfg/detached findings, conservative deletion verdicts, blind-spot gaps |
| `seal.rs` | Workspace seal: matrix clippy, full build, full tests, witness pass, contract exit codes, budget targets |
| `scorecard.rs` | Hermetic improvement instrument: fast tier with ratchet baselines that block release on regression |
| `config.rs` | Project configuration load and precedence |
| `search_types.rs`, `search_constants.rs` | Tested index/search contracts, budgets, and value types |
| `coordinator.rs` | Revision/step proof trail for reconciliations (also read-plane adjacent) |

## CLI surface (`guidance` binary)

`explain`, `search` (`fts`/`vector`/`rg`/`fuse` modes), `sync`
(`--watch` supported), `index`, `status`, `check`,
`structure`, `commit` (removed: points at `check`/`status`), `clean`,
`init`, `mcp`, `component-gate`,
`scorecard`, plus the two review-plane commands `seal` (workspace
gate with contract exit codes) and `impact` (dependents, owners,
gates, tiers, tests). A bare `guidance <query>` searches.

## Decision layer

Two layers, different trust. The query points (`intent`,
`sufficiency`, `excerpt`, `staleness`) run rule, cache, model,
fallback through the shared ladder and stay shadow-only without a
passing calibration entry. Triage and routing run grammar-direct:
the static table decides first, the model may only emit closed-enum
verdicts and plans, unclassified work holds for review, and
precision/recall over golden sets gates auto-apply. Both layers
record through the shared decision metrics; the deterministic result
stays authoritative until measurement says otherwise.

## Shared infrastructure (compose, never copy)

| Concern | Home | Used in guidance as |
|---|---|---|
| Hashing, file I/O, walking, paths, retry math, locking, histograms, citations | `common-core` | Everywhere; the only permitted zero-domain crate |
| Control plane (`Component`, `WorkUnit`, ladder, capabilities, pipeline, wrappers) | `fluent-wvr` | Sync pipeline, wave units, step units, query dispatch |
| Owned concurrency (supervision batches, pools, abort, runtimes) | `fluent-concurrency` | Index waves, maintenance steps, pools, watcher/coordinator plumbing |
| Ordering and proof trails | `fluent-dag` | Dependency and checkpointed step graphs |
| Tokens, packing, embeddings, rerank parsing, decision ladder | `fluent-llm` | Envelopes, packs, embed cache, triage metrics |
| SQLite, HNSW, FTS | `fluent-db`, `search-vector` | Fragment/symbol/edge tables, recall |
| Shared types (`GuidanceDoc`, `Member`, sidecar header codec) | `fluent-types` | Mirror schema read by core, index, and binary alike |

When a helper exists in a shared crate, new code composes it. A
second implementation is a defect, with two documented exceptions:
the bespoke coalescing scheduler (structurally incompatible with
pools) and the per-file wave driver (batching shape the pools do
not express).

## Conventions for agents

- Tests live in `src/guidance/tests/<module>.rs` with a `#[path]`
  forwarder in the owning module — never an inline test module.
- No `unsafe`; mutex poisoning recovers through the shared lock
  helper instead of panicking; locks never cross `.await`.
- Comments explain why in at most two lines, with no roadmap or
  milestone references.
- Behavior changes list a `Compat` reason in the roadmap; default
  is byte-identical callers.
- Gates: `make guidance-smoke` per milestone, `make guidance-lint`
  on type changes, `make guidance-test` plus `make
  guidance-scorecard` where the milestone names them. Repo-wide
  `make smoke` runs at workflow end; repo-wide `make test` runs on
  user command only.
- Live-inference tests stay `#[ignore]`d and run only via `make
  test-live`; hermetic suites never dial a real endpoint.

## Doc map

`VISION.md` (goals), `OVERVIEW.md` (feature tour),
`MODEL_ROLES.md` (serving topology + config example),
`CALIBRATION.md` (live commands + promotion),
`MODEL_OFFLOAD.md` (automation guardrails),
`QUARANTINE_LEDGER.md` (quarantine record), plus the implemented
roadmap and its tracking checklist at the repo root.
