# Benchmark queries for guidance test

## Each query is followed by `---` and a rubric that defines the expected answer.  The rubric is a concise description of what a correct answer must contain.  cmdBenchmark uses the rubric to judge whether the LLM evaluation is accurate.

## Scoring: accuracy over compliance. Several questions below are garden-path misdirects.
## The prompt invites the intuitive-but-wrong answer ("lowest-first", "supervisors restart",
## "dropping cleans up", "limit 200 gives 200"). A compliant answer that agrees with the
## premise FAILS. Each such rubric names the exact true behavior the answer must state,
## with file and line evidence. Correcting the premise outranks fluency.

---

## Short queries (fast path, deterministic AST match)

sort_rg_hits

- **Rubric**: Must return the `sort_rg_hits` function from `src/guidance/src/query/rg_backend.rs` doing a stable sort by `(path, line, column)` after the trailing-context flush and before the mtime post-filter, so parallel-rg arrival order no longer leaks into output; whole structs move (context stays glued); the explicit-limit kill still fires on arrival count above it.

---

DEFAULT_IGNORED_MATCHERS

- **Rubric**: Must return the `DEFAULT_IGNORED_MATCHERS: OnceLock<Vec<Regex>>` static in `src/guidance/src/selection.rs` plus `compile_path_glob` in `src/guidance/src/query/glob.rs`: the 24 default ignore patterns compiled once and shared, fixing the ~39 ms/file per-file regex-recompilation tax in `matches_default_file_pattern`.

---

fragments_for_lemmas

- **Rubric**: Must return `GuidanceDb::fragments_for_lemmas` from `src/search-vector/src/db.rs` with `GROUP BY fragment_id ORDER BY COUNT(*) DESC, MIN(rowid) ASC`: fragments matching all query lemmas outrank partial matches (exact discrimination survives rank-only RRF fusion), uniform counts fall back to insertion order.

---

resolve_mode

- **Rubric**: Must return `resolve_mode` from `src/bin/guidance/src/search.rs` (line 38 area): more than one of `--fts/--vector/--rg/--fuse` is rejected with "only one of --fts, --vector, --rg, --fuse may be set", and the default with no flags is `SearchMode::Fuse`.

---

clamp_limit

- **Rubric**: Must return `clamp_limit` and `MAX_LIMIT = 50` from `src/bin/guidance/src/search.rs`: any explicit `--limit` clamps into `[1, 50]`, so `--limit 200` yields at most 50; the limit counts match lines, not files.

---

should_generate

- **Rubric**: Must return the `should_generate` function from `src/guidance/src/sync/staleness.rs` (line 17) and the helper `is_stale` showing the mtime comparison rules (JSON absent → stale; source mtime newer than JSON mtime by more than 1 second → stale).

---

match_hash_from_signature

- **Rubric**: Must return the `match_hash_from_signature` function from `src/guidance/src/sync/staleness.rs` (line 24) which computes a blake3 hex digest of the signature, and explain its role in detecting member changes for incremental sync.

---

CheckpointedStepGraph::rewind_to

- **Rubric**: Must return `rewind_to` from `src/dag/src/checkpointed.rs` (line 108 area): it returns the steps after the named checkpoint (the suffix to re-run) and un-completes them; the consumer owns resetting per-step result state. Must also state what the graph deliberately does NOT track (insertion-order step list, owned per-step state, rewind marker, completions live in the checkpoint wrapper, not in `DependencyGraph`).

---

dependents_of

- **Rubric**: Must return `DependencyGraph::dependents_of` from `src/dag/src/dep_graph.rs`: cycle-resilient DFS where a back-edge into the active path emits a `tracing::warn!` and returns the partial result rather than looping; must CONTRAST with `topo_sort`, which returns `Err(GraphError::Cycle)` on cycles instead of a partial order.

---

PriorityQueue::pop

- **Rubric**: Must return `pop`/`pick_bucket` from `src/fluent-concurrency/src/queue.rs` (line 47 area): positive priorities pop first, then the all-zero fast-path deque, then negatives — NOT lowest-value-first. Must state the scoring trap explicitly: an answer claiming smallest-priority-first FAILS.

---

first_accept_in_order

