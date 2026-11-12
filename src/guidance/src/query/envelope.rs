//! Unified answer envelope: every query returns intent, confidence with
//! its axis named, results, citations, gaps, and resolution method.
//!
//! Confidence axes are never substituted: a producer-side score orders
//! work or abstains, while only a task-value check (grounded excerpts,
//! verified citations, a passing gate) certifies. The envelope labels
//! which axis each verdict stands on.

use fluent_llm::tokens::TokenBudget;

use super::strategy::{QueryClass, QueryIntent};
use super::synthesize::Stage;
use crate::grounding::{
    can_synthesize, retrieval_tier_for_stage_tier, CitationFreshness, StageCitation,
};
use crate::search_types::RetrievalTier;

/// Which axis a confidence value stands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which axis a confidence value stands on.
pub enum ConfidenceAxis {
    /// Producer self-doubt (rule score, model entropy): orders work or
    /// abstains, never certifies, never persists a verdict.
    ProducerOrderOnly,
    /// Task-value outcome (adequate grounded evidence, verified citations,
    /// a passing gate): the only axis that certifies.
    TaskCertified,
}

/// One confidence value with its axis attached.
#[derive(Debug, Clone, Copy, PartialEq)]
/// One confidence value with its axis attached.
pub struct AnswerConfidence {
    /// Confidence magnitude in `[0.0, 1.0]`.
    pub value: f64,
    /// Which axis the value stands on.
    pub axis: ConfidenceAxis,
}

/// Machine-readable answer schema version. Bumped on any field
/// addition, removal, or rename; consumers reject what they do not know.
pub const ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// Machine-readable answer plus human-renderable fields.
#[derive(Debug, Clone)]
/// Machine-readable answer plus human-renderable fields.
pub struct AnswerEnvelope {
    /// Schema version of this envelope (`ENVELOPE_SCHEMA_VERSION`).
    pub schema_version: u32,
    /// Classified intent.
    pub intent: QueryIntent,
    /// Search primitive that answered (`word_index`, `fts`, `hybrid`, …).
    pub route: String,
    /// Confidence with its axis named.
    pub confidence: AnswerConfidence,
    /// Result stages in rank order.
    pub stages: Vec<Stage>,
    /// `source:line` citations from grounded code stages.
    pub citations: Vec<String>,
    /// Honest gaps (no results, ungrounded, ambiguous).
    pub gaps: Vec<String>,
    /// Worst citation freshness across the emitted lines: any stale
    /// line downgrades the envelope, unverified only when nothing stale.
    pub citation_freshness: CitationFreshness,
    /// How the answer was resolved, including precheck notes.
    pub resolution: String,
    /// Additive retrieval tiers behind the results, sorted and distinct.
    /// Naming only; routing never consults them.
    pub tiers: Vec<String>,
}

impl AnswerEnvelope {
    /// Build an envelope from a classified query, its route, and its
    /// stages. Pure: no I/O, no cache, no model. Grounding decides the
    /// axis — adequate excerpts certify, anything else stays producer-only.
    #[must_use]
    pub fn from_stages(class: &QueryClass, route: &str, stages: Vec<Stage>) -> Self {
        let grounded = can_synthesize(&stages);
        let citations = citations_of(&stages);
        let gaps = gaps_of(&stages, grounded);
        let prechecked = super::strategy::intent_precheck(class).is_some();
        Self {
            schema_version: ENVELOPE_SCHEMA_VERSION,
            intent: class.intent,
            route: route.to_string(),
            confidence: AnswerConfidence {
                value: f64::from(class.confidence),
                axis: if grounded {
                    ConfidenceAxis::TaskCertified
                } else {
                    ConfidenceAxis::ProducerOrderOnly
                },
            },
            stages,
            citations,
            gaps,
            citation_freshness: CitationFreshness::Unverified,
            resolution: if prechecked {
                format!("{route} (deterministic precheck)")
            } else {
                route.to_string()
            },
            tiers: vec![tier_for_intent(class.intent).as_str().to_string()],
        }
    }

