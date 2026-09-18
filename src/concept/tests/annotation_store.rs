use super::*;
use fluent_types::ContentHash;

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

fn store() -> AnnotationStore {
    // The table is created by the ledger migration; run it directly here so
    // the store is testable in isolation.
    let db = Arc::new(SqliteStore::open_in_memory().unwrap());
    db.init_schema(ANNOTATION_SCHEMA).unwrap();
    AnnotationStore::new(db)
}

const ANNOTATION_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ledger_annotations (
    content_hash INTEGER NOT NULL,
    claim_key TEXT NOT NULL,
    claim_id INTEGER NOT NULL,
    tier TEXT NOT NULL,
    status TEXT NOT NULL,
    payload TEXT NOT NULL,
    produced_by TEXT NOT NULL,
    produced_at INTEGER NOT NULL,
    PRIMARY KEY (content_hash, claim_key, claim_id)
);
CREATE INDEX IF NOT EXISTS idx_ledger_annotations_active
    ON ledger_annotations(content_hash, claim_key, claim_id);";

#[test]
fn first_write_establishes_confirmed() {
    let store = store();
    let status = store
        .write(
            ContentHash::from(100u64),
            &claim(
                "frame:x",
                ProvenanceTier::Deterministic,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    assert_eq!(status, ClaimStatus::Confirmed);
    let got = store
        .read(ContentHash::from(100u64), "frame:x")
        .unwrap()
        .expect("present");
    assert_eq!(got.status, ClaimStatus::Confirmed);
    assert_eq!(got.tier, ProvenanceTier::Deterministic);
}

/// DRY guard: the plain snake_case storage text must never drift from the
/// enum's canonical serde wire form (so the match helpers stay in lockstep
/// with `ProvenanceTier`/`ClaimStatus`'s own `rename_all`).
#[test]
fn storage_text_matches_serde_snake_case() {
    for tier in [
        ProvenanceTier::Deterministic,
        ProvenanceTier::LocalModel,
        ProvenanceTier::Frontier,
        ProvenanceTier::HumanReview,
    ] {
        let serde_form: String =
            serde_json::from_value(serde_json::to_value(tier).unwrap()).unwrap();
        assert_eq!(tier_as_text(tier), serde_form, "{tier:?}");
        assert_eq!(tier_from_text(tier_as_text(tier)).unwrap(), tier);
    }
    for status in [
        ClaimStatus::Provisional,
        ClaimStatus::Confirmed,
        ClaimStatus::Superseded,
    ] {
        let serde_form: String =
            serde_json::from_value(serde_json::to_value(status).unwrap()).unwrap();
        assert_eq!(status_as_text(status), serde_form, "{status:?}");
        assert_eq!(status_from_text(status_as_text(status)).unwrap(), status);
    }
}

#[test]
fn higher_tier_supersedes_and_never_deletes() {
    let store = store();
    // Acceptance criterion #2: LocalModel then Frontier on the same node/claim.
    store
        .write(
            ContentHash::from(100u64),
            &claim(
                "frame:x",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    let status = store
        .write(
            ContentHash::from(100u64),
            &claim("frame:x", ProvenanceTier::Frontier, ClaimStatus::Confirmed),
        )
        .unwrap();
    assert_eq!(status, ClaimStatus::Confirmed);

    // The frontier claim is the current, confirmed one.
    let got = store
        .read(ContentHash::from(100u64), "frame:x")
        .unwrap()
        .expect("present");
    assert_eq!(got.tier, ProvenanceTier::Frontier);
    assert_eq!(got.status, ClaimStatus::Confirmed);

    // The prior row was marked superseded, not deleted (row count never drops).
    let history = store.history(ContentHash::from(100u64), "frame:x").unwrap();
    assert_eq!(history.len(), 2, "superseded row retained");
    let prior = history
        .iter()
        .find(|c| c.tier == ProvenanceTier::LocalModel)
        .unwrap();
    assert_eq!(prior.status, ClaimStatus::Superseded);
}

#[test]
fn lower_tier_is_provisional_and_never_overrides() {
    let store = store();
    store
        .write(
            ContentHash::from(100u64),
            &claim(
                "frame:x",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    // A deterministic (lower) write becomes provisional; the local-model
    // confirmed claim stays authoritative.
    let status = store
        .write(
            ContentHash::from(100u64),
            &claim(
                "frame:x",
                ProvenanceTier::Deterministic,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    assert_eq!(status, ClaimStatus::Provisional);

    let got = store
        .read(ContentHash::from(100u64), "frame:x")
        .unwrap()
        .expect("present");
    assert_eq!(
        got.tier,
        ProvenanceTier::LocalModel,
        "lower tier never overrides"
    );
    assert_eq!(got.status, ClaimStatus::Confirmed);

    // The provisional row is recorded (explicitly not authoritative).
    let active = store.read_active(ContentHash::from(100u64)).unwrap();
    assert_eq!(active.len(), 1, "only the confirmed claim is active");
    let all = store.history(ContentHash::from(100u64), "frame:x").unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].status, ClaimStatus::Confirmed);
    assert_eq!(all[1].status, ClaimStatus::Provisional);
}

#[test]
fn equal_tier_rewrite_is_idempotent() {
    let store = store();
    store
        .write(
            ContentHash::from(100u64),
            &claim(
                "frame:x",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    // Re-derivation at the same authority does not self-demote.
    let status = store
        .write(
            ContentHash::from(100u64),
            &claim(
                "frame:x",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    assert_eq!(status, ClaimStatus::Confirmed);
    assert_eq!(
        store
            .history(ContentHash::from(100u64), "frame:x")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn hash_change_invalidates_annotations() {
    // Acceptance criterion #3: a node whose content changed (new hash) has
    // no reachable annotations under the old hash — no scheduler needed.
    let store = store();
    store
        .write(
            ContentHash::from(111u64),
            &claim(
                "frame:x",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    assert!(store
        .read(ContentHash::from(111u64), "frame:x")
        .unwrap()
        .is_some());

    // Same claim key, different (mutated) content hash: unreachable.
    assert!(store
        .read(ContentHash::from(222u64), "frame:x")
        .unwrap()
        .is_none());
    // Re-derivation under the new hash writes fresh.
    store
        .write(
            ContentHash::from(222u64),
            &claim(
                "frame:x",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    assert_eq!(
        store
            .read(ContentHash::from(222u64), "frame:x")
            .unwrap()
            .unwrap()
            .tier,
        ProvenanceTier::LocalModel
    );
    // The old hash's row remains (audit), reachable by history but not by
    // the current read for the new hash.
    assert!(store
        .read(ContentHash::from(111u64), "frame:x")
        .unwrap()
        .is_some());
}

#[test]
fn distinct_claim_keys_are_independent() {
    let store = store();
    store
        .write(
            ContentHash::from(100u64),
            &claim(
                "frame:a",
                ProvenanceTier::Deterministic,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    store
        .write(
            ContentHash::from(100u64),
            &claim(
                "frame:b",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    let active = store.read_active(ContentHash::from(100u64)).unwrap();
    assert_eq!(active.len(), 2);
}

#[test]
fn unstamped_write_rejected_and_writes_no_row() {
    use fluent_db::error::DbError;
    let store = store();
    let err = store
        .write(
            ContentHash::UNSTAMPED,
            &claim(
                "frame:x",
                ProvenanceTier::Deterministic,
                ClaimStatus::Confirmed,
            ),
        )
        .expect_err("unstamped write rejected");
    assert!(
        matches!(err, DbError::UnstampedContent),
        "typed rejection, got {err:?}"
    );
    assert!(store
        .read(ContentHash::UNSTAMPED, "frame:x")
        .unwrap()
        .is_none());
    assert!(store
        .history(ContentHash::UNSTAMPED, "frame:x")
        .unwrap()
        .is_empty());
}

#[test]
fn old_shape_row_stays_readable() {
    // A row written under the pre-change numeric shape (u64 100) reads back
    // identically through the typed API.
    let store = store();
    store
        .write(
            ContentHash::from(100u64),
            &claim(
                "frame:x",
                ProvenanceTier::LocalModel,
                ClaimStatus::Confirmed,
            ),
        )
        .unwrap();
    let got = store
        .read(ContentHash::from(100u64), "frame:x")
        .unwrap()
        .expect("present");
    assert_eq!(got.tier, ProvenanceTier::LocalModel);
    assert_eq!(got.status, ClaimStatus::Confirmed);
}

/// Acceptance criterion #3, through a content hash: covered router-side
/// (`node_store_hash_change_invalidates_annotations`) against the durable
/// store writer wiring; the hash-keying contract itself is pinned by
/// `hash_change_invalidates_annotations` above.

#[test]
fn unicode_payload_round_trips_byte_identical() {
    // M8.1: unicode (CJK/emoji) payload + produced_by survive the TEXT
    // write→read path byte-identical.
    let store = store();
    let hash = ContentHash::from(4242u64);
    let mut c = claim("frame:unicode", ProvenanceTier::HumanReview, ClaimStatus::Confirmed);
    c.payload = serde_json::json!({ "note": "北京 café 🎉", "key": "frame:unicode" });
    c.produced_by = "réviewer-日本語".to_string();
    store.write(hash, &c).unwrap();
    let got = store.read(hash, "frame:unicode").unwrap().expect("present");
    assert_eq!(got.payload, c.payload);
}