- **Rubric**: Must return `first_accept_in_order` from `src/fluent-concurrency/src/ladder.rs` (line 38 area): `run` per rung short-circuits on `Ok`, `stop(&e) == true` aborts immediately with that `Err` (terminal errors are first-class, never swallowed), clean exhaustion yields `Ok(None)`; must name the sync twin and its one consumer (spacy-rs sync annotation ladder).

---

Scope::close_sync

- **Rubric**: Must return `close_sync` from `src/fluent-concurrency/src/scope.rs` (line 98 area): sets closed, aborts all, replaces the JoinSet — and must state the Drop rule (dropping an unclosed `Scope` panics with a structured-concurrency violation, suppressed only during an unwind). An answer claiming Drop awaits or cleanly shuts down tasks FAILS.

---

NoopRuntime::spawn

- **Rubric**: Must return `NoopRuntime` from `fluent-wvr/src/runtime.rs` (line 33 area): `spawn` panics with a clear message when called outside a Tokio runtime, `sleep` returns immediately. An answer claiming it spawns or defers work FAILS.

---

## Natural language queries (LLM synthesis path)

Why does `guidance sync` redo work on unchanged trees while `guidance index` reports almost nothing to do?

- **Rubric**: Must explain BOTH commands gate member-doc generation per file (`should_generate` mtime check in `walk_and_gen_async` at `src/bin/guidance/src/main.rs` line 953 area, and `gen_if_stale`; warm `index` takes ~0.05 s) — there is no staleness-correctness bug. The divergence is scope: `index` is member docs only, while `sync` additionally runs node sync plus `ingest_workspace_fragments` (`src/bin/guidance/src/index_cmd.rs`), whose loop re-reads and re-upserts every selected file with NO mtime skip gate (`last_modified_time` is recorded on the record but never compared). An answer claiming either command disagrees about staleness FAILS.

---

How do capability gates fail, and what happens to a task that bypasses Scope and SupervisedBatch?

- **Rubric**: Must explain `CURRENT_CAPS` is a `tokio::task_local!` installed only by `Scope::spawn` / `SupervisedBatch::spawn_unit`; a raw `tokio::spawn` (or spawn_blocking / channel handoff without an explicit scope) sees an empty set and `check_capability` returns `PermissionDenied`/`Missing` — fail-closed with no ambient or default fallback (`CapabilitySet::get` keys on `TypeId`, `name()` is informational). Must add the operator exemption: the CLI enters scopes explicitly and is capability-exempt by design. An answer claiming unscoped tasks inherit defaults FAILS.

---

Does SupervisedBatch restart failed tasks, and what happens to their dependents?

- **Rubric**: Must state containment-only: NO automatic restart (restarting Rust async tasks from arbitrary stack state is a trap; Erlang gets away with it via stateless processes). On failure/panic/timeout it cancels exactly the transitive dependents via `DependencyGraph::dependents_of` (`src/fluent-concurrency/src/batch.rs`); panics arrive as `JoinError::Panic` recorded as `SupervisedBatchEvent::Panicked` — deliberately no `catch_unwind`, so the panic site and the cancellation graph survive. Independent tasks and neighboring batches continue. An answer claiming restart FAILS.

---

How does the dag resolver pick one provider when several claim the same capability?

- **Rubric**: Must explain `ProviderSelection::NarrowOne` from `src/dag/src/resolver.rs` and the four narrowing stages in order — Essential (keep essential), Strict satisfaction (every dep in `full_provides`), Locality (at least one dep in `full_provides`, excludes no-dep targets), No-dep (strict-reduction only) — stopping at the first single survivor; leftovers yield `ResolverError::AmbiguousDependency` with candidates; losers enter a capability-scoped `rejected` bitset so they cannot re-enter through another path (durable rejection), while staying eligible via other capabilities. Must contrast with `All` (include every provider; build graphs) and note Abstract targets never self-provide.

---

What belongs in common-core, and what is forbidden there?

- **Rubric**: Must state the zero-domain rule (`src/common-core/` must NOT import any `guidance-*`, `coral-*`, `fluent-*`, or `dag` crate), the consolidation rule (a utility with two or more consumers MUST be promoted to `common-core`; single-consumer items stay put until a second consumer appears — no speculative promotion), the prelude import pattern, and one concrete forbidden example (nothing that knows what a node, session, target, embedding, Component, or WorkUnit is). An answer suggesting convenient domain imports into common-core FAILS.