    /// Build an envelope from hash-verified citations: emitted lines
    /// carry ranges plus hashes, the summary is worst-of, and stale or
    /// unverified lines file explicit gaps. Negatives stay
    /// coverage-qualified, never absolute.
    #[must_use]
    pub fn from_verified_stages(
        class: &QueryClass,
        route: &str,
        stages: Vec<Stage>,
        cited: &[StageCitation],
    ) -> Self {
        let grounded = can_synthesize(&stages);
        let citations: Vec<String> = cited.iter().map(|c| c.cite.clone()).collect();
        let mut gaps = gaps_of(&stages, grounded);
        let stale = cited
            .iter()
            .filter(|c| c.freshness == CitationFreshness::Stale)
            .count();
        let unverified = cited
            .iter()
            .filter(|c| c.freshness == CitationFreshness::Unverified)
            .count();
        if stale > 0 {
            gaps.push(format!(
                "{stale} stale citations queued for reprocessing; macro-expanded, cfg-disabled, and build-script code not covered"
            ));
        }
        if unverified > 0 {
            gaps.push(format!(
                "{unverified} citations unverified against stored rows (legacy receipts)"
            ));
        }
        let citation_freshness = if stale > 0 {
            CitationFreshness::Stale
        } else if unverified > 0 {
            CitationFreshness::Unverified
        } else {
            CitationFreshness::Fresh
        };
        let prechecked = super::strategy::intent_precheck(class).is_some();
        Self {
            schema_version: ENVELOPE_SCHEMA_VERSION,
            intent: class.intent,
            route: route.to_string(),
            confidence: AnswerConfidence {
                value: f64::from(class.confidence),
                axis: if grounded && stale == 0 {
                    ConfidenceAxis::TaskCertified
                } else {
                    ConfidenceAxis::ProducerOrderOnly
                },
            },
            stages,
            citations,
            gaps,
            citation_freshness,
            tiers: tiers_for_cited(class, cited),
            resolution: {
                let mut tiers: Vec<&str> = cited.iter().map(|c| c.tier.as_str()).collect();
                tiers.sort_unstable();
                tiers.dedup();
                let method = if tiers.is_empty() {
                    route.to_string()
                } else {
                    format!("{route} (tiers: {})", tiers.join(", "))
                };
                if prechecked {
                    format!("{method} (deterministic precheck)")
                } else {
                    method
                }
            },
        }
    }

    /// Whether the envelope carries an answerable result. Task-value
    /// check: grounded excerpts exist, never inferred from confidence.
    #[must_use]
    pub fn is_answerable(&self) -> bool {
        self.gaps
            .iter()
            .all(|gap| !gap.starts_with("no results") && !gap.starts_with("synthesis withheld"))
    }

    /// Human-readable markdown rendering.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        use std::fmt::Write as _;
        let mut out = format!(
            "intent: {:?}\nroute: {}\nconfidence: {:.2} ({:?})\nresolution: {}\n",
            self.intent, self.route, self.confidence.value, self.confidence.axis, self.resolution
        );
        for stage in &self.stages {
            match stage.line {
                Some(line) => {
                    let _ = writeln!(out, "- {}:{line} {}", stage.source, stage.content);
                }
                None => {
                    let _ = writeln!(out, "- {} {}", stage.source, stage.content);
                }
            }
        }
        if !self.citations.is_empty() {
            let _ = writeln!(out, "citations: {}", self.citations.join(", "));
        }
        for gap in &self.gaps {
            let _ = writeln!(out, "gap: {gap}");
        }
        out
    }
}

/// `source:line` citations from grounded code stages only.
#[must_use]
/// `source:line` citations from grounded code stages only.
pub fn citations_of(stages: &[Stage]) -> Vec<String> {
    stages
        .iter()
        .filter(|stage| {
            stage.kind == fluent_types::StageKind::Code
                && !stage.source.is_empty()
                && stage.line.is_some()
        })
        .map(|stage| format!("{}:{}", stage.source, stage.line.unwrap_or(0)))
        .collect()
}

