# Audit Findings — fluent-wvr / fluent-concurrency

Baseline: `make fluent-wvr-smoke` (50 passed), `make fluent-wvr-test` (182 passed, 1 ignored),
`make fluent-concurrency-smoke` (186 passed), both lints clean. No pre-existing failures owned by audit.

Branch: `audit/fluent-wvr-hypotheses`. First commit: repro + characterization tests with
`#[ignore = "AUDIT-Hn: ..."]` desired tests.

## Summary table

| ID | Verdict | Severity | Evidence |
|----|---------|----------|----------|
| H1 shared-handle mutation | CONFIRMED | S1 | `src/fluent-wvr/tests/audit_h1.rs`, `src/fluent-wvr/src/wrapper.rs:299-306`, `src/fluent-wvr/src/lib.rs:111-128` |
| H2 schema fidelity | CONFIRMED (partial refute on `required`) | S1 (strings), S2 (pattern/version/clamp) | `src/fluent-wvr/tests/audit_h2.rs`, `src/fluent-wvr-macros/src/lib.rs:769-786` |
| H3 f64 parsing | CONFIRMED | S1 | `src/fluent-wvr/tests/audit_h3.rs`, `src/fluent-wvr-macros/src/lib.rs:402-408,519-566` |
| H4 non-scalar coverage | CONFIRMED (Option guards dropped); PARTIAL (compile-fail clarity) | S1 (silent drop), S2 (messages) | `src/fluent-wvr/tests/audit_h4.rs`, `src/fluent-wvr-macros/src/lib.rs:511,423-424` |
| H5 sync timeout | CONFIRMED | S1 | `src/fluent-concurrency/src/audit_tests.rs:h5_*`, `src/fluent-concurrency/src/batch.rs:389-431` |
| H6 wrapper delegation | CONFIRMED | S2 | `src/fluent-wvr/tests/audit_h6.rs`, `src/fluent-wvr/src/wrapper.rs:119-146,272-297` |
| H7 blanket impl | PARTIAL (terminology drift CONFIRMED; prototype no-go pending scratch compile) | S3 | `src/fluent-wvr/src/lib.rs:136`, `doc/skills/fluent-wvr/SKILL.md:289-291,1989-1994` |
| H8 first-Ok overlap | CONFIRMED | S2 | `src/fluent-wvr/tests/audit_h8.rs`, `src/fluent-wvr/src/wrapper.rs:386-396`, `src/fluent-concurrency/src/ladder.rs:40-114` |
| H9 docs drift | CONFIRMED | S3 | SKILL.md:117,1989-1994,796,633-688 vs code |
| H10 typed descriptor | INCONCLUSIVE (design only, no prototype yet) | — | needs ADR |
| H11 shared-mutation design | INCONCLUSIVE (needs ADR after H1) | — | needs ADR |
| H12 cost claims | INCONCLUSIVE (no benchmarks run) | — | needs criterion |
| H13 boundary decoder | PARTIAL (no-panic/identity/idempotence hold on samples; quote-retention + unknown-member behavior need spec) | S2 | `src/fluent-wvr/tests/audit_h13.rs`, `src/fluent-wvr/src/boundary.rs:178-456`, `src/fluent-wvr/src/coerce.rs` |
| H14 ecosystem fit | INCONCLUSIVE (memo not written) | — | needs memo |

Severity: S1 silent wrong behavior / data corruption, S2 surprising but detectable, S3 docs/ergonomics.

## H1 — Shared-handle mutation is inconsistent

Tested:
- Exclusive `set_field` on derive struct validates (min/max) — `audit_h1::h1a_exclusive_set_field_validates` passes.
- Shared `Arc<dyn Component>` `set_field` returns `FieldError::ReadOnly` — `h1a_shared_arc_set_field_is_readonly` passes (`lib.rs:111-128`).
- `ComponentAdapter::set_field` over shared inner (strong_count≥2): returns `Ok` and stores override without validation — `h1b_adapter_shared_stores_invalid_without_validation_characterization` passes. Desired `Err` test ignored.
- Divergence pinned: `adapter.get_field("port")=="0"` while `adapter.inner().get_field=="8079"` and `execute()` reports `8079` (`audit_h1.rs`).

Verdict CONFIRMED, S1. Divergence reachable by cloning the inner `Arc` before wrapping.

