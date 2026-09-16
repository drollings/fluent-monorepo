use std::sync::Arc;

use crate::doc::Doc;
use crate::entity_gate::{
    stamp_gated, verify_stamps, EntityStampGate, KeepReason, StampCandidate, StampOutcome,
};
use crate::labels::Upos;
use crate::lexeme::LexiconConfig;
use crate::llm::{attach, AnnotationSet};
use crate::routing::extract_routing_signals;
use crate::sentencizer::Sentencizer;
use crate::vocab::Vocab;
use fluent_types::{InterlinguaId, InterlinguaNamespace};

fn vocab() -> Arc<Vocab> {
    Arc::new(Vocab::new(LexiconConfig::default()))
}

fn paris_doc() -> Doc {
    // "Show me Paris today": Paris (idx 2) is a PROPN dobj; "today" a noun.
    let tokens = ["Show", "me", "Paris", "today"];
    let json = r#"[
        {"text":"Show","pos":"verb","dep":"root","head":0,"lemma":"show"},
        {"text":"me","pos":"pron","dep":"iobj","head":-1,"lemma":"me"},
        {"text":"Paris","pos":"propn","dep":"dobj","head":-2,"lemma":"paris"},
        {"text":"today","pos":"noun","dep":"npadvmod","head":-3,"lemma":"today"}
    ]"#;
    let mut doc = Doc::new(vocab());
    for t in tokens {
        doc.push_back(t, true).expect("push");
    }
    let set = AnnotationSet::parse_json(json).expect("parse json");
    attach(&mut doc, &set).expect("attach");
    Sentencizer::new().process(&mut doc);
    doc
}

fn ent(local: i64) -> InterlinguaId {
    InterlinguaId::new(InterlinguaNamespace::YagoEntity, local)
}

fn gate() -> EntityStampGate {
    EntityStampGate::new(0.7)
}

fn concept_set(doc: &Doc) -> Vec<InterlinguaId> {
    extract_routing_signals(doc)
        .into_iter()
        .filter_map(|s| s.interlingua)
        .flat_map(|il| il.concept_ids)
        .collect()
}

#[test]
fn parse_leaves_entity_ids_unstamped_by_default() {
    // Candidates-only default: no pipeline path stamps at parse time.
    let doc = paris_doc();
    assert!(doc
        .tokens()
        .iter()
        .all(|t| t.interlingua_entity_id.is_none()));
}

#[test]
fn high_score_distinct_entity_stamps_and_moves_the_signal() {
    let mut doc = paris_doc();
    assert!(concept_set(&doc).is_empty());
    let out = stamp_gated(
        &mut doc,
        &gate(),
        &[StampCandidate {
            token: 2,
            entity: ent(0x9a1),
            score: 0.95,
        }],
    );
    assert!(matches!(out[..], [StampOutcome::Stamped(_)]));
    assert_eq!(doc.token(2).interlingua_entity_id, Some(ent(0x9a1)));
    // Route-impact pairing: the sentence signal gains exactly this entity.
    assert_eq!(concept_set(&doc), vec![ent(0x9a1)]);
}

/// One calibration row: (name, setup, score, entity-local, may_stamp).
type CalibrationCase<'a> = (&'a str, Box<dyn Fn(&mut Doc) + 'a>, f64, i64, bool);

#[test]
fn calibration_table_controls_never_stamp() {
    // Precision/recall calibration: labeled cases with expected decisions.
    // The control group (ambiguous, duplicate, non-PROPN, pre-stamped,
    // boundary) must NOT stamp; only the clear high-score distinct case may.
    let cases: Vec<CalibrationCase<'_>> = vec![
        // (name, setup, score, entity-local, may_stamp)
        (
            "low score ambiguous",
            Box::new(|_: &mut Doc| {}),
            0.4,
            0x1,
            false,
        ),
        (
            "nan score",
            Box::new(|_: &mut Doc| {}),
            f64::NAN,
            0x1,
            false,
        ),
        (
            "duplicate entity already routed",
            Box::new(|doc: &mut Doc| {
                doc.token_mut(2).interlingua_entity_id = Some(ent(0x9));
            }),
            0.95,
            0x9,
            false,
        ),
        (
            "non-propn token",
            Box::new(|doc: &mut Doc| {
                doc.token_mut(3).pos = Upos::Noun;
            }),
            0.95,
            0x2,
            false,
        ),
        (
            "already stamped slot",
            Box::new(|doc: &mut Doc| {
                doc.token_mut(2).interlingua_entity_id = Some(ent(0x7));
            }),
            0.99,
            0x8,
            false,
        ),
        (
            "boundary score stamps",
            Box::new(|_: &mut Doc| {}),
            0.7,
            0x3,
            true,
        ),
        (
            "clear distinct stamps",
            Box::new(|_: &mut Doc| {}),
            0.95,
            0x4,
            true,
        ),
    ];
    for (name, setup, score, local, may_stamp) in cases {
        let mut doc = paris_doc();
        setup(&mut doc);
        // Non-propn case targets token 3; the rest target the Paris token.
        let token = if name == "non-propn token" { 3 } else { 2 };
        let out = stamp_gated(
            &mut doc,
            &gate(),
            &[StampCandidate {
                token,
                entity: ent(local),
                score,
            }],
        );
        let stamped = matches!(out[..], [StampOutcome::Stamped(_)]);
        assert_eq!(stamped, may_stamp, "case {name}");
        if !may_stamp {
            assert!(
                concept_set(&doc)
                    .iter()
                    .all(|e| *e != ent(local) || name == "duplicate entity already routed"),
                "control {name} must not route the candidate entity"
            );
        }
    }
}

#[test]
fn duplicate_mention_of_routed_entity_stays_candidate() {
    // Confirmation without new information: the entity is already routed
    // from another span, so stamping adds no distinct signal — candidate.
    let mut doc = paris_doc();
    doc.token_mut(2).interlingua_entity_id = Some(ent(0x5));
    let out = stamp_gated(
        &mut doc,
        &gate(),
        &[StampCandidate {
            token: 2,
            entity: ent(0x5),
            score: 0.99,
        }],
    );
    assert!(matches!(
        out[..],
        [StampOutcome::Kept {
            token: 2,
            reason: KeepReason::AlreadyStamped,
        }]
    ));
}

#[test]
fn content_mutation_invalidates_stamp_by_keying() {
    let mut doc = paris_doc();
    let out = stamp_gated(
        &mut doc,
        &gate(),
        &[StampCandidate {
            token: 2,
            entity: ent(0x6),
            score: 0.9,
        }],
    );
    let stamped = match &out[..] {
        [StampOutcome::Stamped(s)] => *s,
        _ => panic!("must stamp"),
    };
    // Untouched text verifies clean.
    assert!(verify_stamps(&mut doc, &[stamped]).is_empty());
    assert_eq!(doc.token(2).interlingua_entity_id, Some(ent(0x6)));
    // Mutating the token text invalidates: cleared and reported.
    doc.token_mut(2).lexeme = doc.token(0).lexeme.clone();
    let cleared = verify_stamps(&mut doc, &[stamped]);
    assert_eq!(cleared, vec![2]);
    assert!(doc.token(2).interlingua_entity_id.is_none());
}
