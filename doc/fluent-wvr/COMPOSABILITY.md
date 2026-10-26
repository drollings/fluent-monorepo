# Composability Guide

> Day-to-day patterns for using the shared infrastructure in this workspace.
> Covers `fluent-wvr` (control plane), `fluent-concurrency` (async runtime),
> and the `common-core` helpers they compose. For the full design rationale
> see `doc/skills/fluent-wvr/SKILL.md` and
> `doc/skills/fluent-concurrency/SKILL.md`.

---

## 1. Build a Component in 30 seconds

Derive `FieldAccess` + `Describable`, implement `WorkUnit` manually, then add
`impl_component!`. Never hand-write `FieldAccess` when the derive covers it,
and never hand-roll `as_any`/`as_any_mut`.

```rust
use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};

// Single source of truth: one struct drives field access, validation,
// and JSON Schema. `execute` stays synchronous and non-blocking (see §2).
#[derive(FieldAccess, Describable)]
pub struct MyConfig {
    #[field(desc = "TCP port", min = 1, max = 65535)]
    pub port: u16,
    #[field(desc = "Enable TLS", required = false)]
    pub tls: bool,
}

impl WorkUnit for MyConfig {
    fn name(&self) -> &str { "my_config" }
    fn depends(&self) -> &[ArcIntern<str>] { &[] }
    fn provides(&self) -> &[ArcIntern<str>] { &[] }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        WorkOutput::ok_with_data(
            "configured",
            serde_json::json!({ "port": self.port, "tls": self.tls }),
        )
    }
}

fluent_wvr::impl_component!(MyConfig);
```

`#[field(...)]` attributes (from `src/fluent-wvr-macros/src/lib.rs`):

| Attribute | Effect |
|---|---|
| `desc = "..."` | Human description in `describe()` schema |
| `min = N` / `max = N` | Numeric range enforced by `set_field` |
| `clamp` | Clamp to `[min, max]` instead of rejecting |
| `skip` | Exclude from `FieldAccess` + schema |
| `required = false` | Optional in schema (default `true`) |
| `format = "..."` | Informational `format` hint in schema |
| `max_len = N` | Reject longer strings |
| `sanitize = "trim\|lowercase\|strip_html\|slugify"` | Transform before validation |
| `pattern = "..."` | Substring that must be present (not a regex) |
| `coerce = "trim,strip_quotes,..."` | Boundary-string pipeline before parse (`trim`, `lowercase`, `strip_quotes`, `json_escape`, `normalize_literal`) |
| `parse = "number"` | Tolerant numeric parse (`parse_number`) |
| `empty_is_none = false` | Keep empty string instead of mapping to `None` for `Option<String>` (default `true`) |
| `nested` | The field is itself a component: dotted-path access, delegated schema |
| `items = N` | `Vec<T>` list member with at most `N` items (required on every `Vec<T>`; inferred for `[T; N]`) |
| `fixed` | Fixed-arity list (`[T; N]`, no `count` writer) rather than open (`Vec<T>`) |
| `visible_when = "kind=breakout"` | Editable only while the named selector holds the named value |
| `axis = "..."` | Which axis the member measures: `correctness`, `task-value`, `producer-confidence` |
| `tab` / `group` / `tooltip` / `step` / `inline` | Presentation-only `x-` schema hints (tab pin, section, hover help, drag sensitivity, shared-row marker); never change acceptance |
| `version_added` / `version_removed` | Reserved, planned-not-implemented: using them fails expansion |

Rules:

- `field_names()` and `describe()` are **instance** methods (`&self`), so
  they dispatch through `Arc<dyn Component>`. Never use a static fn shape.
- Configure Rust-struct components **before** wrapping in `Arc`
  (`component.set_field(...)` needs `&mut`). Components with interior
  mutability (`WasmComponent`, `DynamicComponent`) configure through their
  internal lock instead.
- `set_field` on a shared `Arc<dyn Component>` fails with
  `FieldError::ReadOnly` — use `ComponentArcExt::try_as_any_mut` for a
  fallible path, or `ComponentAdapter` (§8) for runtime overrides.
- For units with no configurable fields, use `impl_fieldless!(T)` instead
  of the derive.

---

## 2. Supervise N components with `SupervisedBatch`

