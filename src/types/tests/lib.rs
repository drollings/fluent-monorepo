use super::*;

#[test]
fn node_id_roundtrip() {
    let id = NodeId::from_int(42);
    assert_eq!(id.as_int(), 42);
}
#[test]
fn session_id_roundtrip() {
    let id = SessionId::new("test_session");
    assert_eq!(id.as_str(), "test_session");
}
#[test]
fn types_are_distinct() {
    fn takes_node(_: NodeId) {}
    fn takes_session(_: &SessionId) {}
    takes_node(NodeId::from_int(1));
    let sess = SessionId::new("test");
    takes_session(&sess);
}
#[test]
fn member_serde_roundtrip() {
    let m = Member {
        type_name: MemberType::FnDecl,
        name: SmolStr::new("foo"),
        match_hash: None,
        signature: Some(SmolStr::new("fn foo()")),
        params: vec![],
        returns: None,
        comment: Some(SmolStr::new("test function")),
        tags: vec![SmolStr::new("public")],
        is_pub: true,
        members: vec![],
        equivalents: vec![],
        line: Some(10),
        span_start: None,
        span_end: None,
        item_hash: None,
        range_hash: None,
        comment_generated: false,
    };
    let json = serde_json::to_string(&m).unwrap();
    let m2: Member = serde_json::from_str(&json).unwrap();
    assert_eq!(m.name, m2.name);
    assert_eq!(m.is_pub, m2.is_pub);
}
#[test]
fn guidance_doc_default_language_on_deserialize() {
    let json = r#"{"meta":{"module":"test","source":"test.zig"}}"#;
    let doc: GuidanceDoc = serde_json::from_str(json).unwrap();
    assert_eq!(doc.meta.language.as_str(), "zig");
}
#[test]
fn guidance_doc_serde_roundtrip() {
    let doc = GuidanceDoc {
        meta: Meta {
            module: SmolStr::new("test"),
            source: SmolStr::new("test.zig"),
            language: SmolStr::new("zig"),
        },
        comment: Some(SmolStr::new("module comment")),
        detail: None,
        keywords: vec![SmolStr::new("api")],
        skills: vec![],
        capabilities: vec![],
        hashtags: vec![],
        used_by: vec![],
        members: vec![],
        equivalents: vec![],
        capability_eval: None,
        summary: None,
        summaries: std::collections::BTreeMap::new(),
    };
    let json = serde_json::to_string(&doc).unwrap();
    let doc2: GuidanceDoc = serde_json::from_str(&json).unwrap();
    assert_eq!(doc.meta.module, doc2.meta.module);
    assert_eq!(doc.comment, doc2.comment);
}
#[test]
fn knn_hit_creation() {
    let hit = KnnHit {
        node_id: NodeId::from_int(1),
        distance: 0.95,
        name: SmolStr::new("test"),
    };
    assert_eq!(hit.node_id.as_int(), 1);
}
#[test]
fn stage_kind_serde() {
    let kinds = [StageKind::Prose, StageKind::Code, StageKind::NotFound];
    for kind in &kinds {
        let json = serde_json::to_string(kind).unwrap();
        let k2: StageKind = serde_json::from_str(&json).unwrap();
        assert_eq!(*kind, k2);
    }
}
#[test]
fn target_type_serde() {
    let t = TargetType::File;
    let json = serde_json::to_string(&t).unwrap();
    assert_eq!(json, "\"file\"");
    let t2: TargetType = serde_json::from_str(&json).unwrap();
    assert_eq!(t, t2);
}
#[test]
fn executor_kind_serde() {
    let e = ExecutorKind::Wasm;
    let json = serde_json::to_string(&e).unwrap();
    assert_eq!(json, "\"wasm\"");
}
#[test]
fn overlay_status_absent_pending_ready() {
    let mut node = ContentNode::default();
    assert_eq!(
        node.overlay(OverlayKind::Spacy).status,
        OverlayStatus::Absent
    );
    node.transition_overlay(OverlayKind::Spacy, OverlayStatus::Pending, "arceager", None)
        .expect("pending");
    assert_eq!(
        node.overlay(OverlayKind::Spacy).status,
        OverlayStatus::Pending
    );
    let ready = node
        .transition_overlay(
            OverlayKind::Spacy,
            OverlayStatus::Ready,
            "arceager",
            Some(1000),
        )
        .expect("ready");
    assert_eq!(ready.status, OverlayStatus::Ready);
    assert_eq!(ready.at, Some(1000));
    assert_eq!(
        node.overlay(OverlayKind::Spacy).status,
        OverlayStatus::Ready
    );
    // Other kinds stay untouched.
    assert_eq!(node.overlay(OverlayKind::Llm).status, OverlayStatus::Absent);
}

