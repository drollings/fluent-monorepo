//! End-to-end control-group tests for parse-time entity promotion: the
//! overlay acceptance seam (`EntityPromotion`) over the real candidate plane.
//!
//! - Flag off → nothing is written (candidates-only default preserved).
//! - Flag on + gate passing → the id is stamped, the candidate promotes,
//!   and the content-keyed stamp persists.
//! - Control group (low-score, already-stamped, non-YaGO) → stays Pending.
//! - Content mutation → `verify` clears the stale id and its row.

use crate::ledger::entity_promotion::EntityPromotion;
use crate::ledger::overlay::{CandidateStatus, OverlayCandidate, OverlayCandidateStore};
use crate::ledger::ContentNodeLedger;
use fluent_types::{InterlinguaId, InterlinguaNamespace, NodeId};
use spacy_rs::entity_gate::EntityStampGate;
use std::sync::Arc;

fn store(ledger: &ContentNodeLedger) -> OverlayCandidateStore {
    OverlayCandidateStore::new(ledger.node_store().shared_sqlite().expect("shared sqlite"))
}

fn paris_doc() -> spacy_rs::doc::Doc {
    // "Show me Paris today": Paris (idx 2) is a PROPN dobj; "today" a noun.
    let vocab = Arc::new(spacy_rs::vocab::Vocab::new(
        spacy_rs::lexeme::LexiconConfig::default(),
    ));
    let json = r#"[
        {"text":"Show","pos":"verb","dep":"root","head":0,"lemma":"show"},
        {"text":"me","pos":"pron","dep":"iobj","head":-1,"lemma":"me"},
        {"text":"Paris","pos":"propn","dep":"dobj","head":-2,"lemma":"paris"},
        {"text":"today","pos":"noun","dep":"npadvmod","head":-3,"lemma":"today"}
    ]"#;
    let mut doc = spacy_rs::doc::Doc::new(vocab);
    for t in ["Show", "me", "Paris", "today"] {
        doc.push_back(t, true).expect("push");
    }
    let set = spacy_rs::llm::AnnotationSet::parse_json(json).expect("parse json");
    spacy_rs::llm::attach(&mut doc, &set).expect("attach");
    spacy_rs::sentencizer::Sentencizer::new().process(&mut doc);
    doc
}

fn yago(local: i64) -> InterlinguaId {
    InterlinguaId::new(InterlinguaNamespace::YagoEntity, local)
}

fn gate() -> EntityStampGate {
    EntityStampGate::new(spacy_rs::entity_gate::CALIBRATED_SCORE_THRESHOLD)
}

const TEXT: &str = "Show me Paris today";

fn paris_candidate(node: NodeId, entity: InterlinguaId, score: f64) -> OverlayCandidate {
    // "Paris" sits at bytes 8..13 of TEXT.
    OverlayCandidate::entity_link(node, 8, 13, entity, score, "entity_link")
}

fn pending(store: &OverlayCandidateStore, node: NodeId) -> Vec<OverlayCandidate> {
    store
        .for_node(node)
        .expect("query")
        .into_iter()
        .filter(|c| c.status == CandidateStatus::Pending)
        .collect()
}

#[test]
fn flag_off_writes_nothing() {
    let ledger = ContentNodeLedger::open_in_memory().expect("ledger");
    let s = store(&ledger);
    let node = NodeId::from_int(1);
    s.write_candidate(&paris_candidate(node, yago(0x9a1), 0.95))
        .expect("write");
    let promotion = EntityPromotion::disabled();
    let mut doc = paris_doc();
    let stamped = promotion
        .promote(&mut doc, TEXT, node, &s)
        .expect("promote");
    assert!(stamped.is_empty(), "disabled promotion stamps nothing");
    assert!(
        doc.token(2).interlingua_entity_id.is_none(),
        "candidates-only default preserved"
    );
    assert_eq!(pending(&s, node).len(), 1, "candidate stays Pending");
    assert!(
        s.stamps_for_node(node).expect("query").is_empty(),
        "no stamp rows when disabled"
    );
}