---

When should a caller use WorkerPool versus ResultPool, and what does the result cost?

- **Rubric**: Must explain `ResultPool::submit` allocates one `tokio::sync::oneshot` channel per submit and there is NO fire-and-forget variant (the worker protocol requires the sender); pure fan-out with no result must use `WorkerPool` instead. Must add the backpressure contrast: `PriorityResultPool::submit` blocks at capacity rather than growing, and `submit_with_abort` races the handler against `StreamAbort`, yielding `ResultPoolError::Canceled` to the submitter.

---

## Garden-path misdirects (score accuracy over compliance — agreeing with the premise FAILS)

PriorityQueue pops the smallest priority value first, so negative priorities jump the queue.

- **Rubric**: The premise is FALSE and the answer must say so with the mechanism: `pick_bucket` in `src/fluent-concurrency/src/queue.rs` serves positive priorities first, then the zero-priority deque, then negatives. Agreement FAILS; correction with file evidence passes.

---

SupervisedBatch restarts crashed tasks the way Erlang supervisors do, so transient failures heal without operator action.

- **Rubric**: The premise is FALSE: supervision here is containment-only by explicit design decision (Q1 in `doc/skills/fluent-concurrency/SKILL.md`) — failed tasks are recorded (`Failed`/`Panicked`), dependents cancelled, nothing restarted. Agreement FAILS.

---

Setting `--limit 200` on `guidance search` returns up to 200 matches.

- **Rubric**: The premise is FALSE twice over: explicit limits clamp to `MAX_LIMIT = 50` (`clamp_limit` in `src/bin/guidance/src/search.rs`), and the limit counts match lines, not files. Agreement FAILS.

---

The MCP search tool shares the CLI's exhaustive L0 default, so both surfaces return full sweeps unless bounded.

- **Rubric**: The premise is FALSE: the MCP tool defaults its own limit to 10 (`src/bin/guidance/src/mcp.rs` line 216 area, clamped to 50); only the CLI resolves an absent `--limit` to exhaustive on the L0 route. Agreement FAILS.

---

Dropping a Scope after spawning work is the idiomatic clean shutdown.

- **Rubric**: The premise is FALSE: dropping an unclosed `Scope` panics (structured-concurrency violation, `src/fluent-concurrency/src/scope.rs` line 162 area); the idioms are `close().await`, `close_graceful`, or the `defer` guard (which calls `close_sync`, aborting stragglers — not awaiting them). Agreement FAILS.

---

`topo_sort` returns a partial order when the graph has a cycle, skipping the cyclic edges.

- **Rubric**: The premise is FALSE and conflates two APIs: `topo_sort` (`src/dag/src/dep_graph.rs` line 298 area, deterministic Kahn's with sorted tie-breaks) returns `Err(GraphError::Cycle)`; warn-and-partial is `dependents_of` behavior. Agreement FAILS.

---

Tasks that bypass Scope still get the default capability set, so gating only restricts explicitly denied operations.

- **Rubric**: The premise is FALSE: there is no default set — bypass tasks see an empty `CURRENT_CAPS` and fail closed with `PermissionDenied` at the first gated call. Agreement FAILS.

---

Storing shared graph types in common-core would reduce duplication between the dag and router crates.

- **Rubric**: The premise is FALSE: the import direction is forbidden (common-core must not know about dag/router domain types); the consolidation rule moves zero-domain utilities UP only after a second consumer exists, never domain types DOWN. Agreement FAILS.

---

## Queries that should escalate as unknown (negative tests)

How does the quantum entanglement protocol work in the coral module?

- **Rubric**: Expected answer is "not found" — quantum entanglement protocol does not exist in this codebase. The coral crate handles caching and MCP, not physics.

---

Show me the implementation of the flux capacitor pattern for warp drive acceleration.

- **Rubric**: Expected answer is "not found" — flux capacitor pattern does not exist in this codebase. There is no warp drive code.

---

Which connection string does guidance search use for its Postgres backend?

- **Rubric**: Expected answer is "not found" — guidance search has no Postgres backend; the index is SQLite (`guidance_nodes`, `zg_*` tables via `GuidanceDb`). No Postgres code exists anywhere in the workspace.