#[test]
fn overlay_status_absent_pending_failed() {
    let mut node = ContentNode::default();
    node.transition_overlay(OverlayKind::Embedding, OverlayStatus::Pending, "bge", None)
        .expect("pending");
    let failed = node
        .transition_overlay(
            OverlayKind::Embedding,
            OverlayStatus::Failed,
            "bge",
            Some(2000),
        )
        .expect("failed");
    assert_eq!(failed.status, OverlayStatus::Failed);
    assert_eq!(
        node.overlay(OverlayKind::Embedding).status,
        OverlayStatus::Failed
    );
}

#[test]
fn overlay_failed_is_permanent_until_reset() {
    let mut node = ContentNode::default();
    node.transition_overlay(OverlayKind::Llm, OverlayStatus::Pending, "llama", None)
        .expect("pending");
    node.transition_overlay(OverlayKind::Llm, OverlayStatus::Failed, "llama", Some(3000))
        .expect("failed");
    // A retry is rejected: a failed overlay is never re-derived.
    assert_eq!(
        node.transition_overlay(OverlayKind::Llm, OverlayStatus::Pending, "llama", None),
        Err(OverlayTransitionError::FailedPermanent(OverlayKind::Llm))
    );
    assert_eq!(
        node.transition_overlay(OverlayKind::Llm, OverlayStatus::Ready, "llama", Some(4000)),
        Err(OverlayTransitionError::FailedPermanent(OverlayKind::Llm))
    );
    // The only exit is an explicit reset (the node changed) → re-derive.
    node.transition_overlay(OverlayKind::Llm, OverlayStatus::Absent, "", None)
        .expect("reset");
    node.transition_overlay(OverlayKind::Llm, OverlayStatus::Pending, "llama", None)
        .expect("re-pending after reset");
    node.transition_overlay(OverlayKind::Llm, OverlayStatus::Ready, "llama", Some(5000))
        .expect("re-ready after reset");
    assert_eq!(node.overlay(OverlayKind::Llm).status, OverlayStatus::Ready);
}

#[test]
fn overlay_ready_is_not_recomputed() {
    let mut node = ContentNode::default();
    node.transition_overlay(
        OverlayKind::Spacy,
        OverlayStatus::Ready,
        "arceager",
        Some(1),
    )
    .expect("ready");
    assert_eq!(
        node.transition_overlay(OverlayKind::Spacy, OverlayStatus::Pending, "llm", None),
        Err(OverlayTransitionError::AlreadyReady(OverlayKind::Spacy))
    );
    assert_eq!(
        node.transition_overlay(OverlayKind::Spacy, OverlayStatus::Failed, "llm", Some(2)),
        Err(OverlayTransitionError::AlreadyReady(OverlayKind::Spacy))
    );
    // Equal-status no-op is allowed.
    node.transition_overlay(
        OverlayKind::Spacy,
        OverlayStatus::Ready,
        "arceager",
        Some(3),
    )
    .expect("no-op");
    assert_eq!(
        node.overlay(OverlayKind::Spacy).status,
        OverlayStatus::Ready
    );
}

#[test]
fn overlay_state_serde_roundtrip() {
    let state = OverlayState::ready(OverlayKind::Spacy, "arceager", 42);
    let json = serde_json::to_string(&state).expect("serialize");
    let back: OverlayState = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, state);
    // Wire names are snake_case.
    assert_eq!(
        serde_json::to_string(&OverlayKind::Spacy).unwrap(),
        "\"spacy\""
    );
    assert_eq!(
        serde_json::to_string(&OverlayKind::Embedding).unwrap(),
        "\"embedding\""
    );
    assert_eq!(
        serde_json::to_string(&OverlayStatus::Pending).unwrap(),
        "\"pending\""
    );
    assert_eq!(
        serde_json::to_string(&OverlayStatus::Failed).unwrap(),
        "\"failed\""
    );
    // Absent entries drop the `at` field.
    let absent = OverlayState::absent(OverlayKind::Llm);
    let json = serde_json::to_string(&absent).expect("serialize");
    assert!(!json.contains("\"at\""));
    let back: OverlayState = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, absent);
}

