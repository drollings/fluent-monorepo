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

## Make targets

| Target | Purpose |
|---|---|
| `make <component>-format` | rustfmt that component (write) |
| `make <component>-lint` | clippy `--all-targets -D warnings` for that component |
| `make <component>-smoke` | FAST gate: all-target type-check + unit tests |
| `make <component>-test` | REGULAR gate: the component's full hermetic suite |
| `make <component>-soak` | SLOW gate: the component's `#[ignore]`d heavy tests |
| `make format` / `format-check` | rustfmt the workspace (write / verify) |
| `make lint` | clippy `--workspace --all-targets -- -D warnings` |
| `make smoke` | Repo-wide FAST gate (workflow end) |
| `make test` | Repo-wide REGULAR gate (user command before commit) |
| `make soak` | Repo-wide SLOW gate (heavy `#[ignore]`d tests) |
| `make router` / `router-start` | Build coral-router / build + (re)start on `:8079`, waiting for `/health` |
| `make test-router` | Kill server + fluent-router unit/golden/e2e tests + `--help` dry-run |
| `make test-router-all` | `test-router` + coral-context HNSW benchmarks (slow) |
| `make router-mock` | Config-synced routing integration tests (intent → model_group, derived from `env/coral-router.json`) |
| `make protopyne-trading-suite` | Six trading-crate suites (`--all-features`) + `protopyne-server --help` dry-run |
| `make protopyne-container-smoke` | Build the Python-free container image and run the full smoke |
| `make test-<shorthand>` | One crate's hermetic suite (e.g. `test-llm`, `test-db`); component gates are canonical |
| `make test-live` | **The only pathway that runs real AI inference** — `#[ignore]` + `live-ai`-gated tests, skips cleanly without env |
| `make <crate>-test-live` | Live-AI tests for one crate (e.g. `router-test-live`, `llm-test-live`) |
| `make test-heavy` | Curated slow hermetic suites (`#[ignore]`): spacy-rs corpus, router supervisor/HNSW, search-vector, protopyne soaks |
| `make yago-load` | Operator action: download the full YaGO 4.5 taxonomy + regenerate the embedded class registry |
| `make review-test` | Async parse-review + interlingua suites (spacy-rs review, router review worker/endpoints, ontology loader, boot reconciliation) |
| `make lint-live-ai` | Hermeticity guard: every `#[ignore]` has a `reason`; no hermetic test dials a non-loopback host |

See `doc/router/TESTING.md` for the full testing convention and the
`test-<shorthand>` / `test-live` Make target table.

**AI-inference tests must be `#[ignore]` + `live-ai`-gated and run only via
`make test-live`.** No test in `make test` / `make coral-router-test` /
`make router-mock` / CI may dial a real inference endpoint.

## Runtime-lifetime and HTTP-client ownership

**A pooled resource must never outlive the runtime that created it.** This is
the workspace's most expensive recurring async hazard: a connection handed
back to a client's pool by work spawned on a runtime that is then dropped
leaves the connection stranded with a waker whose runtime no longer exists.
The next caller that checks it out parks forever — no error, no CPU, no
recovery, bounded only by that client's timeout.

Rules:

- **Never bridge on a runtime built for a single call.** Compose
  `common_core::runtime::block_on` (single-threaded fallback — concurrent
  callers serialize) or `block_on_shared` (process-wide multi-thread
  fallback — callers do not serialize, and the runtime outlives the call).
  Do not inline a `tokio::runtime::Builder` fallback; both are already the
  workspace's canonical bridges.
- **Tests must not share a pooled `reqwest::Client` across `#[tokio::test]`
  runtimes.** Each `#[tokio::test]` gets a fresh runtime that is destroyed when
  the test ends, so a shared pool hands connections between runtimes that no
  longer exist. Obtain test clients from `common_core::http::test_http_client()`
  (pooling disabled by construction), or build a client per test.

## Import boundaries

Shared library crates may NOT import from `guidance`, `router`, `coral`, or
`wasm_ipc`, as those are reserved for building compiled tools.

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
