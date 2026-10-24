# ADR-006: First-Ok semantics (H8) — proposal, no code

## Context
`ComponentCascade::execute_first_ok` advances only on `Err`; `Ok(fail)` (success=false) short-circuits. Docs say `Err`/`fail` advances. `first_accept_in_order` has `Ok(None)` skip + `stop` predicate; cascade has neither.

## Options
- (A) Fix code: treat `!success` as continue. Breaking for callers relying on `Ok(fail)` win.
- (B) Fix docs: cascade is `Err`-only; `fail` is success. Non-breaking, matches code.
- (C) Express cascade over `first_accept_in_order_sync` with `Ok(fail)→Ok(None)` mapping + `stop=|_|false`. Requires (A) choice.

## Recommendation
(B) now (docs match code), (A)/(C) only with second consumer + major bump. Add equivalence table to docs.