#[test]
fn overlay_bookkeeping_lives_in_metadata_overlays() {
    let mut node = ContentNode::default();
    node.set_overlay(OverlayState::pending(OverlayKind::Spacy, "arceager"));
    // Stored under the namespaced key, as advisory JSON.
    let meta = node.metadata.as_ref().expect("metadata allocated");
    assert!(meta.get(OVERLAYS_META_KEY).is_some());
    // Survives a ContentNode serde round-trip (the durable content_json).
    let json = serde_json::to_string(&node).expect("serialize");
    let back: ContentNode = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        back.overlay(OverlayKind::Spacy).status,
        OverlayStatus::Pending
    );
    // Updating the same kind replaces, never duplicates.
    node.set_overlay(OverlayState::ready(OverlayKind::Spacy, "arceager", 9));
    assert_eq!(node.overlays().len(), 1);
    assert_eq!(
        node.overlay(OverlayKind::Spacy).status,
        OverlayStatus::Ready
    );
}

#[test]
fn overlay_ledger_transition_matrix() {
    use OverlayStatus::{Absent, Failed, Pending, Ready};
    // Every (from → to) pair, exercised purely on the ledger: terminal
    // statuses exit only via reset; equal-status no-ops always pass.
    for kind in [OverlayKind::Spacy, OverlayKind::Llm, OverlayKind::Embedding] {
        for from in [Absent, Pending, Ready, Failed] {
            for to in [Absent, Pending, Ready, Failed] {
                let mut ledger = OverlayLedger::default();
                if from != Absent {
                    // Drive the ledger to `from` through a legal prefix.
                    ledger.transition(kind, Pending, "t", None).ok();
                    if from == Ready || from == Failed {
                        ledger.transition(kind, from, "t", Some(1)).ok();
                    }
                    assert_eq!(ledger.get(kind).status, from, "{kind:?} setup");
                }
                let result = ledger.transition(kind, to, "t", Some(2));
                let terminal = from == Ready || from == Failed;
                if !terminal || to == from || to == Absent {
                    assert!(result.is_ok(), "{kind:?} {from:?} → {to:?} must pass");
                    assert_eq!(ledger.get(kind).status, to);
                } else {
                    assert!(result.is_err(), "{kind:?} {from:?} → {to:?} must reject");
                    assert_eq!(ledger.get(kind).status, from);
                }
            }
        }
    }
}

#[test]
fn overlay_ledger_roundtrips_through_node_metadata() {
    // The ledger is the pure half: node reads/writes agree with it entry
    // for entry, and unrelated kinds stay absent.
    let mut ledger = OverlayLedger::default();
    ledger
        .transition(OverlayKind::Spacy, OverlayStatus::Pending, "arceager", None)
        .expect("pending");
    ledger
        .transition(
            OverlayKind::Spacy,
            OverlayStatus::Ready,
            "arceager",
            Some(7),
        )
        .expect("ready");
    let mut node = ContentNode::default();
    node.set_overlay_ledger(&ledger);
    assert_eq!(node.overlays().len(), 1);
    let back = node.overlay_ledger();
    assert_eq!(back.get(OverlayKind::Spacy).status, OverlayStatus::Ready);
    assert_eq!(back.get(OverlayKind::Spacy).at, Some(7));
    assert_eq!(back.get(OverlayKind::Llm).status, OverlayStatus::Absent);
    // Delegated entry points agree with the ledger.
    assert_eq!(
        node.overlay(OverlayKind::Spacy),
        back.get(OverlayKind::Spacy)
    );
}

