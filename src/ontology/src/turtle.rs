//! The one Turtle `rdfs:subClassOf` reader for the ontology crate.
//!
//! The taxonomy converter and the runtime [`YagoView`](crate::yago_view::YagoView)
//! both derive their hierarchy from `rdfs:subClassOf` edges. Those edges are
//! read here, through the canonical `guidance_rdf` lexer/parser — the former
//! per-line regexes and the view's hand-rolled `parse_subclass` are gone.
//!
//! [`subclass_edges`] preserves streaming: it pulls triples one at a time via
//! `Parser::next_triple`. Parsing stops at the first malformed statement and
//! returns the edges gathered before it — a bad trailing statement drops only
//! itself and everything after.

use std::collections::BTreeMap;

use guidance_rdf::lexer::{Lexer, TokenKind};
use guidance_rdf::parser::{Parser, Term, Triple};
use guidance_rdf::RDFS_SUBCLASS_OF;

use crate::taxonomy::expand_curie;

/// Every `rdfs:subClassOf` `(subject_iri, object_iri)` edge in `ttl`, with
/// prefixed names expanded to full IRIs, plus the declared `@prefix` map.
#[must_use]
/// Every `rdfs:subClassOf` `(subject_iri, object_iri)` edge in `ttl`, with
/// prefixed names expanded to full IRIs, plus the declared `@prefix` map.
pub fn subclass_edges(ttl: &str) -> (BTreeMap<String, String>, Vec<(String, String)>) {
    let mut parser = Parser::new(ttl);
    let mut edges = Vec::new();
    while let Ok(Some(triple)) = parser.next_triple() {
        if is_subclass_triple(&triple) {
            if let (Some(subject), Some(object)) = (iri_of(&triple.subject), iri_of(&triple.object))
            {
                edges.push((subject, object));
            }
        }
    }
    let prefixes = parser
        .prefixes()
        .iter()
        .map(|(prefix, iri)| (prefix.clone(), iri.clone()))
        .collect();
    (prefixes, edges)
}

/// The `rdfs:subClassOf` edges declared on a single line, CURIEs expanded
/// through `prefixes`. The line-oriented pruner uses this so it does not need
/// a second parser; the canonical lexer does the tokenization.
#[must_use]
/// The `rdfs:subClassOf` edges declared on a single line, CURIEs expanded
/// through `prefixes`. The line-oriented pruner uses this so it does not need
/// a second parser; the canonical lexer does the tokenization.
pub fn subclass_edges_in_line(
    line: &str,
    prefixes: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let mut lex = Lexer::new(line);
    let mut subject: Option<String> = None;
    let mut edges = Vec::new();
    while let Ok(token) = lex.next_token() {
        if token.kind == TokenKind::Eof {
            break;
        }
        if is_subclass_predicate(token.kind, token.value, prefixes) {
            let Ok(object) = lex.next_token() else { break };
            if matches!(object.kind, TokenKind::Iri | TokenKind::PrefixedName) {
                if let Some(subject) = subject.take() {
                    edges.push((subject, expand_curie(object.value, prefixes)));
                    continue;
                }
            }
            subject = None;
            continue;
        }
        match token.kind {
            TokenKind::Iri | TokenKind::PrefixedName => {
                subject = Some(expand_curie(token.value, prefixes));
            }
            _ => subject = None,
        }
    }
    edges
}

/// Whether the token expands to the `rdfs:subClassOf` predicate IRI under the
/// declared prefixes — matching the streaming parser's own expansion.
fn is_subclass_predicate(
    kind: TokenKind,
    value: &str,
    prefixes: &BTreeMap<String, String>,
) -> bool {
    matches!(kind, TokenKind::Iri | TokenKind::PrefixedName)
        && expand_curie(value, prefixes) == RDFS_SUBCLASS_OF
}

fn is_subclass_triple(triple: &Triple) -> bool {
    matches!(&triple.predicate, Term::Iri(iri) if iri == RDFS_SUBCLASS_OF)
}

fn iri_of(term: &Term) -> Option<String> {
    match term {
        Term::Iri(iri) => Some(iri.clone()),
        _ => None,
    }
}