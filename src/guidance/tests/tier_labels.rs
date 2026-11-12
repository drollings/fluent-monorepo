//! Retrieval tiers: every hit, edge, and answer carries exactly one
//! additive label (exact, lexical, fusion, semantic). Dispatch and
//! fallback stay byte-identical; labels only name the machinery.

use crate::graph_index::EdgeTier;
use crate::grounding::StageCitation;
use crate::query::envelope::tier_for_intent;
use crate::query::strategy::QueryIntent;
use crate::search_types::{RecallPath, RetrievalTier, SearchMatchedBy};

#[test]
fn every_matched_by_carries_exactly_one_tier() {
    assert_eq!(
        SearchMatchedBy::Fts.retrieval_tier(),
        RetrievalTier::Lexical
    );
    assert_eq!(
        SearchMatchedBy::Vector.retrieval_tier(),
        RetrievalTier::Semantic
    );
    assert_eq!(
        SearchMatchedBy::FtsAndVector.retrieval_tier(),
        RetrievalTier::Fusion
    );
    assert_eq!(
        SearchMatchedBy::Summary.retrieval_tier(),
        RetrievalTier::Model
    );
    for tier in [
        RetrievalTier::Exact,
        RetrievalTier::Lexical,
        RetrievalTier::Fusion,
        RetrievalTier::Semantic,
        RetrievalTier::Model,
    ] {
        assert_eq!(tier.as_str(), RetrievalTier::from(tier.as_str()).as_str());
    }
}

#[test]
fn recall_paths_map_to_tiers() {
    assert_eq!(RecallPath::Fts.retrieval_tier(), RetrievalTier::Lexical);
    assert_eq!(RecallPath::Lemma.retrieval_tier(), RetrievalTier::Lexical);
    assert_eq!(RecallPath::Vector.retrieval_tier(), RetrievalTier::Semantic);
    assert_eq!(RecallPath::Summary.retrieval_tier(), RetrievalTier::Model);
}

#[test]
fn degraded_syntactic_service_still_labels() {
    assert_eq!(EdgeTier::Syntactic.retrieval_tier(), RetrievalTier::Exact);
    assert_eq!(EdgeTier::Resolved.retrieval_tier(), RetrievalTier::Exact);
}

#[test]
fn citation_tiers_map_without_invention() {
    let citation = |tier: &str| StageCitation {
        cite: "a.rs:1-2 @ b3:x".to_string(),
        freshness: crate::grounding::CitationFreshness::Fresh,
        member: None,
        source: "a.rs".to_string(),
        tier: tier.to_string(),
    };
    assert_eq!(
        citation("fts").retrieval_tier(),
        Some(RetrievalTier::Lexical)
    );
    assert_eq!(
        citation("vector").retrieval_tier(),
        Some(RetrievalTier::Semantic)
    );
    assert_eq!(
        citation("fts+vector").retrieval_tier(),
        Some(RetrievalTier::Fusion)
    );
    assert_eq!(citation("doc").retrieval_tier(), Some(RetrievalTier::Exact));
    assert_eq!(citation("mystery").retrieval_tier(), None);
}

#[test]
fn legacy_fallback_labels_by_intent() {
    assert_eq!(
        tier_for_intent(QueryIntent::SingleIdentifier),
        RetrievalTier::Exact
    );
    assert_eq!(
        tier_for_intent(QueryIntent::IdentifierLookup),
        RetrievalTier::Exact
    );
    assert_eq!(tier_for_intent(QueryIntent::FilePath), RetrievalTier::Exact);
    assert_eq!(
        tier_for_intent(QueryIntent::CapabilityQuery),
        RetrievalTier::Lexical
    );
    assert_eq!(
        tier_for_intent(QueryIntent::MultiKeyword),
        RetrievalTier::Lexical
    );
    assert_eq!(
        tier_for_intent(QueryIntent::GeneralSearch),
        RetrievalTier::Lexical
    );
    assert_eq!(tier_for_intent(QueryIntent::HowTo), RetrievalTier::Semantic);
    assert_eq!(
        tier_for_intent(QueryIntent::Conceptual),
        RetrievalTier::Semantic
    );
}