#[test]
fn origin_role_unknown_preserves_case_everywhere() {
    // All three parse sites agree: known names fold case, unknown keeps it.
    assert_eq!(
        OriginRole::from("CuStOm".to_string()),
        OriginRole::Other("CuStOm".to_string())
    );
    assert_eq!(OriginRole::from("USER"), OriginRole::User);
    let de: OriginRole = serde_json::from_str("\"CuStOm\"").expect("deserialize");
    assert_eq!(de, OriginRole::Other("CuStOm".to_string()));
    let de_known: OriginRole = serde_json::from_str("\"SELF\"").expect("deserialize");
    assert_eq!(de_known, OriginRole::SelfOrigin);
    // The Option site (ContentNode.role) preserves case too.
    let node: ContentNode =
        serde_json::from_str(r#"{"name":"n","source":"s","role":"MiXeD"}"#).expect("deserialize");
    assert_eq!(node.role, Some(OriginRole::Other("MiXeD".to_string())));
    // Serialize round-trips the preserved case.
    let json = serde_json::to_string(&OriginRole::Other("MiXeD".to_string())).unwrap();
    assert_eq!(json, "\"MiXeD\"");
}

#[test]
fn content_node_defaults() {
    let node = ContentNode {
        id: None,
        name: SmolStr::new("root"),
        source: "full text".into(),
        content_hash: ContentHash::UNSTAMPED,
        lod: vec!["hello".into()],
        embedding: None,
        capabilities: None,
        session_id: None,
        request_id: None,
        role: None,
        turn_index: None,
        accepted: None,
        acceptance_score: None,
        active_lod: None,
        parent_id: None,
        step_id: None,
        step_status: None,
        metadata: None,
        created_at: None,
        annotation: None,
    };
    assert_eq!(node.lod.len(), 1);
    assert_eq!(node.content(), Some("hello"));
    assert!(node.embedding.is_none());
    assert!(node.annotation.is_none());
}

/// A concrete, dependency-free overlay for exercising the opaque slot.
#[derive(Debug, Clone)]
struct TestOverlay {
    value: u64,
}

impl NodeOverlay for TestOverlay {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Debug, Clone)]
struct OtherOverlay;

