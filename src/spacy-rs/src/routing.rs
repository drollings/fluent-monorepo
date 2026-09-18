//! DEP-as-routing-signal extraction (walkthrough §10.4; roadmap §6).
//!
//! Given an **attached** [`Doc`] (deps/heads set by the LLM rung or the rule
//! rung), each sentence is reduced to a [`RoutingSignal`]: the predicate
//! (root lemma), the argument frame (`nsubj`/`dobj`/`iobj` pick the tool and
//! its arguments; `compound`/`amod`/`nummod` carry modifiers; `aux`/`neg`/
//! `advmod` qualify intent; `prep` + `pobj` carry the argument frame), plus a
//! per-token transcript (tokens/lemmas/pos/deps/heads) for the ledger
//! `ContentNode` metadata.
//!
//! Role classification compares the token's dep hash against the closed role
//! vocabulary (`hash_utf8` of the label) — no `StringStore` round-trip on the
//! hot classification path. The transcript strings (lemmas/deps) resolve
//! through the store, which [`crate::llm::attach`] populates by interning the
//! strings it hashes.
//!
//! The deterministic rule rung produces a **star parse** (one `root` per
//! sentence, everything else a flat `dep`), so a doc annotated without the LLM
//! degrades gracefully: the signal carries the predicate and the residual
//! `dependents`, with the role slots empty. The extraction itself is
//! dep-complete — a full UD parse fills every slot.

use serde::{Deserialize, Serialize};

use crate::doc::{dep_in, lemma_of, sentence_roles, Doc, SentenceRoles};
use crate::labels::{MODIFIER_DEPS, POBJ_DEPS, PREP_DEPS, QUALIFIER_DEPS};
use fluent_types::InterlinguaId;

/// The interlingua frame of one sentence: the content-
/// addressed ids of the routing-relevant tokens. This is what lets Coral
/// Router dispatch deterministically on the parse — the same phrasing
/// collapses to the same predicate/object ids regardless of surface words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterlinguaSignal {
    /// The predicate (root) token's lemma id.
    pub predicate_id: Option<InterlinguaId>,
    /// The subject token's lemma id.
    pub subject_id: Option<InterlinguaId>,
    /// The direct-object token's lemma id.
    pub direct_object_id: Option<InterlinguaId>,
    /// The indirect-object token's lemma id.
    pub indirect_object_id: Option<InterlinguaId>,
    /// PROPN tokens with a YaGO match (their `interlingua_entity_id`), for
    /// concept-aware routing and review.
    pub concept_ids: Vec<InterlinguaId>,
    /// Per-token lemma ids, aligned with the `tokens` transcript. A token
    /// whose lemma was never resolved carries `InterlinguaId(0)` (the
    /// RESERVED namespace id — the documented none-sentinel).
    pub token_ids: Vec<InterlinguaId>,
    /// Per-sentence parse confidence (the mean of the sentence tokens'
    /// confidence; `None` when the parse carried none). The `confidence_min`
    /// gate on `match_interlingua` filters reads this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// The routing frame of one sentence, plus its per-token transcript.
///
/// Serde-serializable so it can ride in `StageDecision.metadata` and be stored
/// in a ledger `ContentNode`'s metadata verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingSignal {
    /// The sentence text (token orths joined with spaces).
    pub sentence: String,
    /// The predicate — the root token's lemma (a verb, or the nominal head of
    /// a verbless sentence).
    pub predicate: String,
    /// The `nsubj`/`nsubjpass`/`csubj` subject's lemma.
    pub subject: Option<String>,
    /// The `dobj` direct object's lemma.
    pub direct_object: Option<String>,
    /// The `iobj` indirect object's lemma.
    pub indirect_object: Option<String>,
    /// `compound`/`amod`/`nummod` modifier lemmas.
    pub modifiers: Vec<String>,
    /// `aux`/`auxpass`/`neg`/`advmod` intent qualifiers.
    pub qualifiers: Vec<String>,
    /// `prep` + `pobj` argument frames: `(preposition lemma, object lemma)`.
    pub arguments: Vec<(String, String)>,
    /// Residual lemmas whose dep is not one of the role labels (the flat
    /// `dep` children of the star parse land here).
    pub dependents: Vec<String>,
    /// Per-token surface forms.
    pub tokens: Vec<String>,
    /// Per-token lemmas.
    pub lemmas: Vec<String>,
    /// Per-token UPOS names (lowercase).
    pub pos: Vec<String>,
    /// Per-token dep labels.
    pub deps: Vec<String>,
    /// Per-token relative head offsets.
    pub heads: Vec<i32>,
    /// The interlingua frame (ids), when the doc carries resolved ids
    /// `None` for an unresolveable sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interlingua: Option<InterlinguaSignal>,
}

