# ADR-002: Strict native integer parse (H3) + Option guards (H4)

## Context
Constrained integers parsed via `f64` then `as T`: `"1.9"` truncated to `1`, `2^53+1` lost precision, `"1e3"` accepted. `Option<u32>` bounds and `Option<String>` guards silently dropped.

## Decision
- Bare/Option integers: strict `str::parse::<T>()` first. On failure, consult `f64` only to choose `Constraint` (out-of-range) vs `Parse` (gibberish/fraction), preserving the `"65536"` → `Constraint` expectation.
- `parse="number"` tolerant path: `parse_number` + `fract()==0` check for integers.
- `Option<T>` inherits inner bounds/guards; `Option<String>` applies sanitize/max_len/pattern to `Some`.
- Clamp keeps f64 round-trip with fract check so `"-5"` (u32) still clamps to min.

## Alternatives
- Type-native parse + native bounds only (rejected for `"65536"` compat; fallback preserves helpful Constraint).
- Keep f64 (rejected: silent corruption S1).

## Migration
`"1.9"`/`"1e3"` for integer fields now `Parse` (was `Ok`). `Option` out-of-range now `Constraint` (was `Ok`). Update callers relying on truncation.