impl NodeOverlay for OtherOverlay {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn content_node_annotation_slot_downedcasts_and_shares() {
    let overlay = TestOverlay { value: 42 };
    let node = ContentNode {
        annotation: Some(std::sync::Arc::new(overlay)),
        ..ContentNode::default()
    };
    // Typed read through the opaque slot (the router's read path).
    let got = node.annotation_as::<TestOverlay>().expect("downcast");
    assert_eq!(got.value, 42);
    // Unknown overlay types read as absent, not a panic.
    assert!(node.annotation_as::<OtherOverlay>().is_none());
    // Shared: two holders read the same immutable overlay without a lock.
    let shared = node.annotation.clone().expect("shared slot");
    assert_eq!(
        shared.as_any().downcast_ref::<TestOverlay>().unwrap().value,
        42
    );
    // The node remains Debug/Clone/Default with the slot populated.
    let clone = node.clone();
    assert_eq!(clone.annotation_as::<TestOverlay>().unwrap().value, 42);
}

#[test]
fn content_node_annotation_is_never_serialized() {
    let node = ContentNode {
        annotation: Some(std::sync::Arc::new(TestOverlay { value: 7 })),
        ..ContentNode::default()
    };
    let json = serde_json::to_string(&node).expect("serialize");
    assert!(
        !json.contains("annotation"),
        "the overlay is in-memory only and must not leak into content_json: {json}"
    );
    // A node deserialized from that durable JSON carries no overlay.
    let back: ContentNode = serde_json::from_str(&json).expect("deserialize");
    assert!(back.annotation.is_none());
}

#[test]
fn content_node_back_compat_pre_annotation_field() {
    // A `ContentNode` serialized before the annotation slot existed (no
    // `annotation` key, no `..Default` churn) deserializes correctly with
    // `annotation: None` (OVERLAYS M4.2).
    let pre_field_json = r#"{
        "id": 7,
        "name": "legacy",
        "source": "old full text",
        "lod": ["old full text", "old summary"],
        "embedding": null,
        "capabilities": null,
        "session_id": null,
        "request_id": null,
        "role": null,
        "turn_index": null,
        "accepted": null,
        "acceptance_score": null,
        "active_lod": null,
        "parent_id": null,
        "step_id": null,
        "step_status": null,
        "metadata": null,
        "created_at": null
    }"#;
    let node: ContentNode = serde_json::from_str(pre_field_json).expect("deserialize");
    assert_eq!(node.name.as_str(), "legacy");
    assert_eq!(node.lod.len(), 2);
    assert!(node.annotation.is_none(), "legacy node has no overlay");
    // The node round-trips through the store's content_json unchanged.
    let json = serde_json::to_string(&node).expect("serialize");
    let back: ContentNode = serde_json::from_str(&json).expect("round trip");
    assert_eq!(back.name.as_str(), "legacy");
    assert!(back.annotation.is_none());
    // `content_hash` predates this node's serialization → defaults to UNSTAMPED.
    assert_eq!(
        node.content_hash,
        ContentHash::UNSTAMPED,
        "missing content_hash back-compat defaults to UNSTAMPED"
    );
}
#[test]
fn content_hash_wire_is_plain_u64() {
    let json = serde_json::to_string(&ContentHash::UNSTAMPED).expect("serialize");
    assert_eq!(json, "0");
    let back: ContentHash = serde_json::from_str("0").expect("deserialize");
    assert_eq!(back, ContentHash::UNSTAMPED);
    assert!(back.is_unstamped());
    // A node whose JSON carries the old numeric shape still parses.
    let node: ContentNode =
        serde_json::from_str(r#"{"id":null,"name":"n","source":"s","content_hash":0,"lod":[]}"#)
            .expect("old row parses");
    assert_eq!(node.content_hash, ContentHash::UNSTAMPED);
    // Serialized UNSTAMPED node emits the same `0` number.
    let out = serde_json::to_string(&node).expect("serialize");
    assert!(out.contains(r#""content_hash":0"#), "{out}");
}

#[test]
fn content_hash_from_lod0_matches_fnv() {
    assert_eq!(ContentHash::from_lod0(""), ContentHash::UNSTAMPED);
    let h = ContentHash::from_lod0("x");
    assert!(!h.is_unstamped());
    assert_eq!(h.as_u64(), common_core::hash::fnv1a64(b"x"));
    assert_eq!(ContentHash::default(), ContentHash::UNSTAMPED);
}

#[test]
fn wasm_tool_serde() {
    let tool = WasmTool {
        name: SmolStr::new("tokenizer"),
        path: "/usr/local/bin/guidance-tokenizer.wasm".into(),
        capabilities: vec![SmolStr::new("tokenize")],
    };
    let json = serde_json::to_string(&tool).unwrap();
    let t2: WasmTool = serde_json::from_str(&json).unwrap();
    assert_eq!(tool.name, t2.name);
}
#[test]
fn context_hit_roundtrip() {
    let hit = ContextHit {
        source: "ledger".into(),
        content: "2+2 is 4".into(),
        score: 0.97,
        metadata: Some(serde_json::json!({ "node_id": 42 })),
    };
    let json = serde_json::to_string(&hit).unwrap();
    let back: ContextHit = serde_json::from_str(&json).unwrap();
    assert_eq!(back, hit);
    assert_eq!(back.source, "ledger");
}
#[test]
fn context_hit_defaults_metadata_to_none() {
    let hit: ContextHit =
        serde_json::from_str(r#"{"source":"c","content":"x","score":0.5}"#).unwrap();
    assert!(hit.metadata.is_none());
    assert!((hit.score - 0.5).abs() < f32::EPSILON);
}

#[test]
fn overlay_transition_error_display() {
    let ready = OverlayTransitionError::AlreadyReady(OverlayKind::Spacy);
    let text = format!("{ready}");
    assert!(
        text.contains("Ready"),
        "diagnoses the terminal status: {text}"
    );
    let failed = OverlayTransitionError::FailedPermanent(OverlayKind::Llm);
    let text = format!("{failed}");
    assert!(
        text.to_lowercase().contains("failed"),
        "diagnoses the terminal status: {text}"
    );
    // `?`-compatible: the error travels through `Result` combinators.
    let r: Result<OverlayState, OverlayTransitionError> = Err(failed);
    assert!(r.map(|_| ()).is_err());
}
