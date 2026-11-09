# guidance — Architecture

> Briefing on the design of `guidance` as it stands. Goals without
> mechanism live in `VISION.md`; this document describes the mechanism
> as built. Every type named below exists in the tree — the stale-name
> guard fails on invented names.

## What it is

`guidance` is a deterministic-first code navigation subagent. The
`guidance-core` crate (`src/guidance/`) owns the mirror and the
query pipeline; the `guidance` binary (`src/bin/guidance/`) owns
the CLI surface. It answers from a structured, incrementally
updated mirror of the codebase and escalates to models only through
calibrated decision points (judgment) and grounded synthesis —
never for anything deterministic code already answers exactly.

## Crate layout (`guidance-core`)

| Module | Responsibility |
|--------|---------------|
| `ast_parser` | Tree-sitter parsing across supported grammars; span resolution, snippet extraction |
| `config` | `ProjectConfig`: mirror dirs, providers, model-role blocks, per-point thresholds, registry location; project → user → default precedence |
| `query_engine` | Tiered-escalation dispatch over registered backends; fused recall; stage formatting |
| `query/strategy` | `FsmEngine` (intake → classify → route → validate) plus the exact pre-check single home |
| `query/decision_points` | Pinned decision specs, shadow-only authorization, pure route/filter/eligibility helpers, decision metrics |
| `query/shadow` | Non-acting observation records (intent, sufficiency, excerpt rank deltas) |
| `query/search_backend` + `fts/vector/lemma/rg_backend` | Intent-dispatched search rungs; index-backed rungs decline without storage |
| `query/recall`, `query/fusion`, `query/hybrid` | Fragment-index recall, RRF fusion, two-route hybrid assembly |
| `query/llm_filter` (+ `NoopLlmFilter` fail-open default) | Relevance scoring seam for the concept rung |
| `query/concept_ground` | Taxonomy groundedness as rerank-only evidence, never a filter |
| `query/synthesize`, `query/formatter` | Deterministic stage assembly; markdown/JSON/compact/debug rendering |
| `query/ingest`, `query/db_storage`, `query/diagnose` | Fragment ingestion, storage adapters, why-is-this-here diagnosis |
| `grounding` | Grounded-or-silent gate: excerpt sufficiency, citation verification, source-bound synthesis keys |
| `freshness` + `sync/staleness` | Single freshness home (revision matrix, mtime-skew table, hash tiebreak); I/O shells over it |
| `sync_engine` + `sync/json_store/json_writer/comments` | Member-JSON pipeline: parse → enhance → save → comment sync → optional DB sync |
| `index_pipeline` | Fragment index pass: scan → diff → prepare → embed → commit, per-file isolation, bounded waves |
| `selection`, `search_types`, `search_constants` | File selection and kind/size caps; tested index/search contracts and budgets |
| `diff` | Content hashing and change computation |
| `coordinator`, `scheduler`, `watcher`, `runtime` | Watch batches → ordered revisions → coalesced reconcile; AST/DB pools |
| `graph_index` | Import/call graph for closure expansion and dependent-aware invalidation |
| `enhancer` | Comment generation over verbatim excerpts (generative role) |
| `scanner`, `extractor`, `memory`, `plugin` | Pattern detection; code/markdown/text fragment extraction; memory bridge; subprocess plugins |

## CLI surface (`guidance` binary)

`explain`, `search` (with `fts`/`vector`/`rg`/`fuse` modes), `sync`
(`--watch` supported), `index`, `status`, `check` (fast
`--check-ready` probe plus the full gate), `benchmark`, `health`,
`structure`, `commit`, `clean`, `telemetry`, `cache-stats`, `init`,
`todo`, `diary`, `mcp`, plus a bare `guidance <query>` search.
`init` writes the default config including both model roles.

## Query pipeline