`SupervisedBatch` owns a `JoinSet` plus a `DependencyGraph`: it runs units
with per-attempt timeout, jittered-backoff retry, and dependency-aware
cancellation. It is **containment-only** — it cancels dependents, it never
auto-restarts.

```rust,ignore
use std::sync::Arc;
use fluent_concurrency::batch::SupervisedBatch;
use fluent_wvr::{CapabilitySet, Runtime};
use fluent_wvr::prelude::*;

// Production runtime: fluent_concurrency::tokio_runtime().
// Tests/record-replay: TestRuntime. Dry-run/init: NoopRuntime.
let rt: Arc<dyn Runtime> = fluent_concurrency::tokio_runtime();
let caps = CapabilitySet::new();
let mut batch = SupervisedBatch::new(Arc::clone(&rt), caps.clone());

// register returns Result: duplicate name() values are rejected with
// SupervisedBatchError::DuplicateName, not panicked.
batch.register(unit_a)?; // provides "assets_a"
batch.register(unit_b)?; // depends on "assets_a"

// Per-unit overrides (retries, timeout, caps, typed handoff seed):
batch.register_with_context(unit_c, WorkContext::for_unit_in_batch(
    &rt,
    &caps,
    |ctx| { ctx.max_retries = 2; ctx.timeout_ms = 5_000; },
))?;

let summary = batch.await;
assert!(summary.panicked.is_empty());
assert_eq!(summary.completed.len(), 3);
// summary.failed: WorkError::Execution; summary.cancelled: Timeout |
// DependencyFailed | Aborted. Panics land in summary.panicked only.
```

What the supervisor drives:

- **Purity contract.** `WorkUnit::execute` is sync, non-blocking, no I/O:
  no `tokio::spawn`/`sleep`/`block_on`, no shared-state mutation without
  synchronization. Blocking DB work goes through `fluent_db::wvr::store_unit`
  (offloads via `block_in_place`), never inline.
- **Retry/timeout.** `ctx.max_retries` / `ctx.timeout_ms`
  (`default_timeout_ms()` default 30_000). `WorkError::is_retryable()` is the
  predicate: `Dependency` and `Timeout` retry, `Execution` does not.
- **Dependencies.** `depends()`/`provides()` feed a shared
  `DependencyGraph`; on fail/panic/timeout the batch aborts transitive
  dependents. New dependency-tracking workflows compose
  `fluent_dag::dep_graph::DependencyGraph` directly — never re-implement it.
- **Capabilities.** Each task runs with `CURRENT_CAPS` installed from the
  effective `WorkContext.caps`. Raw `tokio::spawn` bypasses this and fails
  closed (`PermissionDenied`) at the first gated call.
- **Tuning.** `SupervisedBatch::new_with_config(rt, caps,
  SupervisedBatchConfig { poll_budget, is_retryable, backoff_base_ms,
  backoff_jitter_ms })`. `poll_budget` (default 64) bounds ready-task
  drains per poll so one batch cannot starve the executor.

In-tree shape: `protopyne-venue` runs each bar's checks as supervised units
(`check_unit`/`run_bar`), and `protopyne-server` runs boot gates under a
batch — failures cancel dependents, independent work continues.

---

## 3. Share a process-wide HTTP client

Instead of `reqwest::Client::new()` in every test, use the shared client
from `common-core`. It amortizes TLS handshakes and connection pool setup.

```rust,no_run
use common_core::http::shared_http_client;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = shared_http_client();
let resp = client.get("https://example.com").send().await?;
# Ok(())
# }
```

In tests, use `test_http_client()` for a consistent User-Agent:

```rust,ignore
use common_core::http::test_http_client;

let client = test_http_client();
```

Capability-gated network access (serving path) goes through
`NetCapability::http_get`/`http_post`/`tcp_connect` instead — see §10.

---

## 4. Cap concurrency with `Limiter`, pools, credit flow, and ladders

### `Limiter` — async semaphore

```rust,ignore
use std::sync::Arc;
use fluent_concurrency::pool::Limiter;

let limiter = Arc::new(Limiter::new(3)); // at most 3 concurrent

for url in urls {
    let limiter = Arc::clone(&limiter);
    scope.spawn(async move {
        limiter.run(|| async {
            fetch(url).await
        }).await;
    });
}
```

Prefer `Scope::spawn`/pool workers over raw `tokio::spawn` so capability
propagation and ownership hold (§5). Prefer async `run` everywhere.

