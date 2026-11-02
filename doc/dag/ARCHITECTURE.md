# fluent-dag — Architecture

> Authoritative API reference: `doc/skills/dag/SKILL.md`. Every type
> named below exists in `src/dag/src/`; capabilities are interned
> through `common_core::interner::CapabilityRegistry` (shared, not
> reimplemented here).

## Module table

| Module | Purpose | Path |
|--------|---------|------|
| `dep_graph` | `DependencyGraph<K>` — register/resolve/dependents, `topo_sort[_from]`, `unresolved_deps`, `ready_nodes` | `src/dag/src/dep_graph.rs` |
| `closure` | Transitive-closure DFS shared by the resolver | `src/dag/src/closure.rs` |
| `resolver` | `DependencyResolver::{new, with_narrowing}`, `ProviderSelection::{All, NarrowOne}`, `ExecutionPlan` (order + names), `ResolverError::{MissingDependency, AmbiguousDependency{name, candidates}, TargetNotFound, CircularDependency}` | `src/dag/src/resolver.rs` |
| `narrowing` | Four-stage provider narrowing (essential → strict-satisfaction → locality → no-dep) + rejected-set durability guard | `src/dag/src/narrowing.rs` |
| `target` | `TargetRegistry`, `Target`, abstract vs concrete targets, self-provision model | `src/dag/src/target.rs` |
| `yamake_loader` | `load_yamake_config` — JSON workflow targets into registry + caps | `src/dag/src/yamake_loader.rs` |
| `hierarchy` | `HierarchyIndex` — DAG-backed ancestors/is-subclass for taxonomies | `src/dag/src/hierarchy.rs` |
| `checkpointed` | `CheckpointedStepGraph::{add_step, checkpoint, complete, ready_steps, rewind_to, cancel_dependents}` resumable pipelines | `src/dag/src/checkpointed.rs` |
| `target_work_unit` | `TargetWorkUnit::from_target` — Target → supervised-unit bridge | `src/dag/src/target_work_unit.rs` |
| `work_unit` / `adapter` / `middleware` / `wvr` | `WorkUnit` adapters and middleware for DAG nodes | `src/dag/src/work_unit.rs`, `adapter.rs`, `middleware.rs`, `wvr.rs` |
| `type_inference` / `version` / `error` | Target type inference, versioned imports, `GraphError` taxonomy | `src/dag/src/type_inference.rs`, `version.rs`, `error.rs` |

## Load-bearing invariants

- `NarrowOne` without a `CapabilityRegistry` is a typed error, never a panic.
- Narrowing losers are recorded in a rejected set; the final closure skips them (no include-all leak through alternate paths).
- Deterministic output: candidates sort by name before filtering.
- Fast path: zero contested capabilities delegates straight to Kahn's.
