# `fluent-concurrency` — Lightweight Async Runtime Framework Specification

## 1. Executive Summary

This document specifies `fluent-concurrency`, a thin, safe, composable extension layer over **Tokio**. It is designed for systems that need the operational resilience of RabbitMQ (bounded worker pools, credit-based backpressure, priority queues, supervision zones) and the minimalism of `smol`, without reimplementing Tokio's scheduler, I/O driver, or timer wheel.

**Core philosophy:**
- **Tokio is the workhorse.** We do not rebuild `async_executor`, `epoll/kqueue/IOCP`, or work-stealing. We *compose* Tokio's primitives.
- **Fluent WVR is the control plane.** Every unit of work presents the same `Component` / `WorkUnit` interface regardless of origin (native struct, WASM plugin, DB config). The orchestrator never branches on implementation type.
- **Safety and locality.** 100% safe Rust (`#![forbid(unsafe_code)]`). No procedural macros hiding task boundaries. `dyn Trait` is restricted to the Control Plane; the Data Plane uses concrete types and flat enums.
- **Explicit ownership.** No ambient authority. Every effect requires a capability token. Every spawned task belongs to a `Scope` whose close must be awaited.

## 2. Design Decisions (Resolving Manifest Open Questions)

| Question | Resolution | Rationale |
|----------|------------|-----------|
| **Q1 — Supervision restart** | **Containment-only.** A `Zone` catches task panics, emits a typed `ZoneEvent`, and cancels dependent tasks. It does **not** automatically restart. | Restarting async tasks from arbitrary state is a checkpoint-semantics problem. RabbitMQ's `supervisor2` gets away with it because Erlang processes are stateless on restart. Rust async tasks carry arbitrary stack state; automatic restart is a trap. We add restart only when profiling proves it necessary. |
| **Q2 — Capability granularity** | **Per-scope establishment with task-local inheritance.** Entering a `Zone` or `Scope` installs a `CapabilitySet` into a `tokio::task_local!`. All `spawn` calls within the scope capture the current set and reinstall it in the child task. | Per-call `&Capability` at every `spawn` site adds ceremony without meaningful security gain when zone boundaries are already enforced. Effect *entry points* (e.g., `fs::read`, `db::query`) still require an explicit `&Capability` parameter in their signature. |
| **Q3 — Deterministic testing** | **Both, phased.** The `Runtime` trait supports a `TestRuntime` that uses Tokio's `start_paused` virtual time + a deterministic `Rng` seed for **record-replay**. For **combinatorial exploration**, the trait is designed to swap in a future `LoomRuntime` backend. The initial stack ships record-replay; loom integration is a future primitive. | A full loom-compatible async executor is a research project. Shipping it now would violate the "no academic abstraction inflation" red flag. The trait boundary is wide enough to add it later without breaking user code. |

## 3. Core Primitives

### 3.1 `Capability` — Bounded Resource Access

Every high-overhead effect (file system, database, AI inference endpoint, blocking thread pool) requires a non-cloneable capability token.

This is a lightweight, safe, two-phase effect pipeline. It maps directly to RabbitMQ's `credit_flow` and Tokio's `Semaphore` semantics, but without the lifetime complications of `tokio::sync::SemaphorePermit`.

### 3.2 `Scope` — Structured Concurrency & Region Ownership

A `Scope` is the fundamental owner of tasks. It is **`must_use`** and requires explicit `await` to close.

**Why not `async Drop`?** Rust does not have async drop. The RabbitMQ Erlang model achieves this because `supervisor2` runs in its own process and can block on `receive`. In Rust, the only way to *guarantee* a child is awaited before the parent frame exits is to make the parent frame itself a `Future` that ends with `scope.close().await`. The `must_use` + `debug_assert!` pattern enforces this at the API level without unsafe or proc macros.

### 3.3 `Zone` — Failure Containment & Supervision

A `Zone` is a `Scope` plus a dependency graph and a diagnostic event sink.

**Key properties:**
- A panic in task A does **not** propagate to the parent runtime thread. It is caught as a `JoinError` by the zone's `poll` loop.
- The zone cancels only the dependents of the failed task; independent tasks continue.
- Neighboring zones are fully isolated because each zone owns its own `JoinSet`.

### 3.4 `WorkerPool` — Bounded Worker Pool

RabbitMQ's `worker_pool` uses a central queue and a fixed set of worker processes that pull jobs. We translate this directly to Tokio tasks.

**Why not `tokio::sync::Semaphore`?** A `Semaphore` is perfect for a *limiter* (see below), but it does not provide a FIFO queue of jobs or dedicated workers. RabbitMQ's `worker_pool` explicitly wants workers to pull from a queue, allowing prioritization and monitoring of queue depth. Our `WorkerPool` gives exactly that.

### 3.5 `Limiter` — Lightweight Concurrency Cap

For cases where you don't need a dedicated worker pool, just a cap on concurrent executions:

This is the Rust equivalent of the `credit_flow` sender side: acquire a slot, run the work, release the slot on completion.

### 3.6 `PriorityQueue` — Event Queue

A simple priority queue optimized for the common case where most items have priority 0, exactly like RabbitMQ's `priority_queue.erl`.

This is O(log P) for `push` and `pop`, where P is the number of distinct non-zero priorities. It is zero-allocation for the all-zero-priority case.

### 3.7 `CreditFlow` — Chain Backpressure

RabbitMQ's `credit_flow` module throttles publishers end-to-end. Our `CreditFlow` uses explicit message passing between sender and receiver, preserving the exact semantics.

This maps 1:1 to the Erlang `credit_flow` semantics: `send` decrements credit, `ack` (called `recv` here) counts down and sends a `bump_credit` when the counter hits zero.

### 3.8 `PartitionedRouter` — Delegate / Sharding

RabbitMQ's `delegate` module groups PIDs by node and routes them to local delegates to reduce inter-node chatter. In a single-process Rust system, this becomes a key-based router.

This preserves causal ordering: all jobs with the same key always go to the same shard.

### 3.9 `Runtime` Trait — Pluggable Backend

**Why `BoxFuture`?** The `Runtime` trait is object-safe so it can be stored as `Arc<dyn Runtime>` in the Control Plane. The cost of one `Box` per `sleep` is negligible because `sleep` is a boundary operation, not a hot-loop inner operation.

## 4. Control Plane / Data Plane Integration (Fluent WVR)

The `fluent-wvr` crate defines `WorkUnit`, `WorkContext`, `WorkOutput`, and `Component`. `fluent-concurrency` does not redefine these; it **consumes** them.

**Cross-cutting concerns** (retry, timing, rate limiting) are applied via the `Instrumented` and `WithRetry` newtype wrappers from `fluent-wvr` *before* type erasure, preserving zero-cost inlining.

## 5. Performance & Locality Guarantees

| Hot Path | Technique | Why |
|----------|-----------|-----|
| Task scheduling | Tokio's local queue + LIFO slot | We do not add indirection. |
| Worker pool job dispatch | `VecDeque` in `Mutex` | One lock per pop; workers sleep on `Notify`. No `dyn` dispatch per job. |
| Priority queue (all same priority) | `VecDeque` fast path | Zero overhead for the common case. |
| Data transformation | Concrete enums + pattern matching | `WorkUnit::execute` is one vtable call per task; inside it, all work is monomorphized. |
| Capability check | `AtomicUsize` counter | Lock-free, no heap allocation. |

## 6. Crate Layout (Proposed)

**Dependencies:**
- `tokio` (features: `rt-multi-thread`, `sync`, `time`, `macros`)
- `fluent-wvr` (for `WorkUnit`, `Component`, `ArcIntern`)
- `serde_json` (for `Describable`)
- `internment` (for `ArcIntern`)

No `async-trait`, no `proc-macro` crates, no `bumpalo`, no `crossbeam` (Tokio's channels and `Notify` are sufficient).

## 7. Anti-Patterns Explicitly Rejected

1. **No `#[async_trait]` or macro-heavy execution.** The framework uses manual `Future` impls and `async fn` where the compiler can see the boundaries.
2. **No `tokio::spawn` without a scope.** The only `spawn` in the framework is `Scope::spawn`, which tracks the handle.
3. **No `dyn Trait` in a per-item loop.** The `PartitionedRouter` hashes once per batch; `PriorityQueue` dispatches via `BTreeMap` keys, not vtables.
4. **No ambient `tokio::fs::read` or `tokio::time::sleep`.** All I/O must be called with a `&Capability` or within a capability-bearing scope.
5. **No automatic restart.** Zones contain; they do not restart. Restart is a deliberate operator action.

## 8. Rejected as Scope Creep

Here are examples of what fluent-concurrency does not try to do as a lightweight single-node runtime, compared to RabbitMQ:

- Actor Hibernation / Idle Backoff: RabbitMQ's gen_server2 needs hibernation because it manages hundreds of thousands of idle, long-lived connections. fluent-concurrency is a pipeline execution engine—tasks are spawned to complete work and terminate. Workers parked on tokio::select! are sleeping efficiently on native OS epoll/kqueue event loops. Adding an explicit backoff framework here adds unnecessary overhead.
- Multi-hop Credit Chains: Our single-hop producer/consumer backpressure is perfectly tailored for a single-node pipeline. We do not need a multi-process AMQP chain.

## 9. Summary

`fluent-concurrency` is a **thin, safe, opinionated harness** over Tokio. It adds the operational primitives that RabbitMQ proved necessary in production (pools, credit flow, supervision, priority) while keeping the Data Plane as fast and flat as `smol`. It follows the Fluent WVR pattern so the orchestrator sees a uniform interface, and it enforces the five architectural pillars of the manifest without unsafe code, bloat, or overengineering.