### `Limiter::run_sync` — for sync callers only

```rust,ignore
use fluent_concurrency::pool::Limiter;

let limiter = Limiter::new(2);
let result = limiter.run_sync(|| async {
    expensive_computation().await
});
```

`run_sync` bridges a genuinely synchronous caller via `block_in_place` (or a
dedicated current-thread runtime when none is active). It parks the calling
worker thread, so it is **not for throughput-critical paths** — bound it to
control-plane callers that cannot `await`.

### `WorkerPool` — background worker with a queue

```rust,ignore
use std::sync::Arc;
use fluent_concurrency::pool::WorkerPool;
use fluent_wvr::Runtime;

let rt: Arc<dyn Runtime> = fluent_concurrency::tokio_runtime();
let pool = WorkerPool::new(rt, 4, 100, |job: MyJob| async move {
    process(job).await;
});

pool.submit(MyJob { /* ... */ }).await?; // waits when full; Err(Closed) if shut
pool.try_submit(MyJob { /* ... */ }).await?; // Err(Full) instead of waiting
pool.shutdown().await;
```

### `ResultPool` — worker pool that returns results

```rust,ignore
use fluent_concurrency::pool::{ResultPool, ResultPoolError};

let pool = ResultPool::new(rt, 4, 100, |n: i32| async move {
    Ok::<i32, String>(n * 2)
});
let result: i32 = pool.submit(21).await.map_err(|e| match e {
    ResultPoolError::Pool(e) => format!("pool: {e}"),
    ResultPoolError::Inner(e) => e,
    ResultPoolError::Canceled => "canceled".into(),
})?;
assert_eq!(result, 42);

// Abort-aware variant: dropping the handler on StreamAbort fires Canceled.
let abort = StreamAbort::new();
let r = pool.submit_with_abort(21, Some(abort.clone())).await;
```

One `oneshot` is allocated per submit. For true fan-out with no result, use
`WorkerPool` instead. In-tree shape: `protopyne-server` fans parameter
sweeps over a `ResultPool` and re-sorts outputs by index.

### `PriorityResultPool` — priority-ordered results

Jobs submit with `submit(job, priority)`; higher values dispatch first, FIFO
within a band. `new(rt, cap, handler)` sizes the queue to `cap * 4`;
`with_queue_capacity` overrides it. `submit` blocks at capacity (backpressure),
it never grows without bound.

### `CreditGatedPool` — credit-bounded workers

`CreditGatedPool::new(rt, credit_limit, queue_capacity, handler)` gates the
submit path on a credit token and releases one per processed job. Compose it
for producer/consumer loops that must not grow without bound — never re-copy
the `CreditSpec` scaffolding per worker.

### `CreditFlow` — single-hop backpressure

```rust,ignore
use fluent_concurrency::flow::{self, CreditSpec};

let (tx, rx) = flow::new(CreditSpec { initial: 8, more_after: 4 });
// Producer: tx.send(|| async { enqueue(job).await }).await; blocks when exhausted.
// Consumer: rx.recv(); // releases a token per processed item.
```

`is_blocked()`/`current_credit()` expose the gate state. In-tree shape:
`protopyne-venue::TradingSession` holds a `flow::new` pair whose receiver
releases one credit per venue submit.

### `first_accept_in_order` — the first-Ok ladder

Any "try A, then B, then C, take the first success" walk composes this —
never a hand-rolled loop.

```rust,ignore
use fluent_concurrency::ladder::{first_accept_in_order, first_accept_in_order_sync};

// Async: Ok(r) short-circuits to Ok(Some(r)); Err(e) continues unless
// stop(&e); exhaustion yields Ok(None) or the last Err.
let quote = first_accept_in_order(sources, |s| async move { s.quote().await }, |_| false).await?;

// Sync twin for non-async callers (protopyne backtest source fallback).
let bars = first_accept_in_order_sync(paths, |p| load(p), |e| e.is_fatal())?;
```

---

## 5. Structured concurrency with `Scope`

`Scope` is `#[must_use]` and owns a `JoinSet`: every spawned task is joined
or aborted on close. Dropping without closing **panics** (suppressed during
an unwind, where it logs and aborts instead).

