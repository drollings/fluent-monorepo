---
name: rdf-parsing
description: Zig RDF parsing module covering Turtle (lexer + recursive-descent parser), N-Quads streaming parser, and IRI normalization. Produces Triple/Term values used by the ontology mapper to ingest YAGO 4.5 and other RDF datasets.
anchors:
  - Parser
  - Triple
  - Term
  - NQuadsParser
---

# RDF Parsing

`src/rdf/` is a named Zig module providing streaming RDF parsers for the Coral ingestion pipeline.

## Supported formats

| Format | Parser | Notes |
|--------|--------|-------|
| Turtle (`.ttl`) | `rdf.Parser` (recursive descent) | Full prefix/base IRI resolution |
| N-Quads (`.nq`, `.nt`) | `rdf.NQuadsParser` | Streaming, line-oriented |

## Core types

```zig
pub const Term = union(enum) {
    iri: []const u8,
    blank: []const u8,
    literal: struct { value: []const u8, datatype: []const u8, lang: []const u8 },
};

pub const Triple = struct {
    subject: Term,
    predicate: Term,
    object: Term,
};
```

## Normalization

`rdf.normalize` provides IRI canonicalization and blank-node skolemization, used by `TripleMapper` before inserting into the Library.

## Sub-modules

- `src/rdf/lexer.zig` — Turtle tokenizer
- `src/rdf/parser.zig` — `Parser`, `Triple`, `Term`
- `src/rdf/nquads.zig` — `NQuadsParser` (streaming)
- `src/rdf/normalize.zig` — IRI normalization, blank-node handling
- `src/rdf/root.zig` — umbrella re-exports `Parser`, `Triple`, `Term`

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (6 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/rdf/parser.zig` | 1.0 | defines_anchor |
| `src/rdf/nquads.zig` | 1.0 | defines_anchor |
| `src/rdf/normalize.zig` | 0.9 | used_by |
| `src/rdf/root.zig` | 0.9 | used_by |
| `src/rdf/lexer_tests.zig` | 0.4 | path_heuristic |
| `src/rdf/lexer.zig` | 0.4 | path_heuristic |

