# ADR-004: Sync execute timeouts (H5) — proposal, no code

## Context
`SupervisedBatch` wraps `unit.execute` (sync) in `tokio::time::timeout`. A blocking `execute` (sleep/busy-loop) holds its worker; the timer cannot fire until `execute` returns, and then inner wins the race → `Completed` instead of `Cancelled`. Measured: 400ms sleep with 50ms budget → `completed=1`, elapsed 400ms.

## Options
- (A) Run `execute` on `spawn_blocking`: timeout can fire, zombie continues on blocking pool. Cost: thread hop per task, unbounded blocking pool growth under load, retry overlap still possible.
- (B) Cooperative token in `WorkContext` (e.g. `cancel: CancellationToken`): `execute` polls it. Cost: opt-in, sync code must check; pure contract unchanged.
- (C) Document hard limit: timeouts apply to async suspension only; blocking `execute` violates purity contract and timeout is best-effort. Cost: docs only, silent-ignore remains footgun.
- (D) Do nothing.

## Recommendation
(B) + (C): add `WorkContext::cancelled()` cooperative flag (default non-cancelled), document that `timeout_ms` does not preempt blocking code, and route `DbWorkUnit`-style offload as canonical pattern. (A) only if profiling shows supervisor starvation.

## Risks
(A) hides blocking; (B) requires unit cooperation; (C) leaves S1 footgun documented but unfixed.
