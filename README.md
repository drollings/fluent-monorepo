# Fluent Monorepo - deterministic-first agentic backbone

This Rust monorepo is an incubator for lightweight, native-compiled,
hardened, fast agentic LLM projects built on a shared, composable
infrastructure.  Concurrency and dependency tracking are pervasive and
battle-tested.  Reflection and high degrees of polymorphism are available
where they help, and zero-cost where they are not.

This provides a deterministic NLP baseline for agentic LLM tools.

## Member Projects

- **Coral Router** - LLM request router; two-stage deterministic-first
  pipeline, escalation ladder, OpenAI-compatible API, owns and supervises the
  local LLM fleet through llama-server and/or ONNX. → `doc/router/VISION.md`
- **Coral Context** - deterministic-first knowledge graph library: 6-tier
  "Level of Detail" pyramid, SQLite graph database, MCP server, WASM plugin
  runtime. → `doc/coral/VISION.md`
- **Guidance** - AST-guided code navigation subagent producing metadata
  mirrors and SQLite vector search databases; sub-100ms deterministic queries
  for AI-assisted development. → `doc/guidance/VISION.md`
- **Fluent DAG** - dependency graphs, narrowing workflow, and checkpointed
  step graph.  → `doc/skills/dag/SKILL.md`
- **Fluent Concurrency** - a lightweight layer over Tokio, hyper, and
  reqwest that keeps async I/O and inference for agentic LLM applications
  fast, guardrailed, and rooted in the battle-tested Rust ecosystem.  →
  `doc/skills/fluent-concurrency/SKILL.md`
- **Fluent WVR** - the foundational lightweight component model:  `Fluent,
  Wrapped, Verified, Reflected`  → `doc/skills/fluent-wvr/SKILL.md`

## Design philosophy

1. **Reflection at the control pane**: macros for polymorphism and
   reflection where it counts.  Objects designed around Fluent WVR get a
   single source of truth in metadata, which allows validated input
   constraints, and composable control objects which allow for polymorphic
   flexibility where it counts, and poses zero cost to the hot paths.
2. **Deterministic-first, LLM-enabled**: prefer local computation over
   probabilistic inference; LLM inference is firmly harnessed, additive to
   deterministic flow
3. **Dependency graphs as memory**: novel solutions are evaluated to become
   nodes in cached Directed Acyclic Graphs - structured for easy reuse
4. **Edge-deployable**: single-process SQLite, no external services, targets
   Raspberry Pi class hardware
5. **Capability-gated I/O**: DB and knowledge effects require explicit
   capability tokens; operator CLI tooling is capability-exempt by design
6. **Structured concurrency**: `Scope`-spawned tasks are awaited when the
   scope closes and server-owned background tasks are awaited at graceful
   shutdown; panics are contained within `SupervisedBatch`
7. **Safe Rust by default**: the shared foundation crates carry
   `#![forbid(unsafe_code)]`; the only `unsafe` in the workspace is boundary
   IPC `read_unaligned`

# Member projects in detail

## Coral Router - an agentic swarm orchestrator

Coral Router exposes an OpenAI-compatible HTTP API and runs route-dispatched
requests through a **multi-stage pipeline** - a deterministic pre-filter,
then a classifier (see `src/router/src/pipeline_types.rs`) - that resolves
to appropriate deterministic, LLM-optional workflows, or delegate to
stronger LLMs at other OpenAI endpoints.  Requests that route to a locally
managed model dispatch to that model while managing its VRAM footprint,
allowing efficient local agentic teams.

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

- **Deterministic before probabilistic.** Anything decidable by a regex or a
  fixed rule is handled without a model call wherever the pipeline runs - a
  cost floor and a fully unit-testable layer with no model in the loop.
- **Cheap before expensive.** Routing is an economic decision: the ladder runs
  deterministic filter → fast classifier → local model → frontier, and a
  request only reaches a rung after the previous one failed.
- **Original context, parallel compaction.** The ledger compacts sessions
  rather than growing them; the orchestrator never reasons over noise, dead
  ends, or superseded exploration.

## Fluent WVR - the design patterns

Fluent WVR (`Fluent, Wrapped, Verified, Reflected`) gives this codebase a
consistent control plane, reflection layer, and baseline for data validation.
It does this with a collection of interlocking design patterns for consistent
metadata on composable units of work, polymorphism where needed, schemas for
datatypes internally and over IPC, and other single sources of truth.

Every orchestratable task presents the same `Arc<dyn Component>` interface -
whether it is a native Rust struct, a WASM plugin, or a database-driven
config - so the orchestrator iterates uniform handles and never branches on
origin. Twelve composable patterns are documented in
`doc/fluent-wvr/USAGE.md` (Fluent Builder, Trait-Based Reflection,
Trait Composition, Trait Objects, Binary IPC, Scoped Ownership, Newtype
Handles, Unit of Work, Middleware Chain, Component Adapter, Structured
Logging Context, Runtime Composition). These patterns are for the control
plane, not for hot-path inner loops: the data plane uses concrete types and
flat enums, and `dyn` lives at the request boundary, not in the tight loop.

## Fluent Concurrency - the execution fabric

`fluent-concurrency` is a thin, 100% safe extension layer over Tokio:
bounded worker pools, structured `Scope`s, the `SupervisedBatch`
(supervision + dependency cancellation + panic/fail/cancel tracking),
`Limiter`, `PriorityQueue`, `CreditFlow`, and the `first_accept_in_order`
ladder.  Tokio is the workhorse - the crate composes its primitives rather
than rebuilding the scheduler.

Tasks spawned inside a `Scope` are awaited when the scope closes, and
server-owned background/connection tasks are awaited when the server drains
them at graceful shutdown. Capability tokens gate DB and knowledge access on
the serving path - operator CLI tooling is capability-exempt by design.

→ `doc/fluent-concurrency/USAGE.md`

## DAG - the dependency fabric

`fluent-dag` provides the `DependencyGraph` and `CheckpointedStepGraph`
primitives that drive the chart executor, session orchestration, and workflow
execution: dependency validation, ready-node selection, dependency-aware
cancellation, and checkpoint/rewind - shared by every graph consumer in the
workspace rather than re-implemented per crate. → `doc/dag/USAGE.md`

# Quick start

```bash
make
make router
```

Coral Router's own loop: `make router` (build), `make router-start` (build and
restart on `:8079`), `make router-test` (tests), `make router-mock`
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