Docs (`SKILL.md:1642`) say “validates against the inner first, then stores” and “if `Arc::get_mut` returns `None`, the override is stored silently” — code matches docs (`wrapper.rs:299-306`), but behavior is a footgun: `get_field`/`describe` see override, `execute`/inner see old value.

Questions for maintainers: is silent-store-while-shared intentional scaffold (per operating rule 3) or bug? Safest fix is fail-closed: return `ReadOnly` or validate via cloned inner without mutating? See ADR needed (H11).

## H2 — describe() fidelity lossy

- `minimum`/`maximum` emitted as JSON strings: `macros/src/lib.rs:769-776` (`let min_str = format!("{}", min); schema.push(quote!{"minimum": #min_str})`). Pinned `audit_h2::h2_minimum_maximum_are_numbers_desired` (strings). Desired numbers test ignored. Same for `maxLength` (`:783-786`). CONFIRMED S1 — fails JSON Schema meta-schema validation.
- `required` respects `required=false` and `Option<T>` (`macros:762,877-879`): `audit_h2::h2_required_respects_option_and_flag` passes. The “ignores” sub-claim is REFUTED.
- `pattern` is substring (`contains` in `macros:504`) but schema emits `"pattern"` (JSON Schema regex slot). Collision CONFIRMED S2.
- `format` emitted correctly as string; `max_len`/`sanitize`/`coerce`/`parse` emitted as custom keywords (not JSON Schema standard) — dropped vs mis-emitted debate; at least `maxLength` type wrong.
- `version_added`/`version_removed` documented in SKILL §19 (`SKILL.md:2265,2269`) have no derive arm; `parse_nested_meta` error swallowed by `let _ =` (`macros:48`) so silently ignored. CONFIRMED S2.
- `skip` correctly excluded (`macros:749`, `audit_h2::h2_skip_excluded`). `clamp` affects `set_field` but leaves no schema trace (`audit_h2::h2_clamp_not_in_schema`). S2 (should be documented or emitted as `x-clamp`).

## H3 — Numeric parsing through f64

Constrained integers (`min`/`max` present) parse via `let wide: f64 = value.parse()` then `wide as T` (`macros:403-407,519-566`). Unconstrained go via `value.parse::<T>()` (strict, correct).

- `"1.9"` → `Ok(1)` truncation for constrained `u16`; strict oracle `Err`. Pinned `audit_h3::h3_fraction_truncates_silently`. CONFIRMED S1.
- `2^53+1` precision loss for `u64` (`audit_h3::h3_large_u64`). CONFIRMED.
- `i64::MIN` round-trip via f64 lossy (`audit_h3::h3_i64_min`). CONFIRMED shape (exact value depends on `as` saturation; test pins f64 round-trip).
- `"1e3"` accepted via f64 for constrained path but rejected by strict oracle (`audit_h3::h3_scientific`). Divergence CONFIRMED.
- Whitespace `" 5 "`: strict `parse` rejects; f64 path? `wide_init` uses `value.parse::<f64>` without trim (except coerce) — also rejects, but `parse_number` tolerant path (with `parse="number"`) trims. Oracle table in test file.

Recommendation (no code yet): type-native parse first, `f64` only where `parse="number"` requested; reject fractions for ints; use `parse_int`/`checked` semantics.

## H4 — Derive coverage

- `Option<u32>` with `min`/`max`: bounds silently dropped because `is_numeric_type(&ty_str)` tests `"Option<u32>"` (false) (`macros:511`). Pinned `audit_h4::h4_option_numeric_bounds_dropped` (999 accepted, schema still shows bounds → schema/validation diverge). CONFIRMED S1.
- `Option<String>` with `max_len`/`pattern`/`sanitize`: guards only apply when `!is_option` (`macros:423-424`). Pinned `audit_h4::h4_option_string_guards_dropped`. CONFIRMED S1.
- `ArcIntern<str>` supported (`macros:400-401`, `audit_h4::h4_arcintern_supported`).
- `Vec<T>`/nested struct/enum/`HashMap`/`Duration`/`PathBuf`/tuples/generics: derive emits `value.parse::<T>()` requiring `FromStr`; missing impl → compile error (good, fail-closed) but message is generic `FromStr` error, not “unsupported field type”. PARTIAL S2. No `trybuild` yet. Recommend explicit `compile_error!` for known- Kata unsupported shapes with clear message, or extend support per matrix.

New issue: `let _ = attr.parse_nested_meta` swallows unknown-attribute errors, so typos/`version_*` silently ignored. Should propagate error.

