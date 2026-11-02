# guidance-ontology — Architecture

> Pointer pair authority is the crate docs (no ontology SKILL file exists).
> Every type named below exists in `src/ontology/src/`; the stale-name guard
> (`src/ontology/tests/doc_stubs.rs`) fails on invented names.

## Module table (verified against source)

| Module | Purpose | Path |
|--------|---------|------|
| `yago_loader` | `YaGoLoader`, `RegistryEntry`, `canonical_class_name`, deterministic `yago_class_id` | `src/ontology/src/yago_loader.rs` |
| `taxonomy` | `TaxonomyEntry`, `normalize_taxonomy_curie`, `collect_edges`, `build_json`, `taxonomy_from_ttl`, `to_json_string`, `to_flat`, `transitive_ancestors` (build-time memoized DFS; runtime closure is `HierarchyIndex`) | `src/ontology/src/taxonomy.rs` |
| `normalize` | `normalize_lexicon_name`, `normalize_yago_name`, `iri_to_curie`, lexicon matching | `src/ontology/src/normalize.rs` |
| `prefix` | `CuriePrefix` closed enum (`Yago`, `Schema`, open fallback), `split_prefix` | `src/ontology/src/prefix.rs` |
| `yago_view` | `YagoView` runtime view (`resolve_curie`, `ancestors_of`) | `src/ontology/src/yago_view.rs` |
| `turtle` | `subclass_edges` single Turtle reader (`pub(crate)`) | `src/ontology/src/turtle.rs` |
| `canonical` | Canonical-form table | `src/ontology/src/canonical.rs` |
| `yago` | 7 static classes, 11 static properties, whitelist | `src/ontology/src/yago.rs` |
| `inference` | `InferenceEngine`, `CapabilityInference` | `src/ontology/src/inference.rs` |
| `mapper` | `TripleMapper` RDF→ContentNode | `src/ontology/src/mapper.rs` |
| `entity` | stoplist + frequency heuristic | `src/ontology/src/entity.rs` |
| `plausibility` | taxonomy plausibility mean | `src/ontology/src/plausibility.rs` |
| `migration` | `4.5→4.6` stub + `VersionRegistry` | `src/ontology/src/migration.rs` |
| `ysm1` | YSM1 binary codec | `src/ontology/src/ysm1.rs` |

## Load-bearing invariants

- `#![forbid(unsafe_code)]`; `autotests = false` with `#[path]`-wired
  suites plus explicit `[[test]]` targets.
- Canonical entry path: `collect_edges` → `build_json` (or
  `taxonomy_from_ttl`) → `to_json_string` / `to_flat`; runtime closure only
  through `HierarchyIndex`.
- Deterministic IDs: `node_id` is the full 64-bit hash, never derived from
  the truncated id; duplicate IRIs are first-wins, never duplicated.
