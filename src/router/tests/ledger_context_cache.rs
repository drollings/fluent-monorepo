use super::*;
use std::sync::Arc;

use crate::node_store::ContentNodeStore;

fn mem_store() -> Arc<ContentNodeStore> {
    Arc::new(ContentNodeStore::open_in_memory().expect("in-memory ledger opens"))
}

fn ask_accepted(store: &Arc<ContentNodeStore>, session: &str, q: &str, a: &str) {
    let node = store
        .record_request(session, "r1", q)
        .expect("record request");
    store
        .record_result(node, true, Some(1.0), a)
        .expect("record result");
}

#[test]
fn accepted_answer_lookup_hits_exact() {
    let store = mem_store();
    ask_accepted(&store, "s", "what is the dragon's mood", "calm and sleepy");
    let (answer, _) = store
        .accepted_answer_for("what is the dragon's mood")
        .expect("exact question hits");
    assert_eq!(answer, "calm and sleepy");
}

#[test]
fn lookup_normalizes_case_and_whitespace() {
    let store = mem_store();
    ask_accepted(&store, "s", "What  IS   the time?", "noon");
    assert!(
        store.accepted_answer_for("what is the time?").is_some(),
        "case/whitespace variants hit the same key"
    );
}

#[test]
fn lookup_keys_on_pii_shape_not_values() {
    // The M6 anonymizer maps same-shape PII to the same placeholders, so a
    // re-asked question with different values still hits — the frontier
    // never pays twice for the same question shape.
    let store = mem_store();
    ask_accepted(
        &store,
        "s",
        "my email is alice@example.com, when is the meeting",
        "at noon",
    );
    assert!(
        store
            .accepted_answer_for("my email is bob@example.org, when is the meeting")
            .is_some(),
        "same PII shape hits"
    );
}

#[test]
fn rejected_answers_never_cache() {
    let store = mem_store();
    let node = store
        .record_request("s", "r1", "will this fail")
        .expect("record request");
    store
        .record_result(node, false, None, "ERROR: boom")
        .expect("record rejection");
    assert!(
        store.accepted_answer_for("will this fail").is_none(),
        "rejected turns must never become cached facts"
    );
}

#[test]
fn unasked_questions_miss() {
    let store = mem_store();
    ask_accepted(&store, "s", "asked", "answered");
    assert!(
        store.accepted_answer_for("never asked").is_none(),
        "unknown questions miss"
    );
    assert!(
        store.accepted_answer_for("   ").is_none(),
        "blank questions miss"
    );
}

#[test]
fn cache_trait_serves_hits_with_provenance() {
    let store = mem_store();
    ask_accepted(&store, "s", "what is two plus two", "four");
    let cache = LedgerContextCache::new(Arc::clone(&store));
    let hit = cache
        .lookup("what is two plus two")
        .expect("trait lookup hits");
    assert_eq!(hit.source, "ledger-qa");
    assert_eq!(hit.content, "four");
    assert!(
        hit.metadata
            .as_ref()
            .and_then(|m| m.get("node_id"))
            .is_some(),
        "hit carries the answer node id, got: {:?}",
        hit.metadata
    );
    assert!(
        cache.lookup("something else entirely").is_none(),
        "trait lookup misses cleanly"
    );
}
