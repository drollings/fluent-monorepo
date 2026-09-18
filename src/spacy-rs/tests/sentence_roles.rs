use super::*;
use std::sync::Arc;

use crate::frame::extract_frames;
use crate::interlingua::InterlinguaResolver;
use crate::llm::{attach, AnnotationSet};
use crate::routing::extract_routing_signals;
use crate::sentencizer::Sentencizer;
use crate::triple::extract_triples;
use crate::vocab::Vocab;
use fluent_concept::InMemoryConceptStore;

fn vocab() -> Arc<Vocab> {
    Arc::new(Vocab::new(crate::lexeme::LexiconConfig::default()))
}

/// Attach a full UD parse and re-run the sentencizer (the state
/// `process_sync` produces).
fn attached(json: &str, tokens: &[&str]) -> Doc {
    let v = vocab();
    let mut doc = Doc::new(Arc::clone(&v));
    for t in tokens {
        doc.push_back(t, true).expect("push");
    }
    let set = AnnotationSet::parse_json(json).expect("parse json");
    attach(&mut doc, &set).expect("attach");
    Sentencizer::new().process(&mut doc);
    doc
}

/// Star-parse doc: deterministic rule rung output (flat `dep` children).
fn star_doc(tokens: &[&str]) -> Doc {
    let v = vocab();
    let mut doc = Doc::new(Arc::clone(&v));
    for t in tokens {
        doc.push_back(t, true).expect("push");
    }
    let rule = crate::pipeline::RuleAnnotator::en_default();
    let set = rule.annotate(&doc);
    attach(&mut doc, &set).expect("attach");
    Sentencizer::new().process(&mut doc);
    doc
}

fn resolver_for(doc: &Doc) -> (Arc<dyn fluent_concept::ConceptStore>, InterlinguaResolver) {
    let store: Arc<dyn fluent_concept::ConceptStore> = Arc::new(InMemoryConceptStore::new());
    let resolver = InterlinguaResolver::new(
        Arc::clone(&store) as Arc<dyn fluent_concept::ConceptStore>,
        Arc::clone(doc.vocab().strings()),
    );
    (store, resolver)
}

const TRANSITIVE_PARSE: &str = r#"[
    {"text":"Show","pos":"verb","dep":"root","head":0,"lemma":"show"},
    {"text":"me","pos":"pron","dep":"iobj","head":-1,"lemma":"me"},
    {"text":"the","pos":"det","dep":"det","head":1,"lemma":"the"},
    {"text":"report","pos":"noun","dep":"dobj","head":-3,"lemma":"report"}
]"#;

const INTRANSITIVE_PARSE: &str = r#"[
    {"text":"The","pos":"det","dep":"det","head":1,"lemma":"the"},
    {"text":"dog","pos":"noun","dep":"nsubj","head":1,"lemma":"dog"},
    {"text":"barks","pos":"verb","dep":"root","head":0,"lemma":"bark"},
    {"text":".","pos":"punct","dep":"punct","head":-1,"lemma":"."}
]"#;

const MULTI_PARSE: &str = r#"[
    {"text":"The","pos":"det","dep":"det","head":1,"lemma":"the"},
    {"text":"cat","pos":"noun","dep":"nsubj","head":1,"lemma":"cat"},
    {"text":"sat","pos":"verb","dep":"root","head":0,"lemma":"sat"},
    {"text":".","pos":"punct","dep":"punct","head":-1,"lemma":"."},
    {"text":"The","pos":"det","dep":"det","head":1,"lemma":"the"},
    {"text":"dog","pos":"noun","dep":"nsubj","head":1,"lemma":"dog"},
    {"text":"ran","pos":"verb","dep":"root","head":0,"lemma":"ran"},
    {"text":".","pos":"punct","dep":"punct","head":-1,"lemma":"."}
]"#;

const VERBLESS_PARSE: &str = r#"[
    {"text":"Hello","pos":"intj","dep":"discourse","head":1,"lemma":"hello"},
    {"text":"world","pos":"noun","dep":"vocative","head":-1,"lemma":"world"}
]"#;

#[test]
fn empty_doc_yields_no_sentence_roles() {
    assert!(sentence_roles(&Doc::new(vocab())).is_empty());
}