Intake tokenizes and runs exact detectors (identifier and file-path
patterns). Classification is the `FsmEngine`: deterministic
pre-checks (empty, file path, single identifier) decide immediately
and short-circuit; only ambiguous queries are model-eligible, and
the route table (word/anchor index, FTS, hybrid, vector, keyword) is
fixed. Dispatch walks registered backends in priority order through
the shared first-accept ladder: first hit wins, `NoResults` falls
through, hard errors short-circuit. Recall over the fragment index
fuses FTS, vector, and lemma routes; assembly is deterministic and
formatters render without re-ranking.

## Decision layer

Four pinned points, one spec each (all version 1): intent Choice,
sufficiency three-way Choice (answer / synthesize / unanswerable),
excerpt Score, commentary-staleness Noul. Every evaluation composes
rule → fingerprint-scoped cache → model → static fallback through
the shared `fluent_llm::decision` ladder; `Unavailable` falls
through, `Rejected` is terminal, degraded verdicts never populate
the cache. Authorization is fail-closed on (point, model
fingerprint, question version): without a passing calibration entry
every point runs shadow-only and the deterministic result stays
authoritative. Pure helpers (`route_sufficiency`,
`relevance_gate`, `staleness_eligible`) encode the guardrails —
grounded-or-silent, stable-order filtering, cosmetics never
nominated, human prose never nominated. Seed corpora live in
`src/guidance/corpora/`; the live canary probe and the promotion
procedure are documented alongside them.

## Sync pipeline and freshness

`sync` walks configured roots and regenerates only stale sidecars:
the mtime gate rules outside the one-second ambiguity window, the
content hash decides inside it, and only hash-changed members reach
a staleness verdict. Generated commentary is written back through
comment sync, which skips existing human prose and generated
comments alike when the source already documents itself. The watcher
feeds the coordinator, which orders revisions through the
coalescing scheduler; the fragment index commits per file with
retry-once isolation, and the DB sync serializes writes. `check`
runs the project test/lint/format commands then the
gen/structure/database gates. Per-file maintenance loops stay
stale-only, and model offload follows strict role separation
(deterministic tools certify; models triage and propose).

## Configuration

Two independently addressable model roles — decision (judgment) and
generative (synthesis) — each with its own endpoint, model, timeout,
and kill switch; per-decision-point thresholds; a calibration
registry location. A disabled or unreachable role degrades to the
deterministic path with a typed `Unavailable`, never a silent wrong
answer. Precedence is project config, then user config, then built-in
defaults; unknown keys are tolerated.

## Telemetry

Per-command latency histograms, benchmark accuracy/latency reports,
health output (comment coverage plus an additive decision-metrics
key: rule wins, fallbacks, drops, p50/p95, cache hits, shadow
disagreement), all over the shared histogram and counter
primitives — no second metrics system. Hermetic tests use the
scripted decision backend; live tests stay ignored and gated, with
skip-not-fail without endpoint env.

## Design contracts

- Deterministic-first ladder: exact → keyword → fusion → vector →
  model last. The model never jumps the queue.
- Grounded or silent; cached answers never override freshness;
  human prose preserved; indexed source is untrusted and no
  decision is the sole guard of a side effect.
- `WorkUnit::execute` stays synchronous and I/O-free; the sync
  bridge serves sync callers outside `execute`.
- Layer rules: consumer crates may import capability crates, never
  the reverse; shared crates never import `guidance`/`router`
  consumers; the binary layer stays thin.
- Tests follow the `tests/<module>.rs` plus `#[path]` forwarder
  convention; quarantines name their replacement and land with the
  replacement test.

## Doc map

`VISION.md` (goals), `OVERVIEW.md` (feature tour),
`MODEL_ROLES.md` (serving topology + config example),
`CALIBRATION.md` (live commands + promotion),
`MODEL_OFFLOAD.md` (automation guardrails),
`QUARANTINE_LEDGER.md` (quarantine record), plus the overhaul
roadmap and its tracking checklist at the repo root.
