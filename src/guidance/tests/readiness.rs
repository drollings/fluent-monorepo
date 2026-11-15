//! Readiness classification: coverage is not producer confidence.

use super::*;

fn ready_coverage() -> Coverage {
    Coverage {
        mirror: MirrorState::Fresh { total: 10 },
        index: IndexState::Ready,
        embeddings: EmbeddingCoverage {
            ready: 8,
            total: 10,
        },
        embeddings_off: false,
        capabilities: CapabilityCoverage {
            ready: 3,
            total: Some(3),
        },
        docs_present: true,
        doc_cards: 5,
        decision: DecisionSupport::Supported,
        support: DecisionSupport::Supported,
    }
}

#[test]
fn complete_coverage_is_ready() {
    assert!(classify(ready_coverage()).is_ready());
}

#[test]
fn missing_mirror_blocks_with_sync_repair() {
    let mut coverage = ready_coverage();
    coverage.mirror = MirrorState::Missing;
    let readiness = classify(coverage);
    assert!(readiness.is_blocked());
    assert!(readiness.repair_hint().unwrap().contains("guidance sync"));
}

#[test]
fn index_generation_mismatch_blocks() {
    let mut coverage = ready_coverage();
    coverage.index = IndexState::SchemaMismatch;
    let readiness = classify(coverage);
    assert!(readiness.is_blocked());
    assert!(readiness.repair_hint().unwrap().contains("rebuilds"));
}

#[test]
fn fully_stale_mirror_blocks_but_partial_stale_degrades() {
    let mut coverage = ready_coverage();
    coverage.mirror = MirrorState::Stale {
        total: 10,
        stale: 10,
    };
    assert!(classify(coverage).is_blocked());

    let mut coverage = ready_coverage();
    coverage.mirror = MirrorState::Stale {
        total: 10,
        stale: 2,
    };
    let readiness = classify(coverage);
    assert!(!readiness.is_blocked());
    assert!(readiness
        .degradation_reasons()
        .iter()
        .any(|reason| matches!(reason, Degradation::Stale { .. })));
}

#[test]
fn zero_embeddings_degrade_not_block() {
    let mut coverage = ready_coverage();
    coverage.embeddings.ready = 0;
    let readiness = classify(coverage);
    assert!(!readiness.is_blocked());
    assert!(readiness
        .degradation_reasons()
        .iter()
        .any(|reason| matches!(reason, Degradation::EmbeddingsMissing { .. })));
}

#[test]
fn absent_capability_corpus_degrades() {
    let mut coverage = ready_coverage();
    coverage.capabilities.ready = 0;
    let readiness = classify(coverage);
    assert!(!readiness.is_blocked());
    assert!(readiness
        .degradation_reasons()
        .iter()
        .any(|reason| matches!(reason, Degradation::CapabilitiesMissing)));
}

#[test]
fn unsupported_decision_transport_degrades() {
    let mut coverage = ready_coverage();
    coverage.decision = DecisionSupport::Unsupported {
        reason: "501".to_string(),
    };
    let readiness = classify(coverage);
    assert!(!readiness.is_blocked());
    assert!(readiness
        .degradation_reasons()
        .iter()
        .any(|reason| matches!(reason, Degradation::DecisionUnsupported { .. })));
}

#[test]
fn missing_docs_degrades() {
    let mut coverage = ready_coverage();
    coverage.docs_present = false;
    assert!(classify(coverage)
        .degradation_reasons()
        .iter()
        .any(|reason| matches!(reason, Degradation::DocsMissing)));
}

#[test]
fn docs_present_capabilities_absent_is_answerable() {
    let mut coverage = ready_coverage();
    coverage.capabilities.ready = 0;
    let readiness = classify(coverage);
    assert!(!readiness.is_blocked());
}

#[test]
fn docs_absent_is_orientation_gap() {
    let mut coverage = ready_coverage();
    coverage.docs_present = false;
    coverage.doc_cards = 0;
    let readiness = classify(coverage);
    assert!(readiness
        .degradation_reasons()
        .iter()
        .any(|reason| matches!(reason, Degradation::DocsMissing)));
}

#[test]
fn embeddings_off_is_distinct_from_missing() {
    let mut coverage = ready_coverage();
    coverage.embeddings_off = true;
    coverage.embeddings.ready = 0;
    assert!(!classify(coverage).is_blocked());
}
