# Audit Findings — fluent-wvr / fluent-concurrency

Branch intent: `audit/fluent-wvr-hypotheses` (per mission § operating rule 2).
Note: this repo's `AGENTS.md` prohibits git write operations for agents, so no
branch was created and nothing was committed by this session; all work below is
in the working tree, in small single-purpose edits, ready to commit per the
mission's commit discipline. Unrelated dirty-tree work (protopyne-*) was not
touched.

Baseline (this session, before edits):

- `make fluent-wvr-smoke`: check-build SUCCEEDED, 76 passed / 0 failed / 0 ignored.
- `make fluent-wvr-test`: 297 passed / 0 failed / 0 ignored (30 binaries).
- `make fluent-concurrency-smoke`: 196 passed / 0 failed / 1 ignored (the H5
  desired-behavior test, `AUDIT-H5`).
- `cargo test -p fluent-concurrency --doc`: 8 passed / 0 failed / 0 ignored
  (after this session's doctest conversions; was 5 passed / 4 ignored).
- Clippy/doc gates rerun after edits (see § Verification).

No pre-existing failures; the 4 ignored doctests were pre-existing and are now
compiled (H9). The 1 remaining `#[ignore]` is the H5 desired test, linked to
ADR-004.

## Summary table

| ID | Verdict | Severity | Evidence |
|----|---------|----------|----------|
| H1 shared-handle mutation | CONFIRMED, fixed (fail-closed) | S1 (was) | `src/fluent-wvr/tests/audit_h1.rs`, `src/fluent-wvr/src/wrapper.rs:344-356`, `src/fluent-wvr/src/lib.rs:111-128` |
| H2 schema fidelity | CONFIRMED, fixed (numbers/required/Option/clamp/version-gate) | S1 strings → fixed; S2 pattern/version/clamp → documented or gated | `src/fluent-wvr/tests/audit_h2.rs`, `src/fluent-wvr-macros/src/lib.rs:588-593,1879-1902`, `audit/expansion-reference.rs:150-180` |
| H3 f64 parsing | CONFIRMED, fixed (strict native first) | S1 → fixed | `src/fluent-wvr/tests/audit_h3.rs`, `audit/expansion-reference.rs:36-60` |
| H4 non-scalar coverage | CONFIRMED (Option guards), fixed; PARTIAL messages → trybuild | S1 → fixed; S2 → fixed via UI tests | `src/fluent-wvr/tests/audit_h4.rs`, `src/fluent-wvr-macros/tests/ui/` |
| H5 sync timeout | CONFIRMED, accepted via ADR (no code) | S1, documented hard limit | `src/fluent-concurrency/src/audit_tests.rs:h5_*` (1 ignored desired test), `src/fluent-concurrency/src/batch.rs:383-431`, ADR-004 |
| H6 wrapper delegation | CONFIRMED, fixed (timeout + `inner()`) | S2 → fixed | `src/fluent-wvr/tests/audit_h6.rs`, `src/fluent-wvr/src/wrapper.rs:173-183,328-336`, ADR-003 |
| H7 blanket impl | PARTIAL (terminology fixed; no-go, ADR) | S3 | E0119 repro (below), ADR-008, `tests/skill_doc_pins.rs` |
| H8 first-Ok overlap | CONFIRMED, consolidated over ladder | S2 → fixed | `src/fluent-wvr/tests/audit_h8.rs`, `src/fluent-wvr/src/wrapper.rs:438-480`, ADR-006 |
| H9 docs drift | CONFIRMED, fixed + CI-enforced via tests | S3 | `src/fluent-wvr/tests/skill_doc_pins.rs` (24-arg vocab), converted doctests, this report |
| H10 typed descriptor | Deferred by design (ADR) | — | ADR-007; `FieldSchema`/`min_raw`/`max_raw` are the lossless substrate |
| H11 shared-mutation model | Fail-closed shipped; redesign deferred (ADR) | — | ADR-005 |
| H12 cost claims | Measured; docs corrected | S3 (folklore → measured) | `src/fluent-wvr/benches/tier_costs.rs`, SKILL §4 tier table |
| H13 boundary decoder | PARTIAL → properties hold; 1 off-by-one fixed | S2 → fixed | `src/fluent-wvr/tests/audit_h13.rs`, `tests/boundary_contract.rs`, `fuzz/fuzz_targets/repair.rs` |
| H14 ecosystem fit | Memo, no code | — | ADR-007 |

Severity: **S1** data corruption/silent wrong behavior, **S2** surprising but
detectable, **S3** docs/ergonomics.

## H1 — Shared-handle mutation is inconsistent

- Exclusive `set_field` on a derive struct validates (min/max) — passing.
- Shared `Arc<dyn Component>::set_field` returns `FieldError::ReadOnly` (lib.rs).
- `ComponentAdapter::set_field` on a shared inner: previously stored the
  override without validation (`get_field` diverges from `execute`/inner).
  **Fixed**: now returns `FieldError::ReadOnly` and stores nothing
  (`wrapper.rs:344-356`); the explicit `with_field_override` builders remain
  the deliberate unchecked path. Docs (§10, §18) describe exactly this.
- Verdict CONFIRMED (bug was reachable by cloning the inner `Arc`), now fixed.
  Characterization tests pin the fail-closed behavior; the old silent-store
  behavior is gone, not just documented.
- Maintainer question (standing): is there a future need for validated shared
  mutation (ADR-005 options B/C)? Current answer: no consumer; deferred.

## H2 — `describe()` schema fidelity

- `minimum`/`maximum` were JSON strings; now emitted as JSON numbers via
  `f64_unsuffixed` literals (`macros/src/lib.rs:588-593`); `maxLength` is an
  integer. Pinned by `audit_h2` + `expansion-reference.rs:150-180`.
- `required` respects `required = false` and excludes all `Option<T>` —
  the "ignores" sub-claim is REFUTED (was already correct).
- `pattern` stays substring-enforced and stays under the JSON Schema
  `pattern` key by deliberate vendor interpretation (documented in the macro
  rustdoc, SKILL §19, COMPOSABILITY table). Collision acknowledged, additive
  contract forbids moving the key.
- `version_added`/`version_removed` are reserved and fail expansion under the
  closed-vocabulary rule (no silent ignore). Pinned by `skill_doc_pins`.
- `skip` excluded; `clamp` leaves exactly one schema trace `"x-clamp"`.
- New presentation/structure attrs (`tab/group/tooltip/step/inline`,
  `nested/items/fixed/visible_when/axis`) are documented in the macro rustdoc,
  SKILL §19, and COMPOSABILITY §1; the pin test lists all 24.
- Verdict CONFIRMED, fixed; remaining pattern-key choice is accepted/documented.

## H3 — Numeric parsing through `f64`

- Constrained integers now parse strict-native first
  (`match value.parse::<u16>()`, expansion lines 36-60). On native failure the
  code consults `f64` only to choose `Constraint` (out-of-range, e.g.
  `"65536"`) vs `Parse` (gibberish/fractions like `"1.9"`, `"1e3"`).
- `parse = "number"` tolerant path requires `fract() == 0.0` for integers.
- Verdict CONFIRMED, fixed. Oracle: `str::parse::<T>()` for the strict path.

## H4 — Derive coverage of non-scalar field types

- `Option<u32>` bounds and `Option<String>` guards (`max_len`/`pattern`/
  `sanitize`) now apply to the inner value; `Option<T>` is never `required`.
- Unsupported shapes fail at compile time; `trybuild` UI tests
  (`tests/ui/`) pin the messages (`unknown_attr`, `shapes`).
- New: `Vec<T>` (requires `items = N`), `[T; N]` (fixed), `nested`
  components (dotted paths), tagged-union arms with `oneOf` schemas.
- Verdict: CONFIRMED (silent drops existed), now fixed; PARTIAL on message
  clarity → addressed with trybuild.

## H5 — Per-attempt timeouts can't interrupt synchronous `execute`

- `execute_with_timeout_and_retry` wraps sync `execute` in
  `tokio::time::timeout`; a blocking body (sleep/busy-loop) holds its worker,
  the deadline cannot fire, and the finished inner wins the race
  (`completed`, late). Retry after overrun may overlap the zombie.
- The guarantee is documented as hard in `batch.rs:383-401` and SKILL §2:
  the timeout bounds the await/retry loop, never a blocking body.
  `cancelled`/`Timeout` fires only at await points (between-attempt backoff).
- Desired test (`h5_sync_timeout_interrupts_blocking_execute`) stays
  `#[ignore = "AUDIT-H5: ..."]` and links ADR-004 (cooperative token deferred).
- Verdict CONFIRMED (S1 footgun documented, not removed). No code change per
  ADR-004: preempting sync code from the supervisor hides blocking and risks
  pool exhaustion; the purity contract + `DbWorkUnit` offload pattern is the
  canonical answer.

## H6 — Wrapper delegation is incomplete

- `Instrumented` and `ComponentAdapter` now delegate `default_timeout_ms`
  (and `type_name`/`depends`/`provides`) to the inner; `SuffixedComponent`
  and `Arc<dyn Component>` already did.
- Downcast: `as_any` returns the outermost wrapper by design; every wrapper
  exposes `inner()` for one-layer peeling (added `Instrumented::inner()`
  with a two-hop doc-test). No forwarded auto-downcast (would hide which
  layer answered). ADR-003 updated.
- Verdict CONFIRMED, fixed.

## H7 — The "no blanket `Component` impl" constraint

- The blocker is coherence (E0119 overlap with
  `impl Component for Arc<dyn Component>`), not unsoundness. Captured with
  a faithful minimal mirror:

  `error[E0119]: conflicting implementations of trait 'C' for type
  'Arc<(dyn B + 'static)>'`

  (mirror source kept at `/tmp/opencode/h7-coherence/src/main.rs` during the
  audit; the error is inlined here as the evidence record).
- Guide wording fixed ("rejected by coherence (E0119)"), pinned by
  `skill_states_no_blanket_impl_as_coherence_limit`. No prototype merged:
  options (a)/(b)/(c) evaluated in ADR-008, verdict no-go — `impl_component!`
  stays.
- Verdict PARTIAL (terminology CONFIRMED-fixed; factoring change rejected).

## H8 — Overlapping first-Ok and sequencing combinators

- `ComponentCascade::execute_first_ok` now composes
  `first_accept_in_order_sync` (`wrapper.rs:438-480`): `Ok`+`!is_accept`
  advances (last failure verdict returned), `Err` advances through the walk,
  empty cascade errors descriptively. A failure verdict never becomes
  `WorkError::Execution`.
- Equivalence pinned by `tests/audit_h8.rs` + ladder tests +
  `tests/cascade_acceptance.rs` (27-case corpus). ADR-006 records the outcome.
- Verdict CONFIRMED (overlap existed), consolidated.

## H9 — Documentation drifts from the code

Fixed this session:

- Closed vocabulary was 14 in docs vs 24 in code (new
  `tab/group/tooltip/step/inline/axis/nested/items/fixed/visible_when`).
  SKILL §19, COMPOSABILITY §1, and the macro rustdoc now list all 24;
  `skill_doc_pins.rs` pins all 24, so drift in either direction fails
  `cargo test --workspace` (which CI runs) — the drift gate.
- Converted 4 pre-existing `rust,ignore` doctests to compiled doctests
  (`scope.rs` module + `ScopeGuard`, `thread_resource.rs` macro; the second
  `with_tlr` snippet folded into the macro doctest). Doc-tests: 8 passed,
  0 ignored.
- Verified against code: `poll_budget` default 64, `register` returns
  `Result<_, SupervisedBatchError>`, prelude contents, `WorkError` 3
  variants, `empty_is_none` default `true`, adapter fail-closed semantics,
  "dynamic dispatch with static checking" wording, no `unsound` wording
  remains, `Instrumented` documented as non-zero-cost when enabled,
  tier-cost table carries measured ratios with the soak-tier bench as source.
- Verdict CONFIRMED, fixed; drift is test-enforced.

## H10 — Typed descriptor layer

Deferred by design (ADR-007). Rationale: the crate's own throwaway rule
(SKILL §20.25) forbids new public API without an in-tree consumer; a
`FieldSpec`/`FieldValue` layer with no second consumer would be speculative.
The lossless substrate for a future typed layer shipped anyway:
`FieldSchema::{min,max}_raw` exact decimal spellings + additive
`axis`/`visible_when`/`items`/`fixed` fields (all `#[serde(default)]`,
old schema JSON still deserializes). Prototype only when two consumers need
typed values, not strings. Verdict: deferred, accepted.

## H11 — Runtime-mutation model for shared components

Fail-closed shipped (H1): shared `set_field` → `ReadOnly`, nothing stored;
`with_field_override` is the explicit unchecked path. Full redesign
(`&self` interior mutability / `ConfigurableShared` / clone-and-modify /
`ArcSwap`) deferred per ADR-005: no consumer needs it, and each option
either breaks the `FieldAccess` signature or adds machinery without a second
user. Verdict: immediate footgun removed; redesign deferred, accepted.

## H12 — Reflection-tier cost claims

Measured by `benches/tier_costs.rs` (criterion, soak-tier only; never runs in
gates). SKILL §4 carries the measured table (≈17–41x `set_field`,
≈330–5400x serde per-document by width, ≈100x `HashMap`, plus `dyn`
`execute` ≈ 17 ns / `Instrumented` ≈ +47 ns / one middleware layer ≈ 79 ns
dispatch notes). Old folklore table replaced. This session did not re-run
the soak bench (criterion, machine-specific); the bench compiles (checked via
`cargo check -p fluent-wvr --benches` in verification). Verdict: measured,
docs corrected.

## H13 — Boundary decoder adversarial testing

- Guarantees pinned by `tests/boundary_contract.rs` + `tests/audit_h13.rs`
  (no-panic on arbitrary bytes, identity on valid JSON, idempotence,
  `for_schema` ignores unknown members, only schema-listed names reach
  `set_field`, bounded work/stack) and `fuzz/fuzz_targets/repair.rs`
  (libFuzzer, manual/nightly-only, never gating).
- This session fixed one off-by-one in the walker: the key-slice end was set
  to the current index instead of one past it, truncating keys when the
  closing quote resolved them (`boundary.rs:288-293`). Covered by boundary
  tests (75 new lines in `tests/boundary.rs` exercising the quote path).
- Verdict PARTIAL → properties hold on the pinned corpus; fuzzer is manual.
  No crash found in-shard this session.

## H14 — Ecosystem fit

Memo in ADR-007 (interop-or-ignore across `schemars`/`validator`,
`bevy_reflect`/`facet`, `typetag`, `tower::Layer`): ignore with standing
interop offers; no new dependencies (only dev-dep `trybuild`, justified for
compile-fail UI tests). Verdict: memo, no code.

## New issues found (not hypothesized)

1. `max_len` overflow panicked the proc macro:
   `i.base10_parse::<usize>().unwrap()` in `parse_field_attrs` — a
   sufficiently large `#[field(max_len = ...)]` literal aborted expansion
   instead of emitting a `compile_error`. Fixed with a proper
   `meta.error(...)` (this session, `macros/src/lib.rs`). No test in-tree
   feeds an overflowing literal; the closed-vocabulary UI test covers the
   error path shape.
2. Vocabulary drift (docs listed 14 of 24 accepted attrs) — fixed (H9).
3. ADR-003/ADR-006 staleness (`Instrumented` has no `inner()`; cascade is
   Err-only) — updated to match shipped code.

## Maintainer questions

1. H1/H11: is validated shared mutation (ADR-005 options B/C) wanted, or is
   fail-closed + explicit overrides the permanent contract? (Safest current
   behavior kept; no change without approval.)
2. H5: is the cooperative cancellation token (ADR-004 option B) worth the
   `WorkContext` signature churn, or is the documented hard limit permanent?
   (Kept as proposal; ignored test links the ADR.)
3. H9: should the manual/nightly fuzz target become a gating nightly job?
   (Left manual per SKILL; proptest regression seeds can land in
   `fuzz/corpus/repair/`.)
4. H10: any second consumer for a typed `FieldSpec` layer on the horizon? If
   yes, it lifts the throwaway-rule objection to prototyping.

## Verification (this session)

- `make fluent-wvr-lint`, `make fluent-concurrency-lint` — clean.
- `make fluent-wvr-smoke`, `make fluent-wvr-test` — green.
- `make fluent-concurrency-smoke`, `make fluent-concurrency-test` — green.
- `cargo test --offline -p fluent-concurrency --doc` — 8 passed, 0 ignored.
- `cargo check -p fluent-wvr --benches` — bench harness compiles.
- `cargo doc --no-deps -p fluent-wvr -p fluent-concurrency
  -p fluent-wvr-macros` — clean. Two pre-existing broken intra-doc links
  were fixed this session (`coerce.rs` `FieldAccess::set_field` scope,
  `boundary.rs` `crate::coerce` ambiguity, `credit_pool.rs`
  `CreditReceiver::recv` scope); `cargo doc` failed before this session.
- `cargo expand` unavailable in this environment; expansion reference produced
  with nightly `-Zunpretty=expanded` on a scratch crate and saved as
  `audit/expansion-reference.rs` (covers min/max, `required = false`,
  `Option<String>`, `pattern`, `sanitize`, `coerce`, `parse = "number"`,
  `skip`; strict-native-first parse and numeric schema confirmed in output).
- No `unsafe` added; no new runtime dependencies; no `unwrap`/`expect` in new
  library code (one pre-existing macro `unwrap` on user input fixed).
- Public API changes are purely additive (`inner()`, new `#[field]` attrs,
  new `FieldSchema` fields with `#[serde(default)]`); behavior fixes are the
  audited footguns (H1/H2/H3/H4/H6/H8) with characterization tests updated to
  the desired behavior.
