# common-core — Vision

> Status: Implemented. The authoritative contract lives in
> `doc/skills/common-core/SKILL.md`; this file is a pointer. Status legend:
> Implemented (shipped and composed), Partial (working core with filed
> follow-ups), Design-only (aspirational).

## Mission

`common-core` is the workspace's only permitted zero-domain crate: generic
utilities with no knowledge of nodes, sessions, targets, embeddings, or
plugins. Anything two or more crates need (hashing, bounded caches,
registries, retry math, runtime bridges, metrics) is promoted here once the
second consumer appears — never speculatively, never copied.

## What belongs / what does not belong

Belongs: `LoadCache` / `ArcLoadCache` bounded get-or-load caches,
`KeyedRegistry` / `ConcurrentRegistry` keyed stores, `LatencyHistogram`
lock-free latency buckets, `retry_async` jittered backoff,
`block_on` / `block_on_shared` runtime bridges, `CapabilityRegistry`
interned bitsets, `BitSetDrift`, `score::ScoreMatrix` generic math and the
closed `score::ScoreDimension` enum, `hash` fingerprinting, `sync`
poison-safe lock helpers.

Does not belong (rejected at review): domain logic of any kind; SQLite
mechanics (owned by `fluent-db::sqlite`); second copies of anything in the
table below. `common-core` imports no `guidance-*`, `coral-*`, `fluent-*`,
or `dag` crate.

## Consumers (verified `Cargo.toml` dependents)

`fluent-wvr`, `fluent-concurrency`, `fluent-dag`, `fluent-db`,
`guidance-ontology`, `fluent-llm`, `fluent-router`, `spacy-rs`,
`coral-context`, `fluent-knowledge`, `search-vector`, and others. The
prelude (`common_core::prelude`) covers the 80% import.

## End-to-end capabilities

A second crate needing shared ownership, retry, caching, or metrics
gets it without building infrastructure: bounded get-or-load caches
with eviction math, keyed registries safe for concurrent use,
jittered retry with deadline math, runtime bridges that never strand
pooled connections, and lock-free latency histograms — each promoted
here only after its second consumer, each composable without an
async runtime, a database, or a model.

## Non-goals

- No async runtime, no I/O driver, no database, no model calls.
- No speculative promotion: one live consumer never justifies a shared
  primitive; the second consumer moves to the shared home.
- No stringly categories where the enum exists: new code matches on
  `ScoreDimension`, never on raw `"coherence"` literals.
