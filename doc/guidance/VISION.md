# guidance — Vision

**A deterministic-first code navigation subagent: exact answers from
a local mirror, model judgment only where judgment belongs, model
synthesis only for genuine novelty.**

## Mission

Frontier models are expensive oracles for questions a local index
can answer exactly: "where is X defined?", "what calls Y?", "how
does this codebase handle Z?" guidance keeps a structured,
incrementally updated mirror of the codebase and answers from it —
keyword and semantic search over real excerpts with line citations.
Where a bounded semantic judgment is the real task, a small decision
model judges — but only inside calibrated guardrails, with the
deterministic answer staying authoritative until calibration says
otherwise. Generative models synthesize over verbatim excerpts only,
never from bare paths or memory.

## Goals

- **Deterministic first.** Exact identifier lookup, then keyword,
  then hybrid fusion, then vectors — each strictly more expensive
  than the last, the model last of all. Nothing exact is ever
  re-decided by a model.
- **Calibrated judgment or silence.** Every semantic verdict runs
  rule → cache → model → static fallback, fails closed to the
  deterministic path, and stays shadow-only until a calibration
  entry authorizes that point, model, and question version. An
  uncalibrated model measures; it never acts.
- **Always current, never rebuilt.** Every phase detects staleness
  incrementally; only changed files are reprocessed, and a global
  check gate (test → lint → format → sync → structure → database)
  keeps the mirror trustworthy before any commit.
- **Grounded or silent.** Synthesis receives verbatim source
  excerpts, never bare paths; no excerpts, no synthesis. Changed
  source always wins over a stored summary.
- **Human prose is sacred.** Human-written commentary survives
  re-syncs untouched; machines may nominate refreshes, never impose
  them — and never over cosmetic diffs.
- **Stale-only work.** Every loop processes only stale files; clean
  files are never reprocessed. Automation proposes, deterministic
  gates dispose: models triage and draft, tests and lints certify,
  and nothing commits by itself.
- **Token-efficient by construction.** Token-budgeted answer
  assembly and structured (JSON) output for orchestrating agents.

## End-to-end capabilities

- **Incremental codebase mirror.** Parse sources into structured
  metadata (signatures, line numbers, content hashes), preserve
  human commentary across re-syncs, and persist everything in a
  queryable local database.
- **Staged question answering.** Route each query by intent
  (identifier, file path, capability, how-to, conceptual) to the
  cheapest search primitive that can satisfy it; consult a decision
  model only for genuinely ambiguous routing, filtering, and
  freshness judgments — each measured before it is trusted.
- **Pre-commit integrity loop.** One command runs the full
  test/lint/format/sync/structure/database cycle, processing only
  stale files.
- **Cross-language parity.** All supported languages produce the
  same metadata schema from their grammars.
- **Subagent output.** Machine-readable answers (intent,
  confidence, results, citations, gaps) for frontier
  orchestrators, plus human-readable markdown.
- **Health and telemetry.** Comment coverage, staleness counts, node
  statistics, query benchmarks, and decision-layer metrics (calls
  avoided, fallback rate, cache hits, shadow disagreement).

## Components (sketch only)

A grammar-based parser producing content-hashed member records; an
incremental sync loop keyed on validated timestamps; a query
pipeline (intake → classify → route → validate → assemble →
synthesize); a ranked search ladder (exact index → keyword → fusion
→ vector); calibrated decision points composed through the shared
ladder; a config-driven check orchestrator. Schemas, state
machines, CLI flags, and thresholds live in `ARCHITECTURE.md` /
`OVERVIEW.md` and the code — this document states what each stage
guarantees, not how it is implemented.

## Non-goals

- Not a model and not an IDE; it is the fast local memory an agent
  consults before spending inference.
- Not a rewrite of history: generated commentary lives in source or
  in the database, and staleness markers are validated, never
  assumed.
- Cached answers never override freshness: changed source always
  wins over a stored summary.
- No side effect on a verdict alone: indexed source text is
  untrusted, and no decision guards a write, patch, or commit by
  itself.
- Automation never rewrites tests to make them pass, and never
  commits.