#[test]
fn flag_on_promotes_passing_candidate() {
    let ledger = ContentNodeLedger::open_in_memory().expect("ledger");
    let s = store(&ledger);
    let node = NodeId::from_int(2);
    s.write_candidate(&paris_candidate(node, yago(0x9a1), 0.95))
        .expect("write");
    let promotion = EntityPromotion::new(gate());
    let mut doc = paris_doc();
    let stamped = promotion
        .promote(&mut doc, TEXT, node, &s)
        .expect("promote");
    assert_eq!(stamped.len(), 1, "one passing candidate stamps");
    assert_eq!(doc.token(2).interlingua_entity_id, Some(yago(0x9a1)));
    assert_eq!(
        stamped[0].text_hash,
        common_core::hash::fnv1a64("Paris".as_bytes()),
        "stamp is content-keyed"
    );
    // The accepted candidate promotes in the plane; the stamp persists.
    let rows = s.for_node(node).expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, CandidateStatus::Promoted);
    let stamps = s.stamps_for_node(node).expect("query");
    assert_eq!(stamps.len(), 1);
    assert_eq!(stamps[0].entity_id, yago(0x9a1));
}

#[test]
fn control_group_stays_candidate() {
    let ledger = ContentNodeLedger::open_in_memory().expect("ledger");
    let s = store(&ledger);
    let node = NodeId::from_int(3);
    // Low-score ([A] fails), pre-stamped slot (first-wins), non-YaGO ([B]
    // fails) — all high-signal except for their one failing leg.
    s.write_candidate(&paris_candidate(node, yago(0x1), 0.40))
        .expect("write");
    s.write_candidate(&paris_candidate(node, yago(0x2), 0.95))
        .expect("write");
    s.write_candidate(&OverlayCandidate::entity_link(
        node,
        8,
        13,
        InterlinguaId::new(InterlinguaNamespace::SpacyLemma, 0x1),
        0.95,
        "entity_link",
    ))
    .expect("write");
    let promotion = EntityPromotion::new(gate());
    let mut doc = paris_doc();
    // Pre-stamp the slot so the 0.95 YaGO candidate hits AlreadyStamped.
    doc.token_mut(2).interlingua_entity_id = Some(yago(0x7));
    let stamped = promotion
        .promote(&mut doc, TEXT, node, &s)
        .expect("promote");
    assert!(stamped.is_empty(), "control group stamps nothing");
    assert_eq!(
        doc.token(2).interlingua_entity_id,
        Some(yago(0x7)),
        "pre-stamped slot untouched"
    );
    assert_eq!(
        pending(&s, node).len(),
        3,
        "all controls stay Pending, got {:?}",
        s.for_node(node).expect("query")
    );
    assert!(
        s.stamps_for_node(node).expect("query").is_empty(),
        "no stamp rows for controls"
    );
}

#[test]
fn content_mutation_clears_stamp() {
    let ledger = ContentNodeLedger::open_in_memory().expect("ledger");
    let s = store(&ledger);
    let node = NodeId::from_int(4);
    s.write_candidate(&paris_candidate(node, yago(0x6), 0.9))
        .expect("write");
    let promotion = EntityPromotion::new(gate());
    let mut doc = paris_doc();
    let stamped = promotion
        .promote(&mut doc, TEXT, node, &s)
        .expect("promote");
    assert_eq!(stamped.len(), 1);
    // Untouched text verifies clean.
    assert!(promotion
        .verify(&mut doc, node, &s)
        .expect("verify")
        .is_empty());
    // Mutating the token text invalidates: cleared, reported, row deleted.
    doc.token_mut(2).lexeme = doc.token(0).lexeme.clone();
    let cleared = promotion.verify(&mut doc, node, &s).expect("verify");
    assert_eq!(cleared, vec![2]);
    assert!(doc.token(2).interlingua_entity_id.is_none());
    assert!(
        s.stamps_for_node(node).expect("query").is_empty(),
        "cleared stamp row deleted"
    );
}
