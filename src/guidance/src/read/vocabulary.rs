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
    "get", "gets", "give", "gives", "take", "takes", "use", "uses", "using", "run", "runs",
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

#[cfg(test)]
#[path = "../../tests/read_vocabulary.rs"]
mod read_vocabulary_tests;
