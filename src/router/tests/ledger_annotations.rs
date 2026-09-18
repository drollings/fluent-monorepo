//! Router-side annotation linkage tests (M10.4): the store-level tests moved
//! with `AnnotationStore` to `fluent-concept` (`tests/annotation_store.rs`) —
//! single home. What stays here exercises the `ContentNodeStore` writer
//! wiring seam (`write_annotation` + session nodes) against the durable
//! store, through the re-exported `AnnotationStore`.

use super::*;
use fluent_concept::annotation_store::AnnotationStore;
use fluent_types::{AnnotationClaim, ClaimStatus, ProvenanceTier};

fn claim(key: &str, tier: ProvenanceTier, status: ClaimStatus) -> AnnotationClaim {
    AnnotationClaim {
        claim_key: key.to_string(),
        tier,
        status,
        payload: serde_json::json!({ "key": key }),
        produced_by: "test".to_string(),
        produced_at: 1,
    }
}

/// Acceptance criterion #3, through the real node store: mutating a node's
/// content changes its `content_hash` and invalidates prior annotations —
/// without manual scheduling. Also exercises the `ContentNodeStore` writer
/// wiring seam (`write_annotation`).
#[test]
fn node_store_hash_change_invalidates_annotations() {
    use crate::node_store::ContentNodeStore;

    let store = ContentNodeStore::open_in_memory().unwrap();
    let id = store
        .record_request("sess", "req", "show me the report")
        .unwrap();
    let hash_before = store.snapshot(id).unwrap().content_hash;
    assert!(!hash_before.is_unstamped(), "LOD0 present → stamped hash");

    // A LocalModel annotation written under the current hash.
    store
        .write_annotation(
            id,
            &claim(
                "frame:report",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap()
        .expect("durable in-memory store");

    let annotations = AnnotationStore::new(store.shared_sqlite().expect("shared store"));
    assert!(annotations
        .read(hash_before, "frame:report")
        .unwrap()
        .is_some());

    // Appending the turn's answer leaves the request node's LOD0 (and its
    // hash) untouched: the claim stays reachable under the same key. The
    // answer rides its own node with its own stamped hash.
    store
        .record_result(id, true, Some(0.9), "display the sales report")
        .unwrap();
    let hash_after = store.snapshot(id).unwrap().content_hash;
    assert_eq!(hash_before, hash_after, "append never mutates the request");
    assert!(
        annotations
            .read(hash_after, "frame:report")
            .unwrap()
            .is_some(),
        "claim under the stable hash stays reachable"
    );
    // Re-derivation under the stable hash supersedes in place (audit rows
    // retained), and the appended answer node carries its own stamped hash.
    store
        .write_annotation(
            id,
            &claim(
                "frame:report",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap()
        .expect("re-derive");
    assert!(annotations
        .read(hash_after, "frame:report")
        .unwrap()
        .is_some());
    let nodes = store.get_session_nodes("sess", 10).unwrap();
    let answer = nodes
        .iter()
        .find(|n| n.role == Some(fluent_types::OriginRole::Assistant))
        .expect("appended answer");
    assert!(
        !answer.content_hash.is_unstamped(),
        "answer node has its own stamped hash"
    );
    assert_ne!(
        answer.content_hash, hash_before,
        "answer hash differs from the request hash"
    );
}
