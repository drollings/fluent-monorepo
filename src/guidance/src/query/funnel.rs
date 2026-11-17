//! Pure term funnel: significant tokens → alias expansion →
//! concept normalization → symbol names. One home for the
//! vocabulary pipeline the capability route and traversal consume
//! (wired: `recall_capability_briefs` funnels every query). Every stage is total and
//! deterministic; the concept stage keeps unknowns (fail-open, the
//! member-groundedness discipline) instead of filtering them.

use crate::query::aliases::SemanticAliases;
use fluent_concept::ConceptStore;

/// Question scaffolding and prepositions that never carry topic
/// meaning. Single home for the relevance gate's list: the gate
/// delegates here, so the rule cannot drift between the two.
pub const SCAFFOLDING_STOPWORDS: &[&str] = &[
    "what", "where", "which", "who", "whom", "when", "why", "how", "does", "are", "was", "were",
    "the", "has", "have", "had", "will", "would", "should", "could", "there", "their", "about",
    "into", "over", "under", "from", "that", "this", "these", "those", "with", "and", "for",
    "your", "you",
];

/// Significant query tokens: lowercase word tokens of length 3 or
/// more, minus scaffolding stopwords. Same rule as the relevance
/// gate (which delegates here).
#[must_use]
/// Significant query tokens: lowercase word tokens of length 3 or
/// more, minus scaffolding stopwords. Same rule as the relevance
/// gate (which delegates here).
pub fn significant_terms(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in crate::query::tokens::split_tokens(query) {
        if token.len() < 3 || SCAFFOLDING_STOPWORDS.contains(&token.as_str()) {
            continue;
        }
        crate::query::tokens::lower_dedup_push(&mut tokens, &token);
    }
    tokens
}

/// One expanded term's concept normalization: the term, its variant
/// curies, and whether any curie resolves in the store. Unknown
/// stays (kept, marked) — never filtered.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One expanded term's concept normalization: the term, its variant
/// curies, and whether any curie resolves in the store. Unknown
/// stays (kept, marked) — never filtered.
pub struct FunneledConcept {
    /// Expanded term (lowercased).
    pub term: String,
    /// Variant curies (`token_variants` × `lemma_to_curies`),
    /// deduped, stable order.
    pub curies: Vec<String>,
    /// True when any curie resolves in the store; false without a
    /// store (fail-open) or when nothing resolves.
    pub grounded: bool,
}

/// The four funnel stages for one query, in pipeline order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// The four funnel stages for one query, in pipeline order.
pub struct FunneledTerms {
    /// Significant tokens (gate rule).
    pub significant: Vec<String>,
    /// Alias union over significant terms (per-token `expand`,
    /// first-seen order — cartesian phrase products would not be
    /// terms, so the union, not `expand_query`, is collected here).
    pub expanded: Vec<String>,
    /// Concept normalization over expanded terms (unknown stays).
    pub concepts: Vec<FunneledConcept>,
    /// `symbol_names` over significant tokens only — scaffolding is
    /// not identifier intent.
    pub symbols: Vec<String>,
}

/// Build the funnel for one query. Pure: no I/O, no model, never
/// panics. A missing store reads everything as unknown (fail-open);
/// an empty query funnels to empty.
#[must_use]
/// Build the funnel for one query. Pure: no I/O, no model, never
/// panics. A missing store reads everything as unknown (fail-open);
/// an empty query funnels to empty.
pub fn funnel_terms(
    query: &str,
    aliases: &SemanticAliases,
    store: Option<&dyn ConceptStore>,
) -> FunneledTerms {
    let significant = significant_terms(query);
    let mut expanded = Vec::new();
    for token in &significant {
        for variant in aliases.expand(token) {
            if !expanded.contains(&variant) {
                expanded.push(variant);
            }
        }
    }
    let concepts = expanded
        .iter()
        .map(|term| {
            let mut curies = Vec::new();
            for variant in guidance_ontology::normalize::token_variants(term) {
                for curie in guidance_ontology::normalize::lemma_to_curies(&variant) {
                    if !curies.contains(&curie) {
                        curies.push(curie);
                    }
                }
            }
            let grounded = store
                .is_some_and(|store| curies.iter().any(|curie| store.resolve_name(curie).is_ok()));
            FunneledConcept {
                term: term.clone(),
                curies,
                grounded,
            }
        })
        .collect();
    let symbols = crate::query::tokens::symbol_names(&significant.join(" "));
    FunneledTerms {
        significant,
        expanded,
        concepts,
        symbols,
    }
}

impl FunneledTerms {
    /// True when every stage is empty (empty or all-scaffolding query).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.significant.is_empty()
            && self.expanded.is_empty()
            && self.concepts.is_empty()
            && self.symbols.is_empty()
    }

    /// One stable line per stage, in pipeline order; empty for an
    /// empty funnel. Pinned by goldens.
    #[must_use]
    pub fn stage_log(&self) -> Vec<String> {
        if self.is_empty() {
            return Vec::new();
        }
        let concepts = self
            .concepts
            .iter()
            .map(|concept| {
                format!(
                    "{} -> {} ({})",
                    concept.term,
                    concept.curies.join(", "),
                    if concept.grounded {
                        "grounded"
                    } else {
                        "unknown"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        vec![
            format!("significant: [{}]", self.significant.join(", ")),
            format!("expanded: [{}]", self.expanded.join(", ")),
            format!("concepts: [{concepts}]"),
            format!("symbols: [{}]", self.symbols.join(", ")),
        ]
    }
}

#[cfg(test)]
#[path = "../../tests/query_funnel.rs"]
mod tests;
