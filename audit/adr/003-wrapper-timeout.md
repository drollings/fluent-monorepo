# ADR-003: Wrapper timeout delegation (H6)

## Context
`Instrumented` and `ComponentAdapter` dropped `default_timeout_ms` (reverted to 30s). `SuffixedComponent` and `Arc` delegated correctly.

## Decision
Delegate `default_timeout_ms` (and existing `type_name`/`depends`/`provides`) through both wrappers. Additive, non-breaking except timeout now honors inner (intended).

## Downcast
`as_any` returns the wrapper by design; one layer peels at a time via
`inner()`. `ComponentAdapter::inner()`, `SuffixedComponent::inner()`, and
`Instrumented::inner()` (added with this change, with a doc-test showing
two-hop peeling) all exist. No forwarded auto-downcast: it would hide
which layer answered. Done — no `unwrap_chain()` helper; one-hop
`inner()` accessors plus `component_downcast_ref` are the API.

## Migration
Timeouts for wrapped units may shorten from 30s to inner override (e.g. 5s). SupervisedBatch contexts using `for_unit` pick up inner timeout automatically.
