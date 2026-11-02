# guidance — Vision

**A deterministic-first code navigation subagent: local answers in
milliseconds, model synthesis only for genuine novelty.**

## Mission

Frontier models are expensive oracles for questions a local index
can answer exactly: "where is X defined?", "what calls Y?", "how
does this codebase handle Z?" guidance keeps a structured,
incrementally updated mirror of the codebase and answers from it —
keyword and semantic search over real excerpts with line citations —
escalating to local model synthesis only when the question needs
it, and caching that synthesis for next time.

## Goals

- **Deterministic first.** Exact identifier lookup, then keyword,
  then hybrid fusion, then vectors — each strictly more expensive
  than the last, the model last of all.
- **Always current, never rebuilt.** Every phase detects staleness
  incrementally; only changed files are reprocessed, and a global
  check gate (test → lint → format → sync → structure → database)
  keeps the mirror trustworthy before any commit.
- **Grounded or silent.** Synthesis receives verbatim source
  excerpts, never bare paths; no excerpts, no synthesis.
- **Token-efficient by construction.** Token-budgeted answer
  assembly and structured (JSON) output for orchestrating agents.

## End-to-end capabilities

- **Incremental codebase mirror.** Parse sources into structured
  metadata (signatures, line numbers, content hashes), preserve
  human/LLM commentary across re-syncs, and persist everything in a
  queryable local database.
- **Staged question answering.** Route each query by intent
  (identifier, file path, capability, how-to, conceptual) to the
  cheapest search primitive that can satisfy it; synthesize with a
  local model only above a novelty threshold.
- **Pre-commit integrity loop.** One command runs the full
  test/lint/format/sync/structure/database cycle, processing only
  stale files.
- **Cross-language parity.** All supported languages produce the
  same metadata schema from their grammars.
- **Subagent output.** Machine-readable answers (intent,
  confidence, results, citations, gaps) for frontier
  orchestrators, plus human-readable markdown.
- **Health and telemetry.** Comment coverage, staleness counts, node
  statistics, and query benchmarks.

## Components (sketch only)

A grammar-based parser producing content-hashed member records; an
incremental sync loop keyed on validated timestamps; a query
pipeline (intake → classify → route → validate → assemble →
synthesize); a ranked search ladder (exact index → keyword → fusion
→ vector); a config-driven check orchestrator. Schemas, state
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
