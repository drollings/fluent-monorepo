# Unified Dependency Resolver — Vision

> API reference, concrete signatures, and benchmarks live in
> `doc/skills/dag/SKILL.md`; this document describes what the
> resolver is for and what it guarantees.

## Mission

Build systems resolve dependencies by *name*: `depends: ["parser"]`
means "run the target named parser first." That breaks when a
dependency names a *capability* — "code review", "translation",
"embedding" — that many tools provide and only one should satisfy.
This resolver extends classic topological ordering with a
provider-selection policy: include every provider (build-graph
semantics) or narrow to exactly one (capability-graph semantics),
reporting structured ambiguity when the request is genuinely
under-determined.

## Goals

- **One algorithm, two policies.** The same resolver serves
  compilation pipelines and LLM workflow orchestration; switching
  is a single policy value, not a second implementation.
- **Decide, or explain why not.** Narrowing applies cheap,
  deterministic preferences (explicit marks first, then
  ready-to-run, then locality); whatever survives un-narrowed is
  reported with the full candidate list so a caller can re-prompt,
  ask a human, or rank by cost — never a bare failure.
- **Deterministic and safe.** Same input, same plan. A provider
  that loses narrowing for a capability stays excluded even if
  another dependency path would rediscover it.

## End-to-end capabilities

- **Build-graph ordering.** Given named targets with dependencies,
  produce a valid execution order; cycles are a named error.
- **Capability-based workflow construction.** Given workflow steps
  described by needed/provided capabilities, select one concrete
  provider per contested capability and order the result.
- **Dynamic tool composition.** Register or retire providers at
  runtime (plugins, APIs, models); the next resolution adapts with
  no rewiring.
- **Multi-model dispatch.** Treat interchangeable backends as
  providers of one capability and let narrowing express cost,
  locality, and readiness preferences.
- **Self-healing plans.** When a provider disappears, re-resolve the
  same workflow to find an alternative without changing the
  workflow description.

## Components (sketch only)

A target registry (names, capabilities, preference marks), a
string-interning capability index, a transitive-closure walk, a
four-stage narrowing pipeline, and Kahn's topological sort as the
final ordering step. Stage order, error shapes, and the
rejection-durability rule are specified in `SKILL.md` — enough for
an independent implementation to agree on outcomes, without cloning
this one's data structures.

## Non-goals

- Not a scheduler, executor, or plugin host; it produces plans,
  others run them.
- Not a relevance ranker; ambiguity is reported, not hidden behind
  scoring.
- No second resolver for "build" vs "workflow" — policy selection
  covers both.