```rust,ignore
use fluent_concurrency::scope::Scope;

# async fn example() {
// Option 1: explicit close (async; aborts then drains)
let mut scope = Scope::new();
scope.spawn(async { /* task A */ });
scope.spawn(async { /* task B */ });
scope.close().await;

// Option 2: graceful close with a deadline, then abort stragglers
let mut scope = Scope::new();
scope.spawn(async { /* task C */ });
scope.close_graceful(std::time::Duration::from_secs(5)).await;

// Option 3: defer guard — Drop calls close_sync() (never close().await)
let mut scope = Scope::new();
scope.spawn(async { /* task D */ });
let _guard = scope.defer();
# }
```

- `Scope::spawn` captures the current `CURRENT_CAPS` and reinstalls it in
  the child — the load-bearing half of capability propagation (§10).
- `close_sync()` is the `Drop`-safe variant: aborts and swaps the `JoinSet`
  without awaiting. `defer()` returns a guard that calls it.
- In-tree shape: `protopyne-server` owns its feed pump in a `Scope`; the
  trading session owns feed tasks the same way and closes on shutdown.

---

## 6. Build a JSON-RPC handler

Implement `JsonRpcHandler` (it is `Send + Sync`) and drive it with
`serve_stdio`:

```rust,ignore
use common_core::{JsonRpcHandler, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, method_not_found, IoError};

struct MyHandler;

impl JsonRpcHandler for MyHandler {
    fn handle_request(&self, raw: &str) -> Result<String, JsonRpcError> {
        let req: JsonRpcRequest = serde_json::from_str(raw)
            .map_err(|e| JsonRpcError { code: -32700, message: format!("parse: {e}") })?;

        match req.method.as_str() {
            "ping" => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: req.id,
                    result: Some(serde_json::json!("pong")),
                    error: None,
                };
                Ok(serde_json::to_string(&resp).unwrap())
            }
            _ => Ok(serde_json::to_string(
                &method_not_found(req.id, &req.method),
            ).unwrap()),
        }
    }
}

fn main() -> Result<(), IoError> {
    serve_stdio(&MyHandler)
}
```

---

## 7. Pass data between units: `WorkContext` channels and `OutputStore`

A `WorkContext` carries four channels — pick the narrowest that fits:

| Channel | Purpose |
|---|---|
| `outputs: OutputStore` (typed `HashMap<String, Arc<dyn Any>>`) | **Primary** in-process handoff. `ctx.set::<T>(key, v)` / `ctx.get::<T>(key)`. Clone shares arcs (zero-copy fan-out). No serialize/deserialize. |
| `structured: HashMap<String, serde_json::Value>` | JSON handoff whose type is unknown at the write site or crosses crate boundaries as `Value` only. `set_structured` / `try_set_structured` serialize; `structured::<T>()` / `structured_ref()` read. |
| `WorkOutput.data: serde_json::Value` | **Only** for serialization boundaries (WASM, network, final unit return). `typed::<T>()` / `typed_lossy` / `data_as` / `data_ref` / `data_take`. |
| `metadata: HashMap<String, MetadataValue>` | Genuinely stringly-typed annotations (`String`/`i64`/`f64`/`bool`/`Null`). |

```rust,ignore
use fluent_wvr::prelude::*;

// Typed handoff: compiler-checked, no JSON round-trip.
ctx.set("attempt", 2_i32);
assert_eq!(ctx.get::<i32>("attempt"), Some(&2));

// Structured handoff for Value-shaped payloads (e.g. RouterRequest).
ctx.set_structured("request", &req);
let req: RouterRequest = ctx.structured("request")?;

// Unit return crossing a boundary.
let out = WorkOutput::typed("ok", &payload)?;
let back: Payload = out.data_as()?;
```

Construct with `WorkContext::default()` (dry-run/init),
`WorkContext::for_unit(&unit, caps)`, or
`WorkContext::for_unit_in_batch(batch_rt, batch_caps, |ctx| ...)` inside a
batch. `WorkOutput::{ok, ok_with_data, fail}` are the plain constructors;
`Display` renders `OK:`/`FAIL:` lines.

---

## 8. Adapt and compose at runtime

### `ComponentAdapter` — runtime overrides on an erased handle

