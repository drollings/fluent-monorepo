# ADR-003: Wrapper timeout delegation (H6)

## Context
`Instrumented` and `ComponentAdapter` dropped `default_timeout_ms` (reverted to 30s). `SuffixedComponent` and `Arc` delegated correctly.

## Decision
Delegate `default_timeout_ms` (and existing `type_name`/`depends`/`provides`) through both wrappers. Additive, non-breaking except timeout now honors inner (intended).

## Downcast
`as_any` returns wrapper by design; add `inner()` accessor where missing (`Instrumented` has none today) or `unwrap_chain()` helper. Proposal only; no code yet. `ComponentAdapter::inner()` exists; add `Instrumented::inner()` + docs.

## Migration
Timeouts for wrapped units may shorten from 30s to inner override (e.g. 5s). SupervisedBatch contexts using `for_unit` pick up inner timeout automatically.
