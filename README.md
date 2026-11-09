# Fluent Monorepo

This workspace holds a set of Rust projects built on shared infrastructure:
a component model for configurable units of work, concurrency primitives
over Tokio, and dependency-graph utilities. The projects cover HTTP request
routing to language models, a knowledge-graph library, code navigation,
paper trading, offline voice interaction, and tokenizer/parser tooling.
Where a task can be decided by rules or local computation, the code does
that instead of calling a model.

## Member Projects

- **Fluent WVR** - component model (`Fluent, Wrapped, Verified, Reflected`):
  derive macros for field access, validation, and JSON Schema from struct
  definitions. → `doc/skills/fluent-wvr/SKILL.md`
- **Fluent Concurrency** - thin layer over Tokio (plus a `reqwest`-backed,
  capability-gated HTTP client on the serving path): bounded worker pools,
  structured scopes, supervised batches, rate limiters, priority queues.
  → `doc/skills/fluent-concurrency/SKILL.md`
- **Fluent DAG** - dependency graphs, provider-narrowing workflow resolution,
  and checkpointed step graphs. → `doc/skills/dag/SKILL.md`
- **Fluent Egui** (in development) - UI scaffolding beneath the trading
  terminal and chart shells; the chart-model/scale crates stay UI-free. →
  `src/fluent-egui/src/lib.rs`
- **Shared crates** - `common-core` (retry, calibration, sync primitives),
  `fluent-llm` (protocol, backends, queues), `search-vector` (vector math /
  HNSW); consumed across projects, never forked per crate.
- **Coral Router** - HTTP service with an OpenAI-compatible API that routes
  chat requests through a two-stage pipeline (deterministic pre-filter, then
  classifier) with a fallback ladder across local and remote models. It starts
  and supervises one `llama-server` process per local weights file, plus
  optional ONNX backends. → `doc/router/VISION.md`
- **Coral Context** - knowledge-graph library: six answering tiers (hot cache
  → tools → graph → semantic → local decomposition → frontier),
  level-of-detail context packing, SQLite graph database, MCP server, WASM
  plugin runtime. → `doc/coral/VISION.md`
- **Guidance** - code navigation tooling that builds metadata mirrors and
  SQLite vector-search databases from a source tree; millisecond-scale
  deterministic queries on the L0 path. → `doc/guidance/VISION.md`
- **Orator** (in development) - local, Linux-first voice interaction:
  dictation, interruptible conversation, live captions, diarized meeting
  notes; inference runs on the workstation. → `doc/orator/VISION.md`
- **spacy-rs** (in development) - tokenizer, part-of-speech tagger,
  dependency parser, and interlingua id assignment in Rust; the rule-based
  parse is the default and model input is used only to amend low-confidence
  spans. → `doc/spacy-rs/VISION.md`

## Design philosophy

1. **Configuration from struct definitions**: derive macros generate field
   access, validation, and JSON Schema from the same fields, so the accessor,
   the validator, and the schema cannot drift apart. Polymorphism lives at
   the request boundary (`Arc<dyn Component>`); hot loops use concrete types.
2. **Local computation first, models second**: prefer rules and local
   computation over model calls; model calls handle what rules cannot, and
   their output is validated the same way.
3. **Dependency graphs for reuse**: resolved plans are cached as directed
   acyclic graphs - structured for easy reuse.
4. **Edge-deployable**: single-process SQLite, no mandatory external
   database or services for the deterministic core; the local inference
   fleet (`llama-server` processes, ONNX weights) and audio/model assets
   are optional and hardware-dependent - full function on modest hardware
   for the deterministic tiers.
5. **Capability-gated I/O**: DB and knowledge effects require explicit
   capability tokens; operator CLI tooling is capability-exempt by design
6. **Structured concurrency**: `Scope`-spawned tasks are awaited when the
   scope closes and server-owned background tasks are awaited at graceful
   shutdown; panics are contained within `SupervisedBatch`
7. **Safe Rust by default**: the shared foundation crates carry
   `#![forbid(unsafe_code)]`; in shipping (non-test) code the only `unsafe`
   is the `read_unaligned` boundary access in `wasm_ipc`

# Member projects in detail

## Coral Router - request routing over local and remote models

Coral Router exposes an OpenAI-compatible HTTP API and runs each request
through a **two-stage pipeline** - a deterministic pre-filter, then a
classifier (see `src/router/src/pipeline_types.rs`) - that resolves to a
rule-based answer, a model-backed workflow, or a different model endpoint.
Requests routed to a locally managed model dispatch to that model while the
router manages its VRAM footprint.

It is built for use with a branch of llama.cpp that allows parallel
inference across context windows with their own sizes, parameters, and
lifetimes per request via HTTP parameters:

https://github.com/drollings/llama.cpp/tree/_multi_context

It is also the **process owner of the local inference fleet**: it spawns and
supervises one `llama-server` per model weights file, serves the `/instances`
management contract, and is the single routing element between those local
tasks and every other OpenAI-compatible endpoint. A dispatch to a local model
is a direct call to the owning server; a frontier or remote call is the same
request routed onward after the local ladder has genuinely failed to resolve
it - never by default.

- **Rules before models.** Anything decidable by a regex or a fixed rule is
  handled without a model call wherever the pipeline runs - a cost floor and
  a fully unit-testable layer with no model in the loop.
