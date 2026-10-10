# guidance — Reverse-Engineering Reference (user-visible behavior)

Date: 2026-09-09
Scope: first-stage reverse engineering — what `guidance` does and its overall approach, generalized from observed behavior. No implementation detail.

## 1. What guidance is

`guidance` is a command-line code-navigation and repository-maintenance orchestrator. Tagline (from `--help`): "AST-guided vector search & edge AI orchestrator".

Its overall approach is **deterministic-first, cache-friendly code understanding**:

1. Parse source files into structural metadata (per-file JSON mirrors).
2. Keep those mirrors incrementally synchronized with sources (staleness by file timestamps).
3. Index the mirrors into a local SQLite-backed search database (keyword plus vector hybrid search).
4. Answer natural-language / identifier / file-path queries from the local index first; consult a local LLM only for synthesis, filtering, scoring, or generation tasks.
5. Wrap the everyday developer loop (test, lint, format, sync, structure summary, DB refresh, commit message drafting, notes) in small subcommands so repeated work is reproducible.

It is offline-capable for its core paths: indexing, status, structure summary, health, keyword search fallback, and DB statistics work with no network. LLM-dependent refinements (comment enhancement, commit message drafting, benchmark scoring, explain synthesis) degrade to a deterministic fallback or a placeholder when no model endpoint is reachable.

Supported source languages observed: Zig (`.zig`, `.zon`), Python (`.py`), Rust (`.rs`). Markdown (`.md`) appears in vision material as plugin-handled; the live parser accepts the three code families above and reports "unsupported language" otherwise. Parse failures are non-fatal warnings: the offending file is skipped, the rest of the run continues.

## 2. System view (external behavior)

### 2.1 Inputs

- A workspace directory containing source files.
- An optional project config: `.guidance/guidance-config.json`, with a per-user fallback under the OS config directory. Absent config means defaults.
- A local search database file (default `.guidance.db`).
- An optional local LLM endpoint described by the config (`providers` + `models` roles: default, fast, thinking, batch, embed).

### 2.2 Outputs and artifacts

- `.guidance/src/<relative-source-path>.json` — one structural mirror per source file. These are derived artifacts, not the source of truth; doc comments live in source files.
- `.guidance.db` — local search database holding indexed nodes, embeddings, and caches.
- `STRUCTURE.md` (workspace root) — generated human/assistant-friendly map of the repository.
- `.guidance/guidance-config.json` — created by `init` when missing.
- `.guidance/doc/TODO.md` (read by `todo`), `.guidance/doc/DIARY.md` (appended by `diary`).
- Console output: tables, status summaries, reports, and errors. Exit code 0 on success, 1 on stage failure.
- Optional global flags on every invocation: `--debug` (verbose tracing plus a command-latency summary) and `--show-prompts` (surface prompts sent to the model).

### 2.3 The closed maintenance loop

The product's central loop, visible to users:

```
source files → parse → per-file JSON mirrors → DB index → query/answer
     ↑                  (only stale files reprocessed)        ↓
     └────────────── comments written back to source ─────────┘
```

Key user-visible rules:

- Indexing is incremental: a file is reprocessed only when its JSON mirror is missing or older than the source (with a small timestamp tolerance). `--force` reprocesses everything.
- A file's JSON mirror timestamp advancing is the marker that all per-file stages succeeded for that file.
- Content-change tracking uses a hash over each member's signature (plus comment), so unchanged members preserve prior annotations while changed members are refreshed.
- DB synchronization walks the JSON mirror tree and upserts changed nodes; it can be skipped (`--no-db`) or previewed (`--dry-run`).

## 3. CLI surface

`guidance [OPTIONS] <COMMAND>`. Top-level help lists 16 subcommands. Each is described below as a user would experience it.

### 3.1 `guidance init [DIR]`

- Purpose: bootstrap a workspace for guidance use.
- Effect: creates `<DIR>/.guidance/` and, when absent, writes a default `guidance-config.json` pointing at `.guidance`, `.guidance.db`, `doc/skills`, `doc/capabilities`, `src`, local provider endpoints, model-role defaults, and embedding settings.
- Idempotent: never overwrites an existing config.
- Output: confirmation naming the initialized directory.

### 3.2 `guidance sync`

- Purpose: the main indexing command — parse sources to JSON mirrors and refresh the search DB.
- Selection modes:
  - `--file <path>`: process one file (or report staleness for a directory).
  - `--scan <dir>`: process one subtree.
  - Default: process every source directory from config (`src_dirs`, else the workspace root).