## H5 — Per-attempt timeouts can’t interrupt sync execute

`SupervisedBatch::execute_with_timeout_and_retry` runs `unit.execute(&ctx)` synchronously inside `tokio::time::timeout` (`batch.rs:389-431`).

- `std::thread::sleep(400ms)` with `timeout_ms=50`: summary `completed=1, cancelled=0`, elapsed ~400ms. Timeout silently ignored (inner wins race after block). Pinned `audit_tests::h5_sync_sleep_blocks_timeout_characterization`. Desired (cancelled, elapsed≈budget) ignored. CONFIRMED S1.
- Busy-loop 300ms on 4-worker runtime: sibling fast task still completes (per-task `JoinSet::spawn`), so no global starvation with spare workers, but blocked worker unavailable for duration. Current-thread runtime not yet tested (need follow-up).
- Retry overlap not yet probed (needs `max_retries>0` + slow execute).

Docs purity contract (`traits.rs:93-111`, SKILL §2) says `execute` MUST be non-blocking; violation defeats supervisor invariants — observed exactly. Options: run `execute` on `spawn_blocking`, cooperative token in `WorkContext`, document hard limits. ADR needed.

## H6 — Wrapper delegation incomplete

| Wrapper × method | `default_timeout_ms` | `type_name` | `depends`/`provides` | `FieldAccess`/`Describable` |
|---|---|---|---|---|
| `Instrumented<U>` | ✗ (30_000 default, `wrapper.rs:119-146` no override) | ✓ delegates | ✓ | ✓ delegates |
| `ComponentAdapter` | ✗ (no override, `wrapper.rs:272-297`) | ✓ delegates | ✓ | ✓ (validated when exclusive, silent when shared — H1) |
| `SuffixedComponent` | ✓ (`wrapper.rs:590-591`) | ✓ | ✓ | ✓ (via inner `Arc`, ReadOnly when shared) |
| `Arc<dyn Component>` | ✓ (`lib.rs:82-83,103-104`) | ✓ | ✓ | ✓ (ReadOnly when shared) |
| `ComponentCascade` | default (no inner to delegate; arguably correct) | own `"component_cascade"` | empty | fieldless + own describe |

Pinned `audit_h6::*`. Desired delegation tests ignored. CONFIRMED S2 — wrapped unit silently reverts to 30s timeout.

`as_any` returns wrapper, so `component_downcast_ref::<Inner>` fails through 1+ layers (`audit_h6::h6_downcast_through_wrapper_fails`, existing `wrapper.rs:instrumented_downcast_roundtrip`). By design today; proposal needed: `inner()`/`unwrap_chain()` helper or forwarded downcast. `ComponentAdapter::inner()` and `SuffixedComponent` inner exist but `Instrumented` has no accessor (field private).

## H7 — Blanket impl constraint

Docs say “no blanket impl — would collide … unsound” (`SKILL.md:289-291`, `§16:1989-1994`). Source shows `impl Component for Arc<dyn Component>` (`lib.rs:136`) plus `WorkUnit`/`FieldAccess`/`Describable` impls for same type; a blanket `impl<T: FieldAccess+Describable+WorkUnit+Send+Sync> Component for T` would overlap when `T = Arc<dyn Component>` → coherence error E0119, not memory unsoundness. Terminology drift CONFIRMED S3. Prototype (a)/(b)/(c) not run; recommend no-go (macro stays) pending scratch-branch compiler-error capture. PARTIAL overall.

## H8 — First-Ok overlap

- `ComponentCascade::execute_first_ok` advances only on `Err` (`wrapper.rs:386-396`); `Ok(WorkOutput{success:false})` short-circuits (`audit_h8::h8_cascade_ok_fail_short_circuits`). Docs claim “`Err`/`WorkOutput::fail` moves to next” (`SKILL.md:1649`) — code/docs diverge. Decide: either fix code to treat `!success` as continue, or fix docs. CONFIRMED S2.
- Empty cascade → `Err(Execution("no units"))`; `first_accept_in_order_sync` empty → `Ok(None)` (`ladder.rs:40-45`). Different shapes, documented.
- `first_accept_in_order[_sync]` has `Ok(None)` skip + `stop(&E)` terminal predicate; cascade has neither. Cannot express cascade over ladder without mapping `WorkOutput::fail` → `Ok(None)` and `stop=|_|false`. Equivalence table in code comments; consolidation requires semantic choice above.

## H9 — Documentation drift

