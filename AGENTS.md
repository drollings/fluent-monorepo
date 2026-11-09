# Coral Router — Development Guide

**What it is**: a Rust-native LLM request router (crate `fluent-router`, binary
`coral-router`) exposing an OpenAI-compatible HTTP API on `:8079`. Every
request runs a **two-stage pipeline** — `DeterministicPreFilter → Classifier`
(see `src/router/src/pipeline_types.rs`) — that resolves to a direct response,
a routing target, or a rejection. Coral Router is also the **process owner** of
the local inference fleet: it spawns and supervises one `llama-server` process
per model weights file, serves the `/instances` management contract at its own
address, and is the single routing element between those llama-server tasks and
every other OpenAI-compatible endpoint. The llama.cpp router mode is never used.

This is a Rust monorepo with many crates and shared infrastructure; coral-router
is the priority.

## Build-Test Loop

One Makefile convention drives every component. Each **component** — a
binary/product target (`coral-router`, `guidance`, `protopyne`,
`protopyne-egui`, `coral`) or a main shared crate (`common-core`,
`fluent-wvr`, `fluent-concurrency`, `dag`, `fluent-egui`) — exposes the same
five gates, named `<component>-<level>`:

| Gate | Meaning | Command |
|---|---|---|
| format | rustfmt the component (**write**) | `make <component>-format` |
| lint | clippy `--all-targets -- -D warnings` | `make <component>-lint` |
| smoke | **FAST** — type-check every target + run unit tests | `make <component>-smoke` |
| test | **REGULAR** — the component's full hermetic suite | `make <component>-test` |
| soak | **SLOW** — the component's `#[ignore]`d heavy tests | `make <component>-soak` |

Repo-wide equivalents: `make format` (write) / `make format-check` (verify) /
`make lint` / `make smoke` / `make test` / `make soak`. `make help` lists the
component names. (`make fmt` / `fmt-check` are aliases for
`format` / `format-check`.)

See `doc/router/TESTING.md` for the full testing convention and the
`test-<shorthand>` / `test-live` Make target table.

**AI-inference tests must be `#[ignore]` + `live-ai`-gated and run only via
`make test-live`.** No test in `make test` / `make coral-router-test` /
`make router-mock` / CI may dial a real inference endpoint.

Run `make help` to see the full list of Makefile targets.

### When to run which level

The entire point is to keep the loop fast: run the **narrowest** level that
answers the question, once.

- **Minor milestone** (small change, component known): `make <component>-smoke`
  — plus `make <component>-lint` when the change touches public types or
  signatures.
- **Concluded workflow** (the component is done): `make <component>-test`, then
  **one** repo-wide `make smoke`.
- **Repo-wide `make smoke`** runs only at the end of a workflow — never
  per-milestone.
- **Repo-wide `make test`** (the full hermetic suite) is run **on user command
  only** — the user runs it before any git commit. An agent does not run it "to
  be safe".
- **Repo-wide `make soak`** is the heavy `#[ignore]`d tier; run it only when a
  change touches concurrency, timing, or calibration. The curated subset is
  `make test-heavy`.

> `smoke` runs the tests compiled into a lib/bin target. A crate that keeps
> every test in an auto-discovered `tests/` integration target reports 0 smoke
> tests; its all-target type-check still runs, and its `-test` gate covers the
> rest.

### One run, then read the log

Every matrix gate runs through `bin/gate-run.sh`, which captures the command's
full output to `$GATE_LOG_DIR/<target>.log` (default
`/tmp/opencode/monorepo-gates`) and prints **one** line on success:

```
make dag-test SUCCEEDED — 156 passed, 0 failed, 0 ignored (1 binary) · 5s  [log: /tmp/opencode/monorepo-gates/dag-test.log]
```

On failure it prints `make <target> FAILED …` followed by a diagnostic excerpt
(the libtest `failures:` blocks, or the `error[E…]`/`error:` diagnostics when the
build itself failed) capped at `GATE_HEAD` lines (default 50), and the log path.

So a gate is already quiet — do **not** re-run it to see a different slice.
`grep`/`tail` the log the summary points at instead:

```
grep -nE "FAILED|panicked|error\[" /tmp/opencode/monorepo-gates/dag-test.log
tail -40 /tmp/opencode/monorepo-gates/dag-test.log
```

## Improvement Loop (guidance scorecard)

The `guidance-scorecard` Makefile target is the measuring instrument for
iterative work on `guidance`: hermetic fast tier (<60s, ≤40 lines + JSON,
exit 2 on a ratchet violation), never tuned against. The loop:

1. Run `make guidance-scorecard`; take the worst **tune-set** metric.
2. Form a hypothesis; make one small change with a failing test first.
3. Run the narrowest gate; rescore; keep iff improved with no ratchet
   violation, revert otherwise. Report one line per iteration.
4. Held-out splits and benchmark files are read-only to the improving
   agent: `src/guidance/corpora/*`, `src/guidance/fixture_graph/*`,
   `src/guidance/scorecard_baseline.json`. Held-out is reported, never
   iterated on.
5. Baselines update only via `guidance scorecard --update-baseline
   --reason "..."` (records reason + diff, refuses silent regressions;
   `--allow-regression` needs explicit human approval). Never hand-edit
   a baseline to make a number pass.

## Import boundaries

Shared library crates may NOT import from `guidance`, `router`, `coral`,
`protopyne-*`, `orator-*`, or `wasm_ipc`, as those are reserved for building
compiled tools.

## Code conventions

* Do not write comments more than two lines long

* Comments should focus on the "why", not the "what" or "how" the code is written

* Do not reference documents or roadmaps that live outside of ./doc in comments, or their milestones

## Unit test conventions

When adding unit tests, always follow this same convention — create
`src/<crate>/tests/<module>.rs` first, then the `#[path]` forwarder, never an
inline `mod tests {`.

### Prohibited AI Use Cases or Actions

* Writing comments that reference the agent's current workflow, whether roadmaps, decisions, or milestones.

* Any git write operations (checkout, commit, stash, etc.)

* Any implementation without review of relevant documents including, but not limited to:

  - ./doc/skills/common-core/SKILL.md
  - ./doc/skills/fluent-wvr/SKILL.md
  - ./doc/skills/fluent-concurrency/SKILL.md

* Any implementation without reviewing shared primitives in shared libraries, and any removal of code from without human approval from these libraries:

  - ./src/common-core
  - ./src/fluent-wvr
  - ./src/fluent-concurrency

* Removing suspected unused primitives from shared crates without specific user approval.  Do not ask for such approval without extensive code review.

* Referencing transient roadmaps or milestones in documentation or comments
