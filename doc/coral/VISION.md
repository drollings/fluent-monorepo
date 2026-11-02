# Coral Context — Vision

**A deterministic-first context graph: answer from memory and
structure before ever paying for inference.**

## Mission

Most AI systems consult a probabilistic model for every query —
paying latency, cost, and unpredictability each time. Coral Context
inverts this: it resolves queries through a cascade of
progressively more expensive tiers, stopping at the first hit, and
turns every expensive answer into a permanent, replayable local
artifact. The result behaves like a fast, cheap, auditable memory
that grows more capable with use, with inference as the last rung —
never the default.

## Goals

- **Deterministic before probabilistic.** Anything already known —
  a hot fact, a tool match, a graph lookup, a semantic near-neighbor
  — resolves locally in milliseconds with no model in the loop.
- **One-time cost for novelty.** A genuinely new problem may reach
  local or frontier inference, but its solution is decomposed into a
  reusable workflow and stored, so the same class of question never
  pays that cost twice. Frontier-call frequency should trend *down*
  over an installation's life.
- **Auditable by replay.** Every resolution records which tier
  answered and why; deterministic tiers replay exactly.
- **Edge-capable.** Single-process storage, bounded memory, no
  mandatory servers — full function on modest hardware.

## End-to-end capabilities

- **Tiered answering.** Ask a question; get a cached answer in
  milliseconds when it (or a near neighbor) is known, a locally
  decomposed multi-step answer for complex tasks, or a frontier
  answer for true novelty — each with its provenance attached.
- **Knowledge graph.** Store entities and relations, traverse them
  structurally, search them by keyword and by meaning (hybrid
  search), and constrain them with a curated type hierarchy.
- **Agent integration.** Serve lookups, inserts, and traversals over
  the Model Context Protocol so AI agents share the same memory.
- **Extensible tools.** Run user- or model-provided tools as
  sandboxed WASM plugins with no host access by default.
- **Context packing.** Assemble token-budgeted, level-of-detail
  context for downstream models: full text where it matters, a line
  or label where it doesn't.
- **Bulk learning.** Ingest structured knowledge (RDF/Turtle,
  whitelisted taxonomies) into the graph in transactional batches.
- **Safety at the boundary.** Blocked private-network egress, PII
  redaction, and input validation on untrusted content.
- **Composes with routing.** Coral answers "has this been seen
  before?"; the request router answers "which model should handle
  it?" Policy and safety gating always run before the cache is
  consulted, never after.

## Components (sketch only)

Six answering tiers in cost order — hot in-memory cache, WASM tool
matching, graph search, semantic similarity, local decomposition,
frontier dispatch — over one embedded graph store with keyword,
vector, and hierarchy traversal; a plugin runtime; a prompt packer;
a bulk ingester. Tier internals, schemas, and budgets live in
`DETAILS.md` / `OVERVIEW.md` / `ARCHITECTURE.md` equivalents and
the code — this document states what the tiers guarantee, not how
they are built.

## Non-goals

- Not a model, a training system, or a multi-tenant service.
- Not a replacement for policy: cached answers never override
  safety or domain gating.
- Not a second router: model selection and escalation policy belong
  to the routing layer; Coral owns the cache decision.

## Success looks like

Most traffic resolves deterministically in tens of milliseconds;
frontier consultations are rare and shrinking; every stored answer
is replayable; the whole system runs on edge-class hardware.