/// Honest gaps: empty results, withheld ungrounded synthesis, ambiguity.
#[must_use]
/// Honest gaps: empty results, withheld ungrounded synthesis, ambiguity.
pub fn gaps_of(stages: &[Stage], grounded: bool) -> Vec<String> {
    let mut gaps = Vec::new();
    if stages.is_empty() {
        gaps.push("no results for query (none found in covered index)".to_string());
        return gaps;
    }
    if !grounded {
        gaps.push("synthesis withheld: no grounded excerpt".to_string());
    }
    use std::collections::HashMap;
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for stage in stages {
        if let Some(name) = stage.member_name.as_deref() {
            *counts.entry(name).or_default() += 1;
        }
    }
    let mut names: Vec<(&str, usize)> =
        counts.into_iter().filter(|(_, count)| *count > 1).collect();
    names.sort_unstable();
    for (name, count) in names {
        gaps.push(format!("ambiguous: {count} candidates for `{name}`"));
    }
    gaps
}

/// Rank-preserving budget fit over stages in rank order.
///
/// Shares the `TokenBudget` estimator with the rest of the workspace, but
/// deliberately not `ffd_pack`: rank order is part of the answer's meaning
/// and re-sorting by size would corrupt it. Keeps stages while the running
/// estimate fits; drops the tail, never the head.
#[must_use]
/// Rank-preserving budget fit over stages in rank order.
///
/// Shares the `TokenBudget` estimator with the rest of the workspace, but
/// deliberately not `ffd_pack`: rank order is part of the answer's meaning
/// and re-sorting by size would corrupt it. Keeps stages while the running
/// estimate fits; drops the tail, never the head.
pub fn fit_stages_to_budget(stages: &[Stage], max_tokens: usize) -> Vec<Stage> {
    let mut budget = TokenBudget(max_tokens);
    let mut out = Vec::new();
    for stage in stages {
        let cost = fluent_llm::tokens::estimate_tokens(&stage.content) as usize;
        if budget.consume(cost) {
            out.push(stage.clone());
        } else {
            break;
        }
    }
    out
}

/// Tier label for a legacy fallback answer: exact intents resolve by
/// identifier or path, lexical intents by keyword, semantic intents by
/// model-backed concept recall. Pure naming over the classified intent.
#[must_use]
/// Tier label for a legacy fallback answer: exact intents resolve by
/// identifier or path, lexical intents by keyword, semantic intents by
/// model-backed concept recall. Pure naming over the classified intent.
pub fn tier_for_intent(intent: QueryIntent) -> RetrievalTier {
    match intent {
        QueryIntent::IdentifierLookup | QueryIntent::SingleIdentifier | QueryIntent::FilePath => {
            RetrievalTier::Exact
        }
        QueryIntent::CapabilityQuery | QueryIntent::MultiKeyword | QueryIntent::GeneralSearch => {
            RetrievalTier::Lexical
        }
        QueryIntent::HowTo | QueryIntent::Conceptual => RetrievalTier::Semantic,
    }
}

/// Distinct tier names across cited stages, falling back to the intent
/// tier when no citation carries a known tier.
#[must_use]
/// Distinct tier names across cited stages, falling back to the intent
/// tier when no citation carries a known tier.
pub fn tiers_for_cited(class: &QueryClass, cited: &[StageCitation]) -> Vec<String> {
    let mut tiers: Vec<String> = cited
        .iter()
        .filter_map(|c| retrieval_tier_for_stage_tier(&c.tier))
        .map(|tier| tier.as_str().to_string())
        .collect();
    tiers.sort();
    tiers.dedup();
    if tiers.is_empty() {
        tiers.push(tier_for_intent(class.intent).as_str().to_string());
    }
    tiers
}
/// Whether an exact precheck verdict restricts dispatch to its primary
/// rung. Producer-side gate: high-confidence deterministic intents never
/// pay vector recall; ambiguous queries keep the full ladder.
#[must_use]
/// Whether an exact precheck verdict restricts dispatch to its primary
/// rung. Producer-side gate: high-confidence deterministic intents never
/// pay vector recall; ambiguous queries keep the full ladder.
pub fn should_short_circuit(class: &QueryClass) -> bool {
    matches!(
        class.intent,
        QueryIntent::SingleIdentifier | QueryIntent::FilePath
    ) && class.confidence >= 0.9
}

#[cfg(test)]
#[path = "../../tests/answer_envelope.rs"]
mod envelope_tests;

#[cfg(test)]
#[path = "../../tests/staged_routing.rs"]
mod staged_routing_tests;
