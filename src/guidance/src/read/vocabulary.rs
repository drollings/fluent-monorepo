//! One query vocabulary for the read plane.
//!
//! Term extraction, light-verb removal, alias expansion, and symbol
//! extraction live here. The significant-term rule is shared with the
//! funnel (`query::funnel::significant_terms` is the single logic
//! site — this module is the read-plane facade, and the binary's
//! `significant_query_tokens` delegates here). Alias expansion reuses
//! the repository-neutral `SemanticAliases`; there is no IDF table in
//! the tree, so ordering is first-seen deterministic everywhere.
//!
//! Light verbs (`decide`, `handle`, `work`, `do`, `make`, …) are
//! generic action words that hijack retrieval when treated as topic
//! terms. They leave the term set unless they are the only content
//! term left.

use search_vector::aliases::SemanticAliases;

/// Generic action words: topic-free unless they are all that remains.
pub const LIGHT_VERBS: &[&str] = &[
    "decide", "decides", "handle", "handles", "work", "works", "do", "does", "make", "makes",
    "get", "gets", "give", "gives", "take", "takes", "use", "uses", "using", "run", "runs", "own",
    "owns", "owned", "owning",
];

/// Significant query terms: the shared funnel rule (lowercase
/// word tokens of length 3+, minus scaffolding stopwords).
#[must_use]
/// Significant query terms: the shared funnel rule (lowercase
/// word tokens of length 3+, minus scaffolding stopwords).
pub fn query_terms(query: &str) -> Vec<String> {
    crate::query::funnel::significant_terms(query)
}

/// Content terms: significant terms minus light verbs, unless
/// nothing else remains (then the verbs stay — dropping everything
/// would answer nothing).
#[must_use]
/// Content terms: significant terms minus light verbs, unless
/// nothing else remains (then the verbs stay — dropping everything
/// would answer nothing).
pub fn content_terms(query: &str) -> Vec<String> {
    let terms = query_terms(query);
    let kept: Vec<String> = terms
        .iter()
        .filter(|term| !LIGHT_VERBS.contains(&term.as_str()))
        .cloned()
        .collect();
    if kept.is_empty() {
        terms
    } else {
        kept
    }
}

/// Alias expansion over content terms in first-seen order (per-token
/// union, never cartesian phrase products).
#[must_use]
/// Alias expansion over content terms in first-seen order (per-token
/// union, never cartesian phrase products).
pub fn expand_terms(query: &str, aliases: &SemanticAliases) -> Vec<String> {
    let mut expanded = Vec::new();
    for token in content_terms(query) {
        for variant in aliases.expand(&token) {
            if !expanded.contains(&variant) {
                expanded.push(variant);
            }
        }
    }
    expanded
}

/// Identifier-shaped terms for symbol-scoped intents only
/// (`Symbol`/`Graph` — see `read::intent::extracts_symbols`).
/// Scaffolding never reaches this set: it runs over significant
/// terms, not raw query text.
#[must_use]
/// Identifier-shaped terms for symbol-scoped intents only
/// (`Symbol`/`Graph` — see `read::intent::extracts_symbols`).
/// Scaffolding never reaches this set: it runs over significant
/// terms, not raw query text.
pub fn symbol_terms(query: &str) -> Vec<String> {
    search_vector::tokens::symbol_names(&query_terms(query).join(" "))
}

/// Scaffolding words that name no subject: they scope the question,
/// never the answer. `crate` is the trap — without this list the
/// ownership anchor becomes the word "crate" and every crate card
/// renders with no description.
pub const SUBJECT_STOPLIST: &[&str] = &[
    "crate",
    "component",
    "codebase",
    "repo",
    "file",
    "module",
    "function",
    "project",
    "workspace",
];

/// Where one subject anchor came from: an exact known crate name or
/// a free identifier-shaped term. Crate anchors filter; free terms
/// stay shadow until measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Where one subject anchor came from: an exact known crate name or
/// a free identifier-shaped term. Crate anchors filter; free terms
/// stay shadow until measured.
pub enum AnchorSource {
    Crate,
    Identifier,
}

/// The query subject: canonical terms plus their provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
/// The query subject: canonical terms plus their provenance.
pub struct SubjectAnchor {
    /// Canonical package for a crate anchor; content terms otherwise.
    pub terms: Vec<String>,
    pub source: AnchorSource,
}

