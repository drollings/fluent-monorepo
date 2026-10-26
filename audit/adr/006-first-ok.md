# ADR-006: First-Ok semantics (H8) — proposal, no code

## Context
`ComponentCascade::execute_first_ok` advances only on `Err`; `Ok(fail)` (success=false) short-circuits. Docs say `Err`/`fail` advances. `first_accept_in_order` has `Ok(None)` skip + `stop` predicate; cascade has neither.

## Options
- (A) Fix code: treat `!success` as continue. Breaking for callers relying on `Ok(fail)` win.
- (B) Fix docs: cascade is `Err`-only; `fail` is success. Non-breaking, matches code.
- (C) Express cascade over `first_accept_in_order_sync` with `Ok(fail)→Ok(None)` mapping + `stop=|_|false`. Requires (A) choice.

## Recommendation
(B) now (docs match code), (A)/(C) only with second consumer + major bump. Add equivalence table to docs.

## Outcome (implemented)

(C) landed: `ComponentCascade::execute_first_ok` now composes
`first_accept_in_order_sync` (`src/fluent-wvr/src/wrapper.rs`), mapping
`Ok`+`!is_accept` to `Ok(None)` with `stop = |_| false`. Behavior is
fail-advance: an `Ok` failure verdict advances and the last one is
returned on total failure; a failure verdict never becomes
`WorkError::Execution`. The ladder stays the single error-walk
implementation. Equivalence is pinned by `tests/audit_h8.rs` (cascade
side) and `src/fluent-concurrency/tests/ladder.rs` (ladder side); the
cascade doc-comment is the equivalence record. No second consumer
appeared, but the change was contained (one function body) and covered
by the pre-existing acceptance corpus (`tests/cascade_acceptance.rs`),
so it shipped without a major bump.