- Behavior:
  - Discovers files by supported extension, skips up-to-date files unless `--force`.
  - Reports totals: files discovered, stale count, files generated.
  - Then, unless `--no-db` or `--dry-run`, syncs the JSON tree into the DB and reports nodes synced.
  - `--dry-run` prints what would happen without writing.
  - `--verbose` lists per-file skip/generate lines and warns per failed file without aborting the run.
  - `--watch` stays resident after the initial sync, watching configured source trees and regenerating mirrors on change (debounce configurable via `--watch-debounce-ms`, default 500 ms). Only supported extensions trigger regeneration.
- Parallelism is user-visible only as speed: many files are processed concurrently; DB writes are serialized to avoid contention. Per-file failures surface as warnings, not aborts.

### 3.3 `guidance status`

- Purpose: staleness report without modifying anything.
- Output: total files discovered, count stale, count up to date, and a clean flag (true when zero stale).
- Respects the same `src_dirs` resolution as `sync`.

### 3.4 `guidance clean`

- Purpose: remove derived artifacts.
- Effect: deletes the DB file when present and all generated JSON files under the mirror tree. Never touches sources.
- Output: what was removed plus completion confirmation.

### 3.5 `guidance explain <QUERY>`

- Purpose: answer "where / what is this code?" queries from the local index.
- Inputs: query text; `--guidance` mirror dir (default `.guidance`); `--db` path (default `.guidance.db`); `--workspace`; `--limit` (default 10 results); `--no-llm`; `--filter` (default `auto`).
- Behavior:
  - Prefers the DB hybrid search when the DB exists; falls back to scanning JSON mirrors with case-insensitive exact / name-substring / token matching (scored 1.0 / 0.8 / 0.5) when the DB is absent or empty; truncates to `--limit`.
  - Prints a Markdown table (`Name | Source | Score`) under an `## Explain: <query>` heading, or `No results found.`
  - Optionally injects memory-plugin context under a `### Memory Context` heading when a memory bridge is available, and records the turn afterward.
  - `--no-llm` guarantees a structural-only answer with no model call.
- Query model (user-visible): the engine classifies queries into intents — single identifier, identifier lookup, file path, capability phrase, how-to question, conceptual description, multi-keyword, general search — tries the matching search strategy first, then falls back through the remaining strategies in priority order until one yields results. Hard errors stop the walk; empty results continue to the next strategy. An empty query currently matches broadly rather than erroring.

### 3.6 `guidance check`

- Purpose: pre-commit / CI gate. Runs a deterministic waterfall and stops at the first failure.
- Stages (user-visible names): `test`, `lint`, `fmt`, `gen` (staleness check), `structure` (STRUCTURE.md presence), `db` (DB presence).
- The test/lint/fmt stages run only the configured commands whose language is actually present in the workspace; each prints `OK` or `FAILED` with cause. Final line is `All checks passed` or a nonzero exit.
- Note: the `gen` stage reports stale-file counts rather than regenerating; run `sync` to fix.

### 3.7 `guidance test`

- Purpose: shorthand for running the workspace test suite.
- Behavior: prints `Running tests...`, runs the configured test command, exits 1 on failure.

### 3.8 `guidance structure`

- Purpose: regenerate the repository map.
- Effect: reads the JSON mirrors and writes `STRUCTURE.md` in the current directory.
- Output: confirmation with line count, or an error line. The generated document contains a directory tree of tracked files, per-file member summaries, and pointers to skill documents.

### 3.9 `guidance health`

- Purpose: documentation-coverage report over the indexed codebase.
- Metrics: files analyzed, total members, members without comments, comment-coverage percentage.
- Formats via `--format`: `ai` (default, Markdown table), `human` (plain labeled lines), `json` (structured object). `--min-age` is accepted but does not currently alter the report.

### 3.10 `guidance telemetry` and `guidance cache-stats`

- Purpose: inspect the search DB.
- Behavior: open the DB at `--db` and print node and embedding counts (telemetry) or cache statistics (cache-stats). When the DB is missing, print a "no database found" notice; when queries fail, print a per-metric notice rather than crashing. `telemetry --reset` is accepted.

### 3.11 `guidance benchmark [QUERY]`

- Purpose: measure search-answer quality, optionally with model-judged scoring.
- Inputs: optional query filter, `--num` query cap, `--no-llm`, `--verbose`, `--api-url` / `--model` overrides, `--timeout` (default 300 s), `--concurrency` (default 2).
- Behavior:
  - Loads scoring tasks from `.guidance/benchmarks.md` (section headings group tasks; each task is query text plus a `Rubric` expectation block terminated by `---`), or synthesizes fallback tasks from module comments when the file is absent.
  - Runs each task through the same search path as `explain`, optionally re-ranks and summarizes with the model under a concurrency cap, scores accuracy / relevance / completeness / navigation against the rubric, and prints per-query timings plus aggregate latency percentiles.

