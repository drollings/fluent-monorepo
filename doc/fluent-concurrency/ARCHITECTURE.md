# fluent-concurrency — Architecture

> Pointer to `doc/skills/fluent-concurrency/SKILL.md` (authoritative).
> Every type named below exists in `src/fluent-concurrency/src/`; the
> stale-name guard (`src/fluent-concurrency/tests/doc_stubs.rs`) fails on
> invented names.

## Module table (from the SKILL, verified against source)

| Module | Purpose | Path |
|--------|---------|------|
| `scope` | `Scope` (`close` / `close_sync` / `close_graceful → ScopeCloseReport`, `#[must_use]`, Drop-panics-if-unclosed), `CURRENT_CAPS` re-export | `src/fluent-concurrency/src/scope.rs` |
| `batch` | `SupervisedBatch`, `SupervisedBatchSummary`, `SupervisedBatchConfig { poll_budget, is_retryable, backoff_base: Duration, backoff_jitter_pct }`, dependency cancellation, per-task retry/timeout | `src/fluent-concurrency/src/batch.rs` |
| `cache` | `AsyncLoadCache` — async bounded get-or-load LRU + singleflight (`get_or_compute[_with]` + `admit` predicate) | `src/fluent-concurrency/src/cache.rs` |
| `affinity` | `AffinityScheduler`, `ScheduledTask`, aging config | `src/fluent-concurrency/src/affinity.rs` |
| `capability` | `default_capability_set` / `capability_set_with_db` constructors | `src/fluent-concurrency/src/capability.rs` |
| `io` | Capability-gated `fs`/`net` engines + `DbCapability` re-export | `src/fluent-concurrency/src/io.rs` |
| `ladder` | Re-export shim: `first_accept_in_order[_sync]`, `first_some_in_order_sync` (impl in `fluent-wvr`) | `src/fluent-concurrency/src/ladder.rs` |
| `runtime` | `TokioRuntime` / `TestRuntime` runtime seam | `src/fluent-concurrency/src/runtime.rs` |
| `thread_resource` | `thread_local_resource!` + `with_tlr` per-thread reuse | `src/fluent-concurrency/src/thread_resource.rs` |
| `close` / `sync` | Close-wake primitives (`CloseWaker`) / poison-safe lock helpers | `src/fluent-concurrency/src/close.rs`, `src/fluent-concurrency/src/sync.rs` |
| `retry` | Re-export of the `common_core::retry` schedule math | `src/fluent-concurrency/src/retry.rs` |
| `pool` | Facade over leaves: `pool/queue.rs` (`Queue`), `pool/worker.rs` (`WorkerPool`), `pool/result.rs` (`ResultPool`: `submit/submit_with_backpressure/submit_all/submit_with_abort`), `pool/priority.rs` (`PriorityResultPool`), `pool/limiter.rs` (`Limiter`), `pool/traits.rs` (`PoolError`, pool traits), `pool/common.rs` (shared loop) | `src/fluent-concurrency/src/pool.rs` |
| `flow` | `CreditSpec`, `CreditSender`, `CreditReceiver`, `flow::new` | `src/fluent-concurrency/src/flow.rs` |
| `feed_worker` | `CreditedFeedWorker`, `FeedConfig` | `src/fluent-concurrency/src/feed_worker.rs` |
| `credit_pool` | `CreditGatedPool` credit-gated worker pool | `src/fluent-concurrency/src/credit_pool.rs` |
| `queue` | `PriorityQueue`, `BoundedPriorityQueue` | `src/fluent-concurrency/src/queue.rs` |
| `stream` | `StreamAbort` sticky cancellation | `src/fluent-concurrency/src/stream.rs` |

## Runtime-lifetime and HTTP-client ownership

**A pooled resource must never outlive the runtime that created it.** This is
the workspace's most expensive recurring async hazard: a connection handed
back to a client's pool by work spawned on a runtime that is then dropped
leaves the connection stranded with a waker whose runtime no longer exists.
The next caller that checks it out parks forever — no error, no CPU, no
recovery, bounded only by that client's timeout.

Rules:

- Never bridge on a runtime built for a single call.

- Tests must not share a pooled `reqwest::Client` across `#[tokio::test]` runtimes.

- `#![forbid(unsafe_code)]`; `autotests = false` with `#[path]`-wired
  suites plus explicit `[[test]]` targets (`audit_h5`, `pool_paths`,
  `doc_stubs`, `scope_caps`, `scoped`, `pool_submit_all`).
- Lock discipline: `std::sync::Mutex` never held across `.await`;
  poison recovered via `common_core::sync::lock`, never `unwrap`.
- Capability fail-closed: tasks bypassing `Scope`/`SupervisedBatch` see an
  empty `CURRENT_CAPS` and are denied at the first gated call.
