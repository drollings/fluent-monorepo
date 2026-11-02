# Coral Router — Vision

*The aspirational brief: what Coral Router is for and what finished
looks like. What is landed today lives in `ARCHITECTURE.md`.*

> **Section status legend.** Each section carries a `Status:` line:
> **Implemented** (exists today), **Partial** (working core plus
> described extensions), **Design-only** (aspirational). Update the
> line when status changes; never rewrite the vision to match a
> landing.

## Mission

> Status: Implemented.

Coral Router is a local-first control plane for LLM traffic: a
single OpenAI-compatible endpoint that answers every request the
cheapest safe way — deterministic logic where possible, a small
local model where sufficient, larger local models where warranted,
frontier providers only when genuinely necessary. To callers it
feels like one coherent, capable model; inside, each request climbs
a ladder where no rung is consulted until every cheaper rung has
genuinely failed. It also owns the local inference fleet it routes
to: one supervised server process per model weights file, managed
through its own management contract.

## Design principles

- **Deterministic before probabilistic; cheap before expensive.**
  Regexes and rules first, then fast classifiers, then local
  models, then frontier — each rung earned by the failure of the
  last. A fully unit-testable layer with no model in the loop.
- **Condensed context, not accumulated context.** Sessions compact
  rather than grow: durable storage keeps everything, the model's
  working context sees only a bounded, relevance-shaped slice.
  Summaries derive from full text, never from summaries.
- **Frontier as a bounded, audited exception.** Frontier calls are
  for genuine difficulty or capability gaps, in a small set of
  deliberate engagement modes. Every one is logged, and every
  success feeds a reusable local artifact — so frontier frequency
  trends *down* as local libraries fill.
- **Terminate, don't loop.** Multi-pass reasoning has a fixed round
  count; escalation fires on named triggers, never by default.
- **Auditable by construction.** Every filter, classification, and
  escalation produces a legible reason in a durable audit stream,
  with provenance tiers so higher-authority claims supersede lower
  ones on the same content version.
- **Own the serving processes.** One server per weights file on
  loopback, supervised with backoff; on-demand residency with
  eviction; the router is the only orchestrator between its servers
  and every other endpoint.

## End-to-end capabilities

> Status: core Implemented; agent coordination Partial.

- **One endpoint, many brains.** Any OpenAI-compatible client gets
  routed answers with per-request model selection it never sees.
- **Policy gating first.** Deterministic filters reject or redirect
  unsafe, incoherent, or out-of-policy requests before any model
  runs.
- **Progressive escalation.** Local chain → anonymized frontier
  question → team review → full handoff, each strictly more
  permissive, each audited.
- **Learning loop.** Successful frontier solutions become replayable
  local workflows keyed by embedding; validated answers accumulate
  into reusable rubrics.
- **Managed fleet.** Boot, supervision, instance pooling, VRAM
  residency, and snapshots through one management contract.
- **Bounded sessions.** Compacted, provenance-typed conversation
  state shared cheaply across parallel and filtered views
  (red-team views, PII-safe views, per-role fidelities).
- **Purpose-built routes.** Planning for underspecified requests
  (prior-workflow match or targeted questions); fixed-round
  rigor review for high-stakes ones.

## Components (sketch only)

> Status: tree engine, escalation modes, serving layer, and ledger
> store Implemented; self-updating tree, coordinator depth
> Partial; stream-native modeling Design-only.

A nested classification tree (deterministic filters, LLM
classifiers emitting structured verdicts, dispatch terminals, and
fallbacks) with auto-constructed prompts; a four-mode escalation
ladder with audit and workflow extraction; supervised per-model
servers with residency policy; a content-node ledger with
level-of-detail rendering and shared/filtered views. Node types,
verdict grammars, budgets, and APIs live in `ARCHITECTURE.md` and
the code.

## What this project deliberately is not

- Not a multi-tenant gateway product; built for one workstation.
- Not a wrapper around third-party routing/caching logic; routing
  and caching are purpose-built here.
- Not frontier-dependent: local-first, with the frontier an
  audited exception.
- Not an ensemble-by-default system: every extra model call must
  be earned by a cheaper stage's failure.
- Not a second router over llama.cpp's router mode; it supervises
  the servers itself.