/// Extract one [`RoutingSignal`] per sentence of an attached `doc`.
///
/// Sentence boundaries come from the `sent_start` markers (set by the
/// sentencizer and by `set_children_from_heads` at each root). A doc with no
/// markers is treated as a single sentence. Requires an attached doc (deps and
/// heads set); a raw tokenizer output yields degenerate signals (first token
/// as predicate, everything else a dependent).
#[must_use]
pub fn extract_routing_signals(doc: &Doc) -> Vec<RoutingSignal> {
    if doc.is_empty() {
        return Vec::new();
    }
    // Shared sentence walk: partition + root + first-match role slots come
    // from the single `sentence_roles` traversal; this maps the slots to
    // lemmas and collects the routing-only extras (modifiers, qualifiers,
    // prep/pobj argument frames, residuals, concept ids, transcript).
    sentence_roles(doc)
        .iter()
        .map(|roles| signal_for_sentence(doc, roles))
        .collect()
}

/// Build the signal for one sentence's precomputed [`SentenceRoles`].
fn signal_for_sentence(doc: &Doc, roles: &SentenceRoles) -> RoutingSignal {
    let (start, end) = roles.span;
    let root = roles.root;

    let strings = doc.vocab().strings();
    let dep_of = |i: usize| -> String {
        strings
            .get(doc.token(i).dep)
            .map(|s| s.to_string())
            .unwrap_or_default()
    };

    // Role slots from the shared walk: the last candidate wins (the
    // historical overwrite chain below assigned on every match, so the
    // final match survived). Lemmas resolved here.
    let subject = roles.subjects.last().map(|&i| lemma_of(doc, i));
    let subject_idx = roles.subjects.last().copied();
    let direct_object = roles.direct_objects.last().map(|&i| lemma_of(doc, i));
    let direct_object_idx = roles.direct_objects.last().copied();
    let indirect_object = roles.indirect_objects.last().map(|&i| lemma_of(doc, i));
    let indirect_object_idx = roles.indirect_objects.last().copied();
    let mut modifiers = Vec::new();
    let mut qualifiers = Vec::new();
    let mut arguments = Vec::new();
    let mut dependents = Vec::new();
    let mut concept_ids = Vec::new();

    for i in start..end {
        if i == root {
            continue;
        }
        let token = doc.token(i);
        // Slot tokens were recorded above; they never populate the
        // modifier/qualifier/argument/residual collections (the same
        // if/else precedence the inline chain had: a slot match never
        // reached the collections below).
        let is_slot = roles.subjects.contains(&i)
            || roles.direct_objects.contains(&i)
            || roles.indirect_objects.contains(&i);
        if !is_slot {
            if dep_in(token, MODIFIER_DEPS) {
                modifiers.push(lemma_of(doc, i));
            } else if dep_in(token, QUALIFIER_DEPS) {
                qualifiers.push(lemma_of(doc, i));
            } else if dep_in(token, PREP_DEPS) {
                let prep = lemma_of(doc, i);
                for &c in &doc.children(i) {
                    if dep_in(doc.token(c), POBJ_DEPS) {
                        arguments.push((prep.clone(), lemma_of(doc, c)));
                    }
                }
            } else if dep_in(token, POBJ_DEPS) {
                // A `pobj` is captured under its `prep`'s argument frame above;
                // not a standalone role.
            } else {
                dependents.push(lemma_of(doc, i));
            }
        }
        if let Some(ent) = token.interlingua_entity_id {
            if ent.is_yago() {
                concept_ids.push(ent);
            }
        }
    }

    let tokens: Vec<String> = (start..end).map(|i| doc.token_text(i)).collect();
    let sentence = tokens.join(" ");
    let lemmas = (start..end).map(|i| lemma_of(doc, i)).collect();
    let pos = (start..end).map(|i| doc.token(i).pos.to_string()).collect();
    let deps = (start..end).map(dep_of).collect();
    let heads = (start..end).map(|i| doc.token(i).head).collect();

    // Sentence confidence: the mean of the tokens' confidence (DRY — reads
    // `TokenRecord.confidence`, never recomputes a second copy). `None` when
    // the parse carried no per-token confidence (LLM/rule rungs).
    let sentence_confidence = {
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for i in start..end {
            if let Some(c) = doc.token(i).confidence {
                sum += c;
                count += 1;
            }
        }
        if count == 0 {
            None
        } else {
            Some(sum / count as f64)
        }
    };

    // The interlingua frame: content-addressed ids of the routing-relevant
    // roles plus the aligned per-token lemma ids (`InterlinguaId(0)` = none).
    let none_id = InterlinguaId::from_u64(0);
    let id_of = |i: usize| doc.token(i).interlingua_lemma_id.unwrap_or(none_id);
    let interlingua = Some(InterlinguaSignal {
        predicate_id: doc.token(root).interlingua_lemma_id,
        subject_id: subject_idx.map(id_of),
        direct_object_id: direct_object_idx.map(id_of),
        indirect_object_id: indirect_object_idx.map(id_of),
        concept_ids,
        token_ids: (start..end).map(id_of).collect(),
        confidence: sentence_confidence,
    });

    RoutingSignal {
        sentence,
        predicate: lemma_of(doc, root),
        subject,
        direct_object,
        indirect_object,
        modifiers,
        qualifiers,
        arguments,
        dependents,
        tokens,
        lemmas,
        pos,
        deps,
        heads,
        interlingua,
    }
}

#[cfg(test)]
#[path = "../tests/routing.rs"]
mod tests;
