# fluent-wvr — Architecture

> Pointer to `doc/skills/fluent-wvr/SKILL.md` (authoritative). Every type
> named below exists in `src/fluent-wvr/src/`; the stale-name guard
> (`src/fluent-wvr/tests/doc_stubs.rs`) fails on invented names.

## Module table (from the SKILL, verified against source)

| Module | Purpose | Path |
|--------|---------|------|
| `traits` | `Component`, `WorkUnit`, `FieldAccess`, `Describable`, `FieldError`, downcast helpers | `src/fluent-wvr/src/traits.rs` |
| `work` | `WorkContext` (four channels: `outputs` typed, `structured` JSON, `data` boundary-only, `metadata` dynamic), `WorkError`, `WorkOutput` | `src/fluent-wvr/src/work.rs` |
| `wrapper` | `Instrumented`, `ComponentAdapter`, `ComponentCascade`, `MiddlewareChain`, `Pipeline`, `retry_call` | `src/fluent-wvr/src/wrapper.rs` |
| `capability` | `Capability`, `CapabilitySet`, `CURRENT_CAPS`, `check_capability`, `FsCapability` | `src/fluent-wvr/src/capability.rs` |
| `ladder` | `first_accept_in_order` / `first_accept_in_order_sync` plus the infallible-`Option` specialization `first_some_in_order_sync` (no second walker anywhere) | `src/fluent-wvr/src/ladder.rs` |
| `runtime` | `Runtime`, `NoopRuntime` | `src/fluent-wvr/src/runtime.rs` |
| `store` | `OutputStore` typed handoff accumulator | `src/fluent-wvr/src/store.rs` |
| `boundary` | `repair_boundary`, `decode_boundary` tolerant LLM-output codec | `src/fluent-wvr/src/boundary.rs` |

## Load-bearing invariants

- `WorkUnit::execute` is synchronous, non-blocking, and side-effect free
  beyond its return value; the supervisor owns retry/backoff/timeout.
- `set_field` takes `&mut self`: configure before `Arc`-wrapping, or use
  interior mutability inside the implementation.
- `field_names` and `describe` are `&self` instance methods (object-safe
  through `dyn Component`); never `static fn`s.