```rust,ignore
use std::sync::Arc;
use fluent_wvr::prelude::*;

let adapted: Arc<dyn Component> = Arc::new(
    ComponentAdapter::new(wasm_unit)
        .with_name_override("custom_name")
        .with_execute_override(Arc::new(|_ctx| {
            Ok(WorkOutput::ok("overridden by adapter"))
        }))
        .with_field_override("port", "8080"),
);
```

- `with_execute_override` takes an `ExecuteFn` (`Arc::new(|ctx| ...)`), not a
  bare closure — the adapter is shared across threads.
- `set_field` validates against the inner first (when exclusively owned);
  on rejection the override is **not** stored. `get_field` checks overrides,
  then falls through. `field_names` always delegates to the inner.
- `describe` merges the inner schema plus `name`/`field_overrides` and sets
  `"adapted": true`. In-tree shape: `protopyne-kernels` builds test doubles
  and `protopyne-server` wraps boot-gate units this way.

### `ComponentCascade` — first-Ok-wins dispatch

```rust,ignore
let cascade = ComponentCascade::with_units(vec![Arc::new(l1), Arc::new(l2)]);
let output = cascade.execute_first_ok(&ctx)?; // first Ok short-circuits
```

`Err` advances; exhaustion returns the last `Err` (empty cascade errors
descriptively). The cascade is itself a `Component` (`component_cascade`),
so it nests and registers like any unit.

### `MiddlewareChain` — post-erasure cross-cutting

Prefer newtype wrappers **before** erasure (inlinable); use middleware after.

```rust,ignore
let wrapped: Arc<dyn Component> = MiddlewareChain::new()
    .push(Box::new(TimingMiddleware::new()))
    .apply(unit);
```

Reuse `Instrumented` inside middleware — never a parallel wrapper type.

### `Pipeline` — sequential fallible steps over `&mut T`

```rust,ignore
use fluent_wvr::wrapper::Pipeline;

let mut pipeline = Pipeline::new()
    .step(|cfg: &mut Config| -> Result<(), ConfigError> { cfg.validate()?; Ok(()) })
    .maybe(|cfg| cfg.env == "prod", |cfg| -> Result<(), ConfigError> { cfg.enforce_tls()?; Ok(()) });
pipeline.run(&mut cfg)?;
```

### `SuffixedComponent` — scatter-gather shards

Wraps a `Component` with a `name:suffix` identity and merges the batch
runtime with per-task caps/metadata (the suffix also travels as
`metadata["task_suffix"]`). Used when one component type registers N times
under a batch.

---

## 9. Observe with `Instrumented`

```rust,ignore
use std::sync::Arc;
use common_core::LatencyHistogram;
use fluent_wvr::wrapper::Instrumented;

// Tracing only:
let unit = Instrumented::new(my_component, "ingest");
// Tracing + shared lock-free histogram (p50/p99/count/sum):
let hist = Arc::new(LatencyHistogram::new());
let unit = Instrumented::with_metrics(my_component, "ingest", hist.clone());
```

`Instrumented` delegates `name`/`depends`/`provides`/`type_name`,
`FieldAccess`, and `Describable` to the inner — it adds observability only.
Wrap **before** type erasure so the compiler can inline. In-tree shape:
`protopyne-kernels::timed::instrumented_kernel` wraps every kernel evaluation
before supervision.

---

## 10. Gate effects with capabilities

Every high-overhead effect requires a token in the current `CURRENT_CAPS`
task-local (a `tokio::task_local!` owned by `fluent-wvr`, re-exported by
`fluent-concurrency::scope`). Absence fails closed with `PermissionDenied` —
never a silent default.

```rust,ignore
use fluent_wvr::{Capability, CapabilitySet, FsCapability, check_capability};

// Install at the Scope/SupervisedBatch boundary; children inherit it.
let caps = CapabilitySet::new().with(FsCapability::new());
let contains: bool = caps.contains::<FsCapability>();

// Effect entry points check first:
check_capability(&FsCapability::new())?;

// Sync serving-path fs without re-implementing the gate:
fluent_wvr::capability::capability_aware_fs::read_to_string(path)?;
```

- `CapabilitySet::{with, get, contains, remove, remove_as, iter, len,
  is_empty}` is a `TypeId`-keyed map; `name()` is informational only.
- `CapabilityError::{Missing, Exhausted}` maps into `io::Error`
  (`PermissionDenied`).