### 3.12 `guidance commit`

- Purpose: draft a commit message from staged changes with repository context, let the user edit it, then commit.
- Flow: read staged diff → load structural context for changed regions (members near changed hunks, capped in size) → resolve the commit model from config → generate a draft via LLM (fallback draft on model failure) → open the user's editor on a temp file → abort when the file is unsaved or emptied (after stripping comment lines) → otherwise commit (or print the message with `--dry-run`). `--force` continues past a hook rejection that would otherwise fail.
- Guardrails: refuses when nothing is staged; temp file is always cleaned up.

### 3.13 `guidance todo`

- Purpose: show outstanding tasks.
- Behavior: prints checklist lines (`- [ ]` / `- [x]`) from `.guidance/doc/TODO.md`, or a notice telling the user to create that file when absent.

### 3.14 `guidance diary [TEXT_OR_PATH]`

- Purpose: append a timestamped journal entry.
- Behavior: treats the argument as a file path when it names a readable file (ingesting its contents) and as inline text otherwise; appends a `## <UTC timestamp>` section to `.guidance/doc/DIARY.md` (creating directories as needed), recording `(empty entry)` for blank input. Always confirms the diary path.

### 3.15 `guidance mcp`

- Purpose: serve the index to AI coding assistants over stdin/stdout.
- Protocol: JSON-RPC 2.0 Model Context Protocol server (initialize, tools/list, tools/call). Exposes at least a `guidance_explain` tool that searches the index and returns ranked results with source locations, so an external agent gets the same answers as the CLI without shelling out. Runs until stdin closes; errors go to stderr with a nonzero exit.

## 4. Data model (as seen by users)

- **Module identity**: dotted relative path (`src/foo/bar.rs` → `src.foo.bar`), plus the raw relative source path and language tag.
- **Members**: each extracted declaration carries a kind (function, struct, enum, test, etc.), name, signature, parameter list, return type, 1-based line number, and optional comment. Comments are stored in source, surfaced in answers, and hashed for change detection — never treated as the source of truth inside JSON.
- **Mirror documents**: per-file JSON holding module metadata, an optional module-level comment, discovery keywords, skill references, reverse-dependency hints, and the member list.
- **Answer stages**: results are assembled as typed stages — prose (descriptions), code (excerpts with verified line numbers), metadata (keywords, skills, usage), and skill docs — then rendered as Markdown / JSON / compact / debug. Grounding rule: synthesis requires at least one code excerpt with a source and line number, and emitted `file:line` citations are checked against the excerpts provided.
- **Search tiers** (most to least deterministic): exact identifier index → domain-anchored lookup → keyword full-text → multi-keyword fusion → keyword+vector hybrid → vector-only. Latency expectations range from sub-millisecond for exact hits to hundreds of milliseconds for model-backed paths.

## 5. Configuration (user-visible)

`guidance-config.json` controls: mirror dir, JSON base, skills/inbox/capabilities dirs, DB path, embedding provider/model/dims, source dirs, provider endpoints (`base_url` + `chat_endpoint`), model roles (`models` map with `default`/`fast`/`thinking`/`batch`/`embed`, plus legacy flat fields), per-language test/lint/format commands, and embedding cache limits. Model references take the form `provider:name` (e.g. `ollama:code:latest`); the provider prefix selects the endpoint, the remainder selects the model. Lookup order: project config → user config → built-in defaults.

## 6. Approach summary

`guidance` treats codebase understanding as a maintainable local asset rather than a per-question model call: parse once, mirror cheaply, refresh incrementally, search locally, and spend model calls only on genuinely ambiguous work (comment drafting, message writing, relevance judging, synthesis). Every expensive or stochastic step has a deterministic floor underneath it — keyword fallback for search, structural output for explain, skip-with-warning for unparsable files, placeholder drafts for unreachable models — so the tool stays useful offline and its outputs stay auditable (file:line citations, DB queryability, config-driven gates).

## 7. Reproduction checklist (behavioral)

A faithful reimplementation must exhibit:

1. The 16-command CLI with the flags, defaults, and outputs above.
2. Incremental per-file mirrors plus a staleness report, force/dry-run/watch modes, and safe failure isolation per file.
3. The intent-classified, tiered-fallback search with exact-match priority and the documented scoring fallback.
4. The config discovery chain and model-reference resolution.
5. STRUCTURE.md generation, health coverage reporting, DB telemetry, benchmark task loading/scoring, commit drafting with editor round-trip, todo/diary note handling, and an MCP stdio server exposing index search.
6. Offline-safe defaults on every path, with model calls strictly optional enhancements.
