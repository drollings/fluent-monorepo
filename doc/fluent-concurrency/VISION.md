# fluent-concurrency — Vision

> Status: Implemented. The authoritative contract lives in
> `doc/skills/fluent-concurrency/SKILL.md`; this file is a pointer.

## Mission

`fluent-concurrency` is a thin, safe, composable extension over Tokio — the
operational resilience of bounded worker pools, credit backpressure, and
supervision batches without reimplementing the scheduler, I/O driver, or
timer wheel. Every task is owned (`Scope`-spawned tasks are awaited at
close; server tasks drain at graceful shutdown); effects are
capability-gated; panics are contained (`SupervisedBatch` cancels only
transitive dependents) and never silently restarted.

## What belongs / what does not belong

Belongs: `Scope` structured concurrency; `SupervisedBatch` failure
containment; `WorkerPool` / `ResultPool` / `PriorityResultPool` pools;
`Limiter` concurrency cap; `CreditFlow` (`flow::new`) chain backpressure;
`CreditedFeedWorker` credit-gated feeds; `ReconnectWorker` gap supervision;
`PriorityQueue` event ordering; `StreamAbort` sticky cancellation;
`tokio_runtime` production backend.

Does not belong: scheduler or I/O driver internals (Tokio's job);
`async_trait` (native AFIT only); automatic restart (containment only);
second copies of `retry_async` (owned by `common-core`) or the ladder
(owned by `fluent-wvr`).

## Consumers (verified `Cargo.toml` dependents)

`fluent-router`, `guidance-core`, `coral-context`, `fluent-llm`,
`spacy-rs`, plus the `coral-router` and `guidance` binaries.

## End-to-end capabilities

A service needing owned concurrency gets it without touching the
scheduler: scoped task trees that drain gracefully on shutdown,
failure containment that cancels only transitive dependents (never
silent restarts), bounded pools with priority lanes, credit-gated
producer/consumer chains with durable spill, sticky cancellation
that frees worker slots mid-turn, and backpressure math shared with
the rest of the workspace.

## Non-goals

- No `tokio::sync::Mutex` where `std::sync::Mutex` suffices (the one
  documented exception is `CreditSender`, whose guard crosses `recv().await`).
- No per-call runtime bridges; `Limiter::run_sync` composes
  `block_on_shared` and parks (never for throughput paths).
- The per-attempt timeout bounds the await/retry loop, never a blocking
  body (the H5 zombie characterization is pinned, not fixed).
