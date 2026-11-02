# fluent-wvr — Vision

> Status: Implemented. The authoritative contract lives in
> `doc/skills/fluent-wvr/SKILL.md` (twelve composable control-plane
> patterns); this file is a pointer.

## Mission

`fluent-wvr` is the workspace control plane: one compiler-checked interface
(`Component` = `FieldAccess` + `Describable` + `WorkUnit` + downcast) behind
which a native struct, a WASM plugin, or a database-driven config can all be
driven uniformly by any orchestrator. Construction reads like configuration
(`bon` builders), validation surfaces once, dispatch is uniform (`Arc<dyn
Component>`), and cross-cutting concerns (`Instrumented` timing,
`first_accept_in_order` fallback) compose without touching business logic.

## What belongs / what does not belong

Belongs: `Component`, `WorkUnit`, `FieldAccess`, `Describable` traits;
`WorkContext`, `WorkError`, `WorkOutput`, `OutputStore` channel types;
`CapabilitySet` + `CURRENT_CAPS` task-local gating; `Runtime` trait;
`Instrumented` / `ComponentAdapter` / `ComponentCascade` wrappers;
`first_accept_in_order` canonical fallback combinator; the
`FieldAccess`/`Describable` derive macros (`fluent-wvr-macros`).

Does not belong: domain logic, I/O drivers, retry math (owned by
`common-core::retry`), graph algorithms (owned by `fluent-dag`). Thin
trait boundaries only — no `rusqlite`, no LLM, no `fluent-types` deps.

## Consumers (verified `Cargo.toml` dependents)

`fluent-dag`, `fluent-concurrency`, `fluent-db`, `fluent-router`,
`guidance-core`, `coral-context`, `fluent-llm`, `spacy-rs`.

## End-to-end capabilities

An orchestrator needing uniform control over heterogeneous units —
native structs, WASM plugins, database-driven configs — gets one
compiler-checked interface: build units as configuration, validate
once, dispatch uniformly, gate effects by capability, and compose
timing and ordered-fallback wrappers without touching business
logic.

## Non-goals

- No blanket `impl<T> Component for T` (would collide with the
  `Arc<dyn Component>` impl; `impl_component!` is the mechanism).
- No persistence trait on `Component` until a second consumer needs it.
- `dyn` at the request boundary, concrete types in the tight loop — never
  a vtable per element in bulk paths.
