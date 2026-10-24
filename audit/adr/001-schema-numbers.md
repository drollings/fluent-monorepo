# ADR-001: Schema numeric bounds as JSON numbers (H2)

## Context
`describe()` emitted `"minimum": "1"` (string) not `1` (number). Fails JSON Schema meta-schema validation. Same for `maximum`, `maxLength`.

## Options
- (a) Emit numbers (chosen). Breaking for string consumers, but strings were never valid.
- (b) Keep strings + document as `x-` extension. Rejected: invalid vs spec.
- (c) Feature flag. Rejected: no need, no in-tree string consumers found.

## Decision
Emit `minimum`/`maximum` as JSON numbers (`f64_unsuffixed`), `maxLength` as integer. `Option<T>` inner type already named number correctly via `strip_option_wrapper` (pre-existing).

## Migration
Consumers parsing `minimum` as string must accept number. `FieldSchema::{min,max}` (typed `Option<f64>`) unchanged.

## Risks
External validators expecting strings break; announce in changelog.
