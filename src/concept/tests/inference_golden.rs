use super::*;
use guidance_rdf::parser::{Term, Triple};

fn build_triple(s: &str, p: &str, o: &str) -> Triple {
    Triple {
        subject: Term::Iri(s.to_string()),
        predicate: Term::Iri(p.to_string()),
        object: Term::Iri(o.to_string()),
    }
}

const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

#[test]
fn subclass_transitivity_derives_ac() {
    let ab = build_triple("Scientist", RDFS_SUBCLASS_OF, "Person");
    let bc = build_triple("Person", RDFS_SUBCLASS_OF, "Agent");
    let mut engine = InferenceEngine::new();
    engine.add_rule(InferenceRule {
        rule_type: RuleType::SubclassTransitivity,
        trigger_predicate: RDFS_SUBCLASS_OF.to_string(),
    });
    let derived = engine.infer(&[ab, bc]).unwrap();
    assert!(derived
        .iter()
        .any(|t| triple_subject_iri(t).as_deref() == Some("Scientist")
            && triple_object_iri(t).as_deref() == Some("Agent")));
}

#[test]
fn unsupported_rule_fails_closed_and_unruled_derives_nothing() {
    let mut engine = InferenceEngine::new();
    engine.add_rule(InferenceRule {
        rule_type: RuleType::InverseOf,
        trigger_predicate: RDFS_SUBCLASS_OF.to_string(),
    });
    let ab = build_triple("A", RDFS_SUBCLASS_OF, "B");
    assert!(matches!(
        engine.infer(&[ab]),
        Err(InferenceError::UnsupportedRule(RuleType::InverseOf))
    ));
    let bare = InferenceEngine::new();
    let ab = build_triple("A", RDFS_SUBCLASS_OF, "B");
    let bc = build_triple("B", RDFS_SUBCLASS_OF, "C");
    assert!(bare.infer(&[ab, bc]).unwrap().is_empty());
}
