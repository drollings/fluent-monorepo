use super::*;

#[test]
fn tier_ordering_is_authority_ascending() {
    assert!(ProvenanceTier::Deterministic < ProvenanceTier::LocalModel);
    assert!(ProvenanceTier::LocalModel < ProvenanceTier::Frontier);
    assert!(ProvenanceTier::Frontier < ProvenanceTier::HumanReview);
    assert!(ProvenanceTier::Deterministic < ProvenanceTier::HumanReview);
}

#[test]
fn tier_and_status_wire_names_are_snake_case() {
    assert_eq!(
        serde_json::to_string(&ProvenanceTier::HumanReview).unwrap(),
        "\"human_review\""
    );
    assert_eq!(
        serde_json::to_string(&ProvenanceTier::LocalModel).unwrap(),
        "\"local_model\""
    );
    assert_eq!(
        serde_json::to_string(&ClaimStatus::Superseded).unwrap(),
        "\"superseded\""
    );
}

#[test]
fn claim_serde_roundtrip_and_constructors() {
    let confirmed = AnnotationClaim::confirmed(
        "frame:predicate:obj",
        ProvenanceTier::LocalModel,
        serde_json::json!({ "direct_object_id": 42 }),
        "onnx/llm",
        1700000000,
    );
    assert_eq!(confirmed.status, ClaimStatus::Confirmed);
    assert_eq!(confirmed.tier, ProvenanceTier::LocalModel);

    let provisional = AnnotationClaim::provisional(
        "frame:predicate:obj",
        ProvenanceTier::Deterministic,
        serde_json::json!({}),
        "arceager",
        1700000000,
    );
    assert_eq!(provisional.status, ClaimStatus::Provisional);

    for claim in [&confirmed, &provisional] {
        let json = serde_json::to_string(claim).expect("serialize");
        let back: AnnotationClaim = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, claim, "round trip for {json}");
    }
}