/// Resolve the query subject: an exact known crate/component/package
/// name wins (canonical package plus the matched name, `Crate`), else
/// the content terms minus the scaffolding stoplist (`Identifier`),
/// else none. First exact hit in query order wins; hyphenated names
/// stay whole.
#[must_use]
/// Resolve the query subject: an exact known crate/component/package
/// name wins (canonical package plus the matched name, `Crate`), else
/// the content terms minus the scaffolding stoplist (`Identifier`),
/// else none. First exact hit in query order wins; hyphenated names
/// stay whole.
pub fn subject_anchor(query: &str, known_crates: &[String]) -> Option<SubjectAnchor> {
    for raw in query.split_whitespace() {
        let chunk = raw
            .trim_matches(|c: char| {
                matches!(
                    c,
                    '"' | '\'' | '(' | ')' | '[' | ']' | ',' | '.' | ';' | ':' | '?' | '!'
                )
            })
            .to_lowercase();
        if chunk.is_empty() || SUBJECT_STOPLIST.contains(&chunk.as_str()) {
            continue;
        }
        if let Some(canonical) = match_crate(&chunk, known_crates) {
            return Some(SubjectAnchor {
                terms: crate_terms(canonical, &chunk),
                source: AnchorSource::Crate,
            });
        }
        for token in chunk.split(|c: char| !c.is_ascii_alphanumeric()) {
            if token.is_empty() || SUBJECT_STOPLIST.contains(&token) {
                continue;
            }
            if let Some(canonical) = match_crate(token, known_crates) {
                return Some(SubjectAnchor {
                    terms: crate_terms(canonical, token),
                    source: AnchorSource::Crate,
                });
            }
        }
    }
    let terms: Vec<String> = content_terms(query)
        .into_iter()
        .filter(|term| !SUBJECT_STOPLIST.contains(&term.as_str()))
        .collect();
    if terms.is_empty() {
        return None;
    }
    Some(SubjectAnchor {
        terms,
        source: AnchorSource::Identifier,
    })
}

/// Canonical package first, then the matched name when it differs
/// (dirname and component aliases mention code the package id never
/// names). The filter matches any term; scoping reads the first.
fn crate_terms(canonical: String, matched: &str) -> Vec<String> {
    if matched == canonical {
        vec![canonical]
    } else {
        vec![canonical, matched.to_string()]
    }
}

/// Canonical package for one anchor name, via the gate table.
fn match_crate(name: &str, known_crates: &[String]) -> Option<String> {
    if !known_crates.iter().any(|known| known == name) {
        return None;
    }
    crate::component_map::package_for_crate_name(name).map(str::to_string)
}

/// Whether a free-term anchor discriminates: it must mention fewer
/// than half of the card bodies. Crate anchors skip this — an exact
/// crate is selective by construction. The caller fails open on
/// `false`; counting rides `--trace`.
#[must_use]
/// Whether a free-term anchor discriminates: it must mention fewer
/// than half of the card bodies. Crate anchors skip this — an exact
/// crate is selective by construction. The caller fails open on
/// `false`; counting rides `--trace`.
pub fn anchor_selective(anchor: &SubjectAnchor, card_bodies: &[&str]) -> bool {
    if anchor.source == AnchorSource::Crate || card_bodies.is_empty() {
        return true;
    }
    let hits = card_bodies
        .iter()
        .filter(|body| {
            let fold = body.to_lowercase();
            anchor.terms.iter().any(|term| fold.contains(term.as_str()))
        })
        .count();
    hits * 2 <= card_bodies.len()
}

/// Shipped answer-floor coverage: half of the asked subject terms, or
/// every identifier-shaped name. Uncalibrated until the answer-floor
/// gate passes; moving it needs a recorded calibration reason.
pub const ANSWER_FLOOR_COVERAGE: f64 = 0.5;

/// Subject coverage of one evidence text in `[0, 1]`:
/// identifier-shaped names are all-or-nothing, otherwise the fraction
/// of non-scaffolding content terms present. Empty subjects cover
/// nothing — nothing was asked.
#[must_use]
pub fn subject_coverage(query: &str, evidence: &str) -> f64 {
    let fold = evidence.to_lowercase();
    let identifiers: Vec<String> = search_vector::tokens::symbol_names(query)
        .into_iter()
        .filter(|name| names_identifier(name))
        .collect();
    if !identifiers.is_empty() {
        let present = identifiers
            .iter()
            .all(|name| fold.contains(&name.to_lowercase()));
        return f64::from(u8::from(present));
    }
    let terms: Vec<String> = content_terms(query)
        .into_iter()
        .filter(|term| !SUBJECT_STOPLIST.contains(&term.as_str()))
        .collect();
    if terms.is_empty() {
        return 0.0;
    }
    let hits = terms
        .iter()
        .filter(|term| fold.contains(term.as_str()))
        .count();
    hits as f64 / terms.len() as f64
}

/// Whether one evidence text names the query subject at the shipped
/// coverage floor.
#[must_use]
pub fn evidence_names_subject(query: &str, evidence: &str) -> bool {
    subject_coverage(query, evidence) >= ANSWER_FLOOR_COVERAGE
}

/// Identifier-shaped: internal capitals (camelCase/PascalCase) or
/// separators (`_`, `::`). A sentence-start capital is prose, and a
/// bare lowercase word is a topic term, not an exact name.
fn names_identifier(name: &str) -> bool {
    name.contains('_') || name.contains("::") || name.chars().skip(1).any(char::is_uppercase)
}

#[cfg(test)]
#[path = "../../tests/read_vocabulary.rs"]
mod read_vocabulary_tests;