#[test]
fn transitive_slots_match_routing_and_triple() {
    let doc = attached(TRANSITIVE_PARSE, &["Show", "me", "the", "report"]);
    let roles = sentence_roles(&doc);
    assert_eq!(roles.len(), 1);
    let r = &roles[0];
    assert_eq!(r.span, (0, 4));
    assert_eq!(r.root, 0);
    assert!(r.subjects.is_empty());
    assert_eq!(r.direct_objects, vec![3]);
    assert_eq!(r.indirect_objects, vec![1]);

    // Routing agreement: the same slots surface as routing lemmas.
    let signals = extract_routing_signals(&doc);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].direct_object.as_deref(), Some("report"));
    assert_eq!(signals[0].indirect_object.as_deref(), Some("me"));
    assert!(signals[0].subject.is_none());

    // Triple agreement: children-restricted first-match coincides here.
    let triples = extract_triples(&doc);
    assert_eq!(triples.len(), 1);
    assert_eq!(triples[0].predicate, r.root);
    // Triple's children-restricted first-match coincides with the walk's
    // span-ordered candidates on well-formed parses.
    let kids = doc.children(r.root);
    assert_eq!(
        triples[0].subject,
        r.subjects.iter().copied().find(|c| kids.contains(c))
    );
    assert_eq!(
        triples[0].object,
        r.direct_objects.iter().copied().find(|c| kids.contains(c))
    );
}

#[test]
fn intransitive_subject_slot_and_frame_roles() {
    let doc = attached(INTRANSITIVE_PARSE, &["The", "dog", "barks", "."]);
    let roles = sentence_roles(&doc);
    assert_eq!(roles.len(), 1);
    let r = &roles[0];
    assert_eq!(r.span, (0, 4));
    assert_eq!(r.root, 2);
    assert_eq!(r.subjects, vec![1]);
    assert!(r.direct_objects.is_empty());
    assert!(r.indirect_objects.is_empty());

    let (store, resolver) = resolver_for(&doc);
    let analysis = extract_frames(&doc, &resolver, store.as_ref(), None);
    assert_eq!(analysis.frames.len(), 1);
    assert_eq!(analysis.frames[0].sentence_span, r.span);
    let kinds: Vec<crate::frame::RoleType> = analysis.frames[0]
        .roles
        .iter()
        .map(|(k, _, _)| *k)
        .collect();
    assert!(kinds.contains(&crate::frame::RoleType::Subject));
}

#[test]
fn multi_sentence_yields_one_roles_entry_per_sentence() {
    let doc = attached(
        MULTI_PARSE,
        &["The", "cat", "sat", ".", "The", "dog", "ran", "."],
    );
    let roles = sentence_roles(&doc);
    assert_eq!(roles.len(), 2);
    assert_eq!(roles[0].span, (0, 4));
    assert_eq!(roles[0].root, 2);
    assert_eq!(roles[0].subjects, vec![1]);
    assert_eq!(roles[1].span, (4, 8));
    assert_eq!(roles[1].root, 6);
    assert_eq!(roles[1].subjects, vec![5]);

    assert_eq!(extract_routing_signals(&doc).len(), 2);
    assert_eq!(extract_triples(&doc).len(), 2);
    let (store, resolver) = resolver_for(&doc);
    assert_eq!(
        extract_frames(&doc, &resolver, store.as_ref(), None)
            .frames
            .len(),
        2
    );
}

// ── Control group: shapes that must NOT change ──

#[test]
fn star_parse_keeps_empty_slots_with_populated_dependents() {
    let doc = star_doc(&["Show", "me", "the", "report"]);
    let roles = sentence_roles(&doc);
    assert_eq!(roles.len(), 1);
    assert!(roles[0].subjects.is_empty());
    assert!(roles[0].direct_objects.is_empty());
    assert!(roles[0].indirect_objects.is_empty());

    // The routing signal degrades gracefully: role slots empty, residual
    // dependents populated.
    let signals = extract_routing_signals(&doc);
    assert_eq!(signals.len(), 1);
    assert!(signals[0].subject.is_none());
    assert!(signals[0].direct_object.is_none());
    assert!(!signals[0].dependents.is_empty());
}

#[test]
fn verbless_sentence_falls_back_to_span_start() {
    let doc = attached(VERBLESS_PARSE, &["Hello", "world"]);
    let roles = sentence_roles(&doc);
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].root, roles[0].span.0);
}

#[test]
fn selection_policy_stays_per_caller_on_multiple_subjects() {
    // Two `nsubj` children of one root: routing keeps the LAST candidate
    // (its historical overwrite chain), triple keeps the FIRST child.
    // The shared walk reports both candidates in span order; selection is
    // the callers' contract, pinned here so neither drifts.
    let json = r#"[
        {"text":"John","pos":"propn","dep":"nsubj","head":2,"lemma":"john"},
        {"text":"Mary","pos":"propn","dep":"nsubj","head":1,"lemma":"mary"},
        {"text":"run","pos":"verb","dep":"root","head":0,"lemma":"run"},
        {"text":".","pos":"punct","dep":"punct","head":-1,"lemma":"."}
    ]"#;
    let doc = attached(json, &["John", "Mary", "run", "."]);
    let roles = sentence_roles(&doc);
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].subjects, vec![0, 1]);

    let signals = extract_routing_signals(&doc);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].subject.as_deref(), Some("mary"));

    let triples = extract_triples(&doc);
    assert_eq!(triples.len(), 1);
    assert_eq!(triples[0].subject, Some(0));
}
