---
name: ontology
description: Standalone ontology crate with RDF-to-ContextNode mapping, YAGO taxonomy, rule-based inference engine, and named entity extraction
anchors:
  - TripleMapper
  - InferenceEngine
  - YagoTaxonomy
  - EntityExtractor
  - PendingNode
  - PendingEdge
---

# Ontology

`src/ontology/` is a standalone crate (`guidance-ontology`) providing entity extraction, capability inference, and YAGO taxonomy integration for semantic knowledge representation.

## Architecture

```
RDF Triples (from guidance-rdf)
  → TripleMapper::map_triple() → PendingNode + PendingEdge pairs
  → InferenceEngine::infer()   → expanded triple set (subclass transitivity, etc.)
  → YagoTaxonomy               → class/property resolution
  → EntityExtractor             → named entity frequency analysis
  → ContextNode                 → insertion into Library database
```

## Modules

| Module | Lines | Purpose |
|--------|-------|---------|
| `mapper.rs` | 420 | `TripleMapper`, `PendingNode`, `PendingEdge` — maps RDF triples to `ContextNode` pairs with 6-level LOD |
| `inference.rs` | 370 | `InferenceEngine`, `InferenceRule` — rule-based inference: subclass transitivity, subproperty transitivity, domain/range, inverse-of |
| `yago.rs` | 402 | `YagoTaxonomy`, `OntologyClass`, `OntologyProperty` — YAGO 4.5 class/property definitions with IRI constants |
| `entity.rs` | 143 | `EntityExtractor`, `EntityFreq` — named entity extraction from text using stoplist filtering |
| `migration.rs` | 84 | Schema migration helpers for `coral_nodes` / `coral_edges` tables |

## TripleMapper

Converts `(subject, predicate, object)` RDF triples into `ContextNode` pairs with pre-computed LOD slices. Uses YAGO class resolution for type-aware mapping.

```rust
use guidance_ontology::mapper::TripleMapper;

let mapper = TripleMapper::new();
let (subject_node, object_node) = mapper.map_triple("Zig", "is_a", "language");
assert_eq!(subject_node.name.as_str(), "Zig");
assert_eq!(object_node.name.as_str(), "language");
```

Each mapped node carries 6 LOD levels (`LOD_COUNT = 6`): full triple string, edge context, entity name, and progressively shorter summaries.

## InferenceEngine

Rule-based inference engine that expands a triple set using OWL/RDFS rules:

```rust
use guidance_ontology::inference::{InferenceEngine, InferenceRule, RuleType};

let mut engine = InferenceEngine::new();
engine.add_rule(InferenceRule {
    rule_type: RuleType::SubclassTransitivity,
    trigger_predicate: "http://www.w3.org/2000/01/rdf-schema#subClassOf".into(),
});

let inferred = engine.infer(&triples)?;
// Adds transitive subclass triples
```

### Supported rule types

| RuleType | Effect |
|----------|--------|
| `SubclassTransitivity` | If A subclass-of B and B subclass-of C, infer A subclass-of C |
| `SubpropertyTransitivity` | If P subproperty-of Q, infer Q triples from P triples |
| `DomainRange` | Infer type triples from property domain/range declarations |
| `InverseOf` | Infer inverse triples for symmetric properties |

## YagoTaxonomy

YAGO 4.5 class and property definitions with IRI constants:

```rust
use guidance_ontology::yago::{NS_YAGO, NS_RDFS, YAGO_VERSION, OntologyClass};

assert_eq!(YAGO_VERSION, "4.5");
assert_eq!(NS_YAGO, "http://yago-knowledge.org/resource/");
```

Provides `YAGO_CLASS` and `YAGO_PROPERTY` maps for class hierarchy resolution, property domain/range lookup, and LOD target assignment.

## EntityExtractor

Named entity extraction using stoplist filtering:

```rust
use guidance_ontology::entity::EntityExtractor;

let extractor = EntityExtractor::new();
let entities = extractor.extract("Einstein developed the theory of relativity at Princeton");
// Returns [("Einstein", Person, 1), ("Princeton", Location, 1)]
```

Entity types: `Person`, `Project`, `Location`, `Uncertain`.

## Key files

- `ontology/src/mapper.rs` — `TripleMapper`, `PendingNode`, `PendingEdge`, `LOD_COUNT`
- `ontology/src/inference.rs` — `InferenceEngine`, `InferenceRule`, `RuleType`
- `ontology/src/yago.rs` — YAGO namespace constants, `OntologyClass`, `OntologyProperty`, `PropertyRange`
- `ontology/src/entity.rs` — `EntityExtractor`, `EntityFreq`, `EntityType`
- `ontology/src/migration.rs` — Schema migration helpers
- `ontology/src/lib.rs` — `OntologyError` enum

## Dependencies

- `guidance-rdf` — `Triple`, `Term` types from the RDF parser
- `guidance-types` — `ContextNode`, `NodeId`

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| Crate layout | `ontology/mapper.zig`, `ontology/inference.zig`, `ontology/yago.zig` | Standalone `guidance-ontology` crate with `mapper`, `inference`, `yago` modules |
| Triple type | `Triple { subject, predicate, object: Term }` | `guidance_rdf::parser::Triple` (same structure) |
| YAGO integration | YAGO 4.5 with whitelist filtering | YAGO 4.5 constants and class/property maps |
| Inference rules | Hardcoded rule set | Configurable `InferenceEngine` with `add_rule()` |
| Entity extraction | Not present | `EntityExtractor` with stoplist-based filtering |
| Schema migration | Not present | `migration.rs` for SQLite schema evolution |

## Zig reference

See `doc/capabilities/ontology/CAPABILITY.md` in the Zig project for the original `TripleMapper`, `MappingConfig`, `FlushResult`, and full YAGO integration.