- **Cheapest capable backend first.** The ladder runs deterministic filter →
  fast classifier → local model → frontier, and a request only reaches a rung
  after the previous one failed to resolve it.
- **Compacted session history.** The ledger compacts sessions rather than
  growing them, so follow-up requests carry the settled context instead of
  the full transcript.

## Fluent WVR - the design patterns

Fluent WVR (`Fluent, Wrapped, Verified, Reflected`) is the shared convention
for configurable units of work: one struct definition drives field access,
validation, and JSON Schema, so the three cannot drift apart.

Every unit of work presents the same `Arc<dyn Component>` interface -
whether it is a native Rust struct, a WASM plugin, or a database-driven
config - so callers iterate over uniform handles instead of branching on
origin. Twelve composable patterns are documented in
`doc/fluent-wvr/COMPOSABILITY.md` (day-to-day guide; authoritative contract in
`doc/skills/fluent-wvr/SKILL.md`): Fluent Builder, Trait-Based Reflection,
Trait Composition, Trait Objects, Binary IPC, Scoped Ownership, Newtype
Handles, Unit of Work, Middleware Chain, Component Adapter, Structured
Logging Context, Runtime Composition. These patterns are for the control
plane, not for hot-path inner loops: the data plane uses concrete types and
flat enums, and `dyn` lives at the request boundary, not in the tight loop.

## Fluent Concurrency - the execution fabric

`fluent-concurrency` is a thin, 100% safe extension layer over Tokio
(plus a `reqwest`-backed capability-gated HTTP client on the serving path):
bounded worker pools, structured `Scope`s, the `SupervisedBatch`
(supervision + dependency cancellation + panic/fail/cancel tracking),
`Limiter`, `PriorityQueue`, `CreditFlow`, and the `first_accept_in_order`
ladder. Tokio does the scheduling - the crate composes its primitives rather
than rebuilding them.

Tasks spawned inside a `Scope` are awaited when the scope closes, and
server-owned background/connection tasks are awaited when the server drains
them at graceful shutdown. Capability tokens gate DB and knowledge access on
the serving path - operator CLI tooling is capability-exempt by design.

→ `doc/skills/fluent-concurrency/SKILL.md`

## DAG - the dependency fabric

`fluent-dag` provides the `DependencyGraph` and `CheckpointedStepGraph`
primitives that drive the chart executor, session orchestration, and workflow
execution: dependency validation, ready-node selection, dependency-aware
cancellation, and checkpoint/rewind - shared by every graph consumer in the
workspace rather than re-implemented per crate. → `doc/skills/dag/SKILL.md`

# Quick start

```bash
make
make router
```

Coral Router's own loop: `make router` (build), `make router-start` (build and
restart on `:8079`), `make test-router` (tests), `make router-mock`
(config-synced routing integration tests, derived from `env/coral-router.json`).

# Authorship

Authored by Daniel Rollings, February 2026, based on conceptual transfer from
projects in Python, C++, and Zig, ported to Rust.

# License

`fluent-monorepo` is **dual-licensed** under the terms of either:

1. **GNU Lesser General Public License v3.0 or later** (`LGPL-3.0-or-later`), OR
2. **Commercial License**

You may select the license terms that best fit your project's compliance requirements.

---

### Option 1: Open Source Use (LGPLv3)

You are free to use, modify, and distribute this software under the terms of the **GNU Lesser General Public License v3.0** (`LICENSE-LGPLv3`).

* **Internal / Cloud SaaS Use:** You can freely use `fluent-monorepo` inside your organization or behind a network/SaaS boundary without triggering copyleft obligations.
* **Open Source Projects:** You may freely include or link against `fluent-monorepo` in open-source applications.
* **Rust Static Linking Notice:** Because Cargo compiles Rust dependencies directly into static application binaries, distributing a proprietary closed-source application that embeds `fluent-monorepo` under LGPLv3 requires you to either:
  1. Open-source your application under a compatible license, **OR**
  2. Provide object files (`.rlib`/`.o`) or source code sufficient to allow end users to re-link your application against modified versions of `fluent-monorepo` (per LGPLv3 Section 4).

If your project cannot comply with LGPLv3 static-linking requirements or you do not wish to distribute object files for proprietary code, you must obtain a **Commercial License**.

---

### Option 2: Commercial License

The **Commercial License** removes all LGPLv3 copyleft and re-linking obligations, allowing you to freely embed, statically link, and distribute `fluent-monorepo` within closed-source, proprietary products.

A Commercial License is recommended for:
* Closed-source commercial software products distributed to end users.
* Enterprise deployments requiring formal SLA guarantees, dedicated technical support, indemnification, or liability waivers.
* Teams requiring custom contributor agreements or tailored integration support.

---

### Third-Party Dependencies & Acknowledgments

`fluent-monorepo` is built on top of the Rust open-source ecosystem and relies on third-party crates, including:

* **Tokio** runtime and async primitives - licensed under the permissive [MIT License](https://github.com/tokio-rs/tokio/blob/master/LICENSE)
* Additional ecosystem dependencies - licensed under permissive standard licenses (MIT, Apache-2.0, or BSD)

Under the terms of these permissive upstream licenses, you remain fully compliant when linking them alongside `fluent-monorepo`. Complete license notices for all transitive dependencies are included in the source distribution and generated dependency manifests (`cargo-deny` audit reports).