- Async file ops live on `FsCapability::{read, write, metadata}` (tokio);
  network on `NetCapability::{http_get, http_post, http_post_json_stream,
  tcp_connect}`; SQLite on `DbCapability::open` (`fluent-db`, gated
  `acquire`). `default_capability_set()` pre-populates fs + net;
  `capability_set_with_db(path)` adds the DB token.
- Operator CLI tooling is capability-exempt by design; the serving path is
  always gated.

---

## 11. Decode boundary strings with `boundary` + `coerce`

Data from outside the process arrives as strings. The boundary walker repairs
structural damage (trailing commas, unquoted keys, single quotes, comments,
bare literals, unterminated strings) and coerces members through `set_field`:

```rust,ignore
use fluent_wvr::{decode_boundary_typed, BoundaryOptions, FieldAccess, Describable};

#[derive(Default, FieldAccess, Describable)]
struct ToolArgs { action: String, retries: u32 }

// Schema-driven: field_names() drives member recognition, set_field coerces.
let (args, decoded): (ToolArgs, Vec<String>) = decode_boundary_typed(raw)?;

// Lower-level faces: repair only, or extract (name, value-string) members.
let repair = fluent_wvr::repair_boundary(raw, &BoundaryOptions::lenient());
let members = fluent_wvr::extract_members(raw, &BoundaryOptions::for_schema(ToolArgs::default().field_names()))?;

// Shared string vocabulary (also behind #[field(coerce = "...")]).
let v = fluent_wvr::coerce(raw, &[fluent_wvr::Coercion::Trim, fluent_wvr::Coercion::StripQuotes]);
let n: Option<f64> = fluent_wvr::parse_number(raw);
let b: Option<bool> = fluent_wvr::parse_bool(raw);
let parts: Vec<String> = fluent_wvr::split_top_level(raw, ',');
```

Members that fail to coerce keep their `Default` and are skipped (debug-logged);
decoding errors only when nothing decodes. `BoundaryOptions::for_schema`
restricts recognition to declared fields; the schema-blind `lenient()` mode is
pure structural repair.

---

## 12. Macros and small helpers

- `impl_component!(T)` / `impl_component!(generic (U: Component + 'static)
  for Wrapper<U>)` — the only way to satisfy `Component` (no blanket impl;
  it would collide with `Arc<dyn Component>`). Never hand-roll `as_any`.
- `impl_fieldless!(T)` (+ generic arm) — `FieldAccess` no-op trio for units
  with no configurable fields (`ComponentCascade` uses it).
- `describe_work_unit(name, depends, provides, purity)` — canonical
  `Describable` document for stages; replaces hand-written JSON literals.
- `component_downcast_ref::<T>` / `component_downcast_mut::<T>` /
  `ComponentArcExt::try_as_any_mut()` — safe downcasts after erasure.
- `FieldSchema` / `SchemaProvider` — compatibility scaffold (derive-only,
  never invoked in production); do not prune as dead code.
- `DynamicComponent` — compatibility scaffold for DB/HTTP-assembled configs
  (`new(name, executor).with_field_keys(...).with_config(rows)`); keeps the
  `Component` interface uniform with no production consumer yet. In-tree
  reference: `protopyne-kernels::component` documents runtime-assembled
  handles composing it.
- `retry_call(max_attempts, base_ms, f)` — sync, explicitly-blocking retry
  with `common_core::retry::backoff_ms` schedule. Never call from
  `WorkUnit::execute`; async transport retry is
  `common_core::retry::retry_async`, supervised retry is the batch.
- `NoopRuntime` / `TokioRuntime` / `TestRuntime` (`Runtime` trait: `spawn`,
  `sleep`, `now`) — production, paused-time record-replay, and init/dry-run
  backends. Construct production via `fluent_concurrency::tokio_runtime()`.

---

## 13. The prelude imports

```rust
use common_core::prelude::*;
```

Provides the 80% case: `backoff_ms`, `capped_backoff_ms`,
`capped_exp_backoff_ms`, `blake3_hex`, `sha256_hex`, `fnv1a64`, `hex_encode`,
`format_size`, `format_json`, `LatencyHistogram`, `ensure_dir`,
`ensure_dir_or_panic`, `write_atomic`, `read_to_string_err`, `lock`,
`lock_read`, `lock_write`, `now_secs`, `LoadCache`, `method_not_found`,
`JsonRpcError`, `JsonRpcHandler`, `JsonRpcRequest`, `JsonRpcResponse`,
`IoError`. Token budgets live in `fluent_llm::tokens`, not here; async
`retry_async` lives in `fluent_concurrency::retry`.

