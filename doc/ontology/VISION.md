# guidance-ontology — Vision

> Status: Implemented (core loader, taxonomy pipeline, YaGO view) + Partial
> (full 130k taxonomy via operator `make yago-load`; embedded registry ships
> the curated sample). There is no `doc/skills/ontology/SKILL.md`; the
> authorities are the crate docs, `doc/router/ARCHITECTURE.md`
> (one-loader/two-consumers reconciliation), and this pointer pair. Crate
> directory is `src/ontology/`; package name is `guidance-ontology`.

## Mission

`guidance-ontology` is the YaGO taxonomy and entity layer: registry loading
with deterministic content-addressed ids, TTL→JSON taxonomy conversion, the
runtime `YagoView`, and the lexicon normalization pipeline. One loader
(`YaGoLoader`) feeds two consumers (coral's durable graph + the router's
`SqliteConceptStore`); a boot reconciliation locks them equal.

## What belongs / what does not belong

Belongs: `YaGoLoader` / `RegistryEntry` loading, `TaxonomyEntry` taxonomy
maps, `CuriePrefix` closed prefix enum, `normalize_taxonomy_curie` /
`normalize_lexicon_name` / `iri_to_curie` normalization, `YagoView`
runtime view, `turtle::subclass_edges` single edge reader, `yago` static
registry, `inference` capability inference, `mapper` RDF→ContentNode,
`entity` extraction heuristic, `plausibility` scoring, `canonical`
canonical-form table, `ysm1` binary codec, `migration` version registry.

Does not belong: hand-rolled graph closures (compose
`fluent-dag::HierarchyIndex`); second decode pipelines (both normalizers
compose `normalize_extracted_name`); open-ended string prefix branches
where `CuriePrefix` matches. `spacy-rs` never depends on ontology in
production (dev-only registration for tests).

## Consumers (verified `Cargo.toml` dependents)

`coral-context` (ingest), `fluent-router` (overlay canonicalization, entity
promotion), `guidance-core` (concept grounding), `xtask`, `coral-router`
binary (boot).

## End-to-end capabilities

A pipeline needing grounded meaning gets it without owning a
taxonomy: load a versioned entity registry once with
deterministic content-addressed ids, normalize variant spellings
and namespace prefixes through one closed mapping, serve runtime
lookups over a lazily-loading view, and infer capabilities from
types — with boot-time reconciliation proving the graph store and
the router agree.

## Non-goals

- No full-taxonomy embedding: the crate ships the curated sample; the full
  130k registry is an operator download, never a build input.
- No prefix invention: arbitrary TTL `@prefix` declarations stay stringly
  by necessity; only `yago`/`schema` are closed.
- No second `ClassEntry` spelling: `TaxonomyEntry` and `RegistryEntry` are
  the canonical names, with no aliases.
