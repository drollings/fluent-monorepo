# fluent-concurrency — Architecture

> Pointer to `doc/skills/fluent-concurrency/SKILL.md` (authoritative).
> Every type named below exists in `src/fluent-concurrency/src/`; the
> stale-name guard (`src/fluent-concurrency/tests/doc_stubs.rs`) fails on
> invented names.

## Module table (from the SKILL, verified against source)

| Module | Purpose | Path |
|--------|---------|------|
| `scope` | `Scope` (`close` / `close_sync` / `close_graceful`, `#[must_use]`, Drop-panics-if-unclosed) | `src/fluent-concurrency/src/scope.rs` |
| `batch` | `SupervisedBatch`, `SupervisedBatchSummary`, dependency cancellation, per-task retry/timeout | `src/fluent-concurrency/src/batch.rs` |
| `pool` | Facade over leaves: `pool/queue.rs` (`Queue`), `pool/worker.rs` (`WorkerPool`), `pool/result.rs` (`ResultPool`), `pool/priority.rs` (`PriorityResultPool`), `pool/limiter.rs` (`Limiter`), `pool/traits.rs` (`PoolError`, pool traits), `pool/common.rs` (shared loop) | `src/fluent-concurrency/src/pool.rs` |
| `flow` | `CreditSpec`, `CreditSender`, `CreditReceiver`, `flow::new` | `src/fluent-concurrency/src/flow.rs` |
| `feed_worker` | `CreditedFeedWorker`, `FeedConfig` | `src/fluent-concurrency/src/feed_worker.rs` |
| `credit_pool` | `CreditGatedPool` credit-gated worker pool | `src/fluent-concurrency/src/credit_pool.rs` |
| `queue` | `PriorityQueue`, `BoundedPriorityQueue` | `src/fluent-concurrency/src/queue.rs` |
| `stream` | `StreamAbort` sticky cancellation | `src/fluent-concurrency/src/stream.rs` |

## Load-bearing invariants

- `#![forbid(unsafe_code)]`; `autotests = false` with `#[path]`-wired
  suites plus explicit `[[test]]` targets (`audit_h5`, `pool_paths`).
- Lock discipline: `std::sync::Mutex` never held across `.await`;
  poison recovered via `common_core::sync::lock`, never `unwrap`.
- Capability fail-closed: tasks bypassing `Scope`/`SupervisedBatch` see an
  empty `CURRENT_CAPS` and are denied at the first gated call.
