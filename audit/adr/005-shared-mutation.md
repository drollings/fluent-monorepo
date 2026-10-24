# ADR-005: Shared-mutation model (H1/H11) — proposal, no code

## Context
`Arc<dyn Component>::set_field` → `ReadOnly` when shared. `ComponentAdapter::set_field` stores override without validation when shared → `get_field`/`execute` diverge (H1 confirmed S1).

## Options
- (A) `&self` interior mutability everywhere. Breaks `FieldAccess` signature, large churn.
- (B) `ConfigurableShared` trait (`set_field_shared(&self)`). Additive, wrappers implement; old `set_field` stays exclusive.
- (C) Clone-and-modify (`with_field(&self) -> Result<Arc<dyn Component>>` + `clone_box`). No mutation, fits `Arc`; cost: clone per write.
- (D) Immutable config + `ArcSwap` handle. Cleanest for hot reads, but new dependency + API.

## Recommendation
Prototype (B) + (C): add `try_set_shared` fail-closed today (return `ReadOnly`, do not store) to remove divergence, then (C) as non-breaking additive API. (A)/(D) deferred to major.

## Immediate
Do NOT change adapter to store silently (current docs intent). Keep characterization test; desired `Err` test stays ignored pending maintainer answer (FINDINGS Q1).