```rust
use fluent_wvr::prelude::*;
```

Provides: `Component`, `WorkUnit`, `FieldAccess`, `FieldError`, `Describable`,
`describe_work_unit`, `WorkContext`, `WorkError`, `WorkOutput`, `OutputStore`,
`Capability`, `CapabilitySet`, `MetadataValue`, `DynamicComponent`,
`DynamicExecutor`, `impl_component`, `impl_fieldless`, `retry_call`,
`ComponentAdapter`, `ComponentCascade`, `ExecuteFn`, `Instrumented`,
`Middleware`, `MiddlewareChain`, `Pipeline`, `SuffixedComponent`,
`ArcIntern`, plus the boundary helpers (`decode_boundary`,
`decode_boundary_typed`, `extract_members`, `repair_boundary`,
`BoundaryError`, `BoundaryOptions`). The `coerce` vocabulary (`Coercion`,
`coerce`, `parse_number`/`parse_int`/`parse_bool`, `split_top_level`,
`strip_outer_quotes`, `escape_control_chars`) is re-exported from the
`fluent_wvr` crate root rather than the prelude.
`Runtime`/`NoopRuntime` import from `fluent_wvr` directly; `TokioRuntime` /
`tokio_runtime()` / `StreamAbort` / pools / `Scope` / `SupervisedBatch` come
from `fluent_concurrency`.

---

## Quick reference

| What you want | Use |
|---|---|
| Define a component | `#[derive(FieldAccess, Describable)]` + `impl WorkUnit` + `impl_component!(T)` |
| Fieldless unit | `impl_fieldless!(T)` |
| Atomic file write | `common_core::io::write_atomic(path, data)` |
| Read file with size cap | `common_core::io::read_to_string_err(path)` |
| Content hash | `common_core::hash::blake3_hex(data)` |
| Shared HTTP client | `common_core::http::shared_http_client()` |
| Typed in-process handoff | `ctx.set::<T>(k, v)` / `ctx.get::<T>(k)` (`OutputStore`) |
| JSON handoff across crates | `ctx.set_structured` / `ctx.structured::<T>()` |
| Boundary decode | `decode_boundary_typed::<T>(raw)` |
| String shaping | `coerce` + `Coercion::{Trim, Lowercase, StripQuotes, JsonEscape, NormalizeLiteral}` |
| Concurrency cap | `Limiter::new(3).run(\|\| async { ... }).await` |
| Sync caller bridge | `Limiter::run_sync` (control-plane only) |
| Background workers | `WorkerPool::new(rt, cap, queue, handler)` |
| Results from workers | `ResultPool::new(rt, cap, queue, handler)` + `submit_with_abort` |
| Priority dispatch | `PriorityResultPool` / `AffinityScheduler` |
| Credit-bounded producer | `CreditGatedPool` or `flow::new(CreditSpec { initial, more_after })` |
| First-Ok fallback | `first_accept_in_order` / `first_accept_in_order_sync` |
| Structured concurrency | `Scope::new()` + `spawn` + `close().await` / `close_graceful` / `defer()` (calls `close_sync`) |
| Supervised DAG | `SupervisedBatch::new(rt, caps)` + `register` (`Result`) + `await` |
| Runtime adaptation | `ComponentAdapter::new(h).with_name_override(..).with_field_override(..)` |
| First-Ok dispatch | `ComponentCascade::with_units(..)` + `execute_first_ok` |
| Post-erasure wrap | `MiddlewareChain::new().push(..).apply(h)` |
| Sequential steps | `Pipeline::new().step(..).maybe(..)` |
| Per-unit timing | `Instrumented::new` / `with_metrics(h, label, hist)` |
| Gated effects | `CapabilitySet::new().with(FsCapability::new())` + `check_capability` |
| JSON-RPC over STDIO | `impl JsonRpcHandler` (`Send + Sync`) + `serve_stdio(&handler)` |
| Latency metrics | `LatencyHistogram::new()` + `observe_duration(start)` |
| Sync retry (non-execute) | `retry_call(max, base_ms, \|\| ...)` |
| Per-thread reuse | `thread_local_resource!` + `with_tlr` |
