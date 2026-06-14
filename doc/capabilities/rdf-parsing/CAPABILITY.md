---
name: rdf-parsing
description: Standalone RDF parser crate with Turtle lexer, recursive-descent parser, N-Quads streaming parser, and IRI/blank-node normalizer
anchors:
  - Lexer
  - Parser
  - NQuadsParser
  - Normalizer
  - Triple
  - Term
  - RdfError
---

# RDF Parsing

`src/rdf/` is a standalone crate (`guidance-rdf`) providing a full RDF parsing stack: Turtle lexer, recursive-descent parser, N-Quads streaming parser, and IRI/blank-node normalizer.

## Architecture

```
Raw RDF text (Turtle or N-Quads)
  → Lexer::new(input).next_token()    — token stream
  → Parser::new(lex).parse_triples()  — Vec<Triple>
  → NQuadsParser::parse_line(line)    — Quad (per-line streaming)
  → normalize::hash_iri() / hash_blank_node()  — normalized identifiers
```

## Modules

| Module | Lines | Purpose |
|--------|-------|---------|
| `lexer.rs` | 668 | Full tokenizer for Turtle/N-Quads: IRI, PrefixedName, BlankNode, Literal, LangTag, DatatypeMarker, keywords, punctuation |
| `parser.rs` | 538 | Recursive-descent Turtle parser: triples, blank node chains, collections, `@prefix`, `a` shorthand |
| `nquads.rs` | 217 | N-Quads line-by-line streaming parser with optional graph component |
| `normalize.rs` | 176 | IRI and blank-node normalization via hashing |

## Core types

### Triple and Term

```rust
use guidance_rdf::parser::{Triple, Term, Literal};

let triple = Triple {
    subject: Term::Iri("http://example.org/Zig".into()),
    predicate: Term::Iri("http://www.w3.org/2000/01/rdf-schema#label".into()),
    object: Term::Literal(Literal {
        value: "Zig".into(),
        lang: None,
        datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
    }),
};
```

### Quad (N-Quads)

```rust
use guidance_rdf::nquads::{Quad, NQuadsParser};

let quad = NQuadsParser::parse_line(
    "<http://example.org/Zig> <http://www.w3.org/2000/01/rdf-schema#label> \"Zig\" ."
).unwrap().unwrap();

assert!(quad.graph.is_none());  // no named graph
```

## Lexer

Full tokenizer supporting Turtle and N-Quads token streams:

```rust
use guidance_rdf::lexer::{Lexer, TokenKind};

let mut lex = Lexer::new("<http://example.org/Zig> a <http://schema.org/ProgrammingLanguage> .");
assert_eq!(lex.next_token()?.kind, TokenKind::Iri);
assert_eq!(lex.next_token()?.kind, TokenKind::Keyword);  // "a" shorthand
assert_eq!(lex.next_token()?.kind, TokenKind::Iri);
assert_eq!(lex.next_token()?.kind, TokenKind::Dot);
```

Token kinds: `Iri`, `PrefixedName`, `BlankNode`, `BlankNodeOpen`, `BlankNodeClose`, `Literal`, `LangTag`, `DatatypeMarker`, `Keyword`, `Dot`, `Semicolon`, `Comma`, `OpenParen`, `CloseParen`, `Eof`.

## Parser

Recursive-descent Turtle parser supporting:
- Triple patterns with subject-predicate-object
- Blank node chains (`_:b1 p1 o1 ; p2 o2`)
- Collections (`(a b c)`)
- `@prefix` declarations
- `a` shorthand for `rdf:type`
- Prefixed names (`foaf:name`)

```rust
use guidance_rdf::parser::Parser;

let turtle = r#"
    @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
    <http://example.org/Zig> rdf:type <http://schema.org/ProgrammingLanguage> ;
        <http://www.w3.org/2000/01/rdf-schema#label> "Zig" .
"#;

let mut parser = Parser::new(turtle)?;
let triples = parser.parse_triples()?;
assert_eq!(triples.len(), 2);
```

## NQuadsParser

Line-by-line streaming parser for N-Quads format:

```rust
use guidance_rdf::nquads::NQuadsParser;

// Parse a single line
let quad = NQuadsParser::parse_line(
    "<http://example.org/Zig> <http://www.w3.org/2000/01/rdf-schema#label> \"Zig\" <http://example.org/graph1> ."
)?.unwrap();

assert_eq!(quad.graph, Some(Term::Iri("http://example.org/graph1".into())));
```

## Normalizer

IRI and blank-node normalization for consistent identifiers:

```rust
use guidance_rdf::normalize::{hash_iri, hash_blank_node};

let iri_hash = hash_iri("http://example.org/Zig");
let bn_hash = hash_blank_node("_:b1");
```

## Error handling

```rust
use guidance_rdf::RdfError;

// RdfError variants:
// - UnterminatedIRI
// - UnterminatedLiteral
// - InvalidEscape
// - UnexpectedChar { line, col }
// - UnexpectedEOF
// - UnexpectedToken { line, col, expected, got }
// - InvalidPrefix
// - OutOfMemory
```

## Key files

- `rdf/src/lexer.rs` — `Lexer`, `Token`, `TokenKind`
- `rdf/src/parser.rs` — `Parser`, `Triple`, `Term`, `Literal`
- `rdf/src/nquads.rs` — `NQuadsParser`, `Quad`
- `rdf/src/normalize.rs` — `hash_iri`, `hash_blank_node`
- `rdf/src/lib.rs` — `RdfError`, `RDF_TYPE`, `XSD_NS` constants

## Dependencies

- Zero external dependencies (self-contained parser, no `nom` or `pest`)

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| Crate layout | `src/rdf/` with `lexer.zig`, `parser.zig`, `nquads.zig`, `normalize.zig` | Standalone `guidance-rdf` crate with same module structure |
| Error handling | Error unions | `thiserror`-derived `RdfError` enum |
| Memory | `ArenaAllocator` for parser temporaries | Scoped `Vec`/`String` allocations, RAII drop |
| Streaming | `NQuadsParser` with iterator interface | `NQuadsParser::parse_line()` per-line API |
| IRI normalization | `normalizeIRI` function | `hash_iri` / `hash_blank_node` for stable identifiers |

## Zig reference

See `doc/capabilities/rdf-parsing/CAPABILITY.md` in the Zig project for the original Turtle parser, N-Quads streaming, and IRI normalization design.