Confirmed drifts (claim → location → actual):
- “`dyn Trait` + `Arc` — *static* dispatch” (`SKILL.md:117`) → dynamic dispatch with static checking; fix wording.
- “blanket impl is unsound” (`§16`) → coherence conflict E0119; fix term.
- “zero cost” wrappers (`SKILL.md:220,758`) → `Instrumented::execute` emits `tracing::info!` per call (`wrapper.rs:139`) + optional histogram; not zero cost when enabled. Measure with/without subscriber (not yet run).
- Derive expansion (`SKILL.md:633-688`) shows 3 fields while example struct has 5 (`endpoint`, `name` omitted) and shows `minimum: "1"` strings (now known bug). Fix snippet or generate from `cargo expand`.
- `register` return type, `poll_budget=64`, `WorkError` variants, `empty_is_none` default true, `ComponentArcExt` semantics — spot-checked correct; full fence extraction + doctest/CI check pending (Phase 4 item 1).

## H10 — Stringly-typed core API

Design only. No prototype yet. INCONCLUSIVE. ADR + feature-gated `FieldSpec`/`FieldValue` prototype pending.

## H11 — Shared-mutation model

After H1. Options (A `&self` interior mutability, B `ConfigurableShared`, C clone-and-modify, D `ArcSwap`) not prototyped. INCONCLUSIVE. ADR pending; deprecation path for `ReadOnly` footgun required.

## H12 — Reflection-tier costs

No benchmarks run. INCONCLUSIVE. Need criterion: direct vs `serde_json::from_value` vs `set_field` vs `HashMap`, 3/12/40 fields, `dyn` call, `Instrumented`, one `MiddlewareChain` layer. Correct docs table afterwards; consider length-bucketed/perfect-hash match if wide structs slow.

## H13 — Boundary decoder

- No panic on sample arbitrary inputs (`audit_h13::h13_never_panics`); needs proptest + fuzz for proof.
- Identity on valid JSON holds for sample; idempotence holds for sample.
- `lenient()` preserves string contents except control-char escaping + single-quote conversion (by design).
- `for_schema` ignores unknown members (pinned).
- New surprise: `decode_boundary` retains JSON quotes for `String` fields without `coerce` (`mini.action=="\"hi\""`). Either spec (members are JSON fragments, `set_field` expects unquoted?) or bug (should `StripQuotes`). Needs maintainer decision. PARTIAL, S2.
- Bounded work/stack on deeply nested/megabyte input not measured; key-smuggling vs `set_field` validation not probed (relies on `set_field` constraints, but H3/H4 gaps apply).

## H14 — Ecosystem fit

Memo not written. INCONCLUSIVE.

## Maintainer questions (need answers before fixes)

1. H1/H11: is `ComponentAdapter` silent-store-while-shared intentional? If yes, document divergence + when `execute` vs `get_field` wins. If no, should `set_field` return `ReadOnly` (fail-closed) or clone-and-validate?
2. H2: should `minimum`/`maximum`/`maxLength` become numbers (breaking schema JSON)? Should `pattern` stay substring (rename to `contains`/`substring` in schema with `x-` prefix) or become regex? Should `version_*` be implemented or docs removed? Should `clamp` appear in schema?
3. H3: confirm strict `str::parse` oracle for ints (reject `1.9`/`1e3`/whitespace) vs tolerant `parse="number"` opt-in?
4. H4: should `Option<T>` inherit inner constraints/guards, or docs declare them unsupported? Should unsupported types be `compile_error!` with message?
5. H5: accept `spawn_blocking` for `execute` (cost: thread hop per task) or keep purity contract + document “timeouts don’t preempt blocking execute”?
6. H6: should wrappers delegate `default_timeout_ms` (additive, non-breaking)? Should downcast-through-wrapper be supported (add `inner()` accessors)?
7. H8: should `ComponentCascade` treat `Ok(fail)` as continue (matching docs) or docs fixed to `Err`-only?
8. H13: should `decode_boundary` strip JSON quotes for `String` fields by default?
9. H12/H14 scope: run benchmarks + ecosystem memo now or defer to ADRs?

## New issues (not hypothesized)

- N1: `fluent-wvr-macros/src/lib.rs:48` swallows `parse_nested_meta` errors → unknown `#[field(...)]` args silently ignored. Should propagate `compile_error`.
- N2: `FieldSchema::min/max: Option<f64>` loses integer precision for `u64::MAX`/`i64::MIN`; typed layer (H10) should preserve.
