//! Shadow decision hooks for the guidance query pipeline.
//!
//! Each hook runs a precomputed `Decision` alongside the authoritative
//! deterministic path and records a `ShadowRecord` — label, confidence,
//! latency, would-have-acted — without altering the response. Indexed source
//! text stays untrusted: no record here guards a side effect, and remote
//! endpoints stay behind the transport opt-in. `NoopLlmFilter` remains the
//! default scorer until per-point calibration greens.

use fluent_llm::decision::{decide_verdict, Decision, ThresholdSet, Verdict};

/// One shadow observation at a named decision point.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// One shadow observation at a named decision point.
pub struct ShadowRecord {
    /// Decision point: `intent`, `sufficiency`, or `excerpt`.
    pub point: String,
    pub label: String,
    pub confidence: f64,
    pub margin: f64,
    pub latency_ms: u64,
    pub cached: bool,
    pub verdict: String,
    pub would_have_acted: bool,
    /// The authoritative outcome the record was logged against.
    pub authoritative: String,
}

fn verdict_spelling(verdict: Verdict) -> String {
    match verdict {
        Verdict::Act => "act".to_string(),
        Verdict::Review => "review".to_string(),
        Verdict::Escalate => "escalate".to_string(),
    }
}

fn record(
    point: &str,
    decision: &Decision,
    thresholds: &ThresholdSet,
    authoritative: String,
) -> ShadowRecord {
    let verdict = decide_verdict(decision, thresholds);
    ShadowRecord {
        point: point.to_string(),
        label: decision.label.clone(),
        confidence: decision.confidence,
        margin: decision.margin,
        latency_ms: decision.latency_ms,
        cached: decision.cached,
        verdict: verdict_spelling(verdict),
        would_have_acted: verdict == Verdict::Act,
        authoritative,
    }
}

/// Intent-Choice shadow: the deterministic `QueryIntent` wins; the decision
/// is only logged with its would-have-acted verdict.
#[must_use]
/// Intent-Choice shadow: the deterministic `QueryIntent` wins; the decision
/// is only logged with its would-have-acted verdict.
pub fn intent_shadow(
    authoritative_intent: &str,
    decision: Option<&Decision>,
    thresholds: &ThresholdSet,
) -> Option<ShadowRecord> {
    decision.map(|d| record("intent", d, thresholds, authoritative_intent.to_string()))
}

/// Evidence-sufficiency Noul shadow over recall/fusion outputs: logs whether
/// the decision would have filtered the evidence set.
#[must_use]
/// Evidence-sufficiency Noul shadow over recall/fusion outputs: logs whether
/// the decision would have filtered the evidence set.
pub fn sufficiency_shadow(
    evidence_count: usize,
    decision: Option<&Decision>,
    thresholds: &ThresholdSet,
) -> Option<ShadowRecord> {
    decision.map(|d| {
        record(
            "sufficiency",
            d,
            thresholds,
            format!("evidence:{evidence_count}"),
        )
    })
}

/// Excerpt filter/rerank shadow: rank delta of each candidate between the
/// prior order and the model order, with the decisions that produced it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// Excerpt filter/rerank shadow: rank delta of each candidate between the
/// prior order and the model order, with the decisions that produced it.
pub struct RankDelta {
    pub candidate: String,
    pub prior_rank: Option<usize>,
    pub model_rank: Option<usize>,
    pub confidence: f64,
}

#[must_use]
pub fn rerank_shadow(
    prior_order: &[String],
    model_order: &[String],
    decisions: &[(String, Decision)],
) -> Vec<RankDelta> {
    let confidence_of = |name: &str| {
        decisions
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0.0, |(_, d)| d.confidence)
    };
    let mut names: Vec<&String> = prior_order.iter().chain(model_order.iter()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| RankDelta {
            candidate: name.clone(),
            prior_rank: prior_order.iter().position(|n| n == name),
            model_rank: model_order.iter().position(|n| n == name),
            confidence: confidence_of(name),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::strategy::{FsmEngine, QueryClass};

    fn thresholds() -> ThresholdSet {
        ThresholdSet {
            act: 0.9,
            review: 0.7,
        }
    }

    fn choice(label: &str, top: f64) -> Decision {
        Decision::from_distribution(
            label,
            vec![(label.to_string(), top), ("other".to_string(), 1.0 - top)],
            "test".to_string(),
            1,
            7,
            false,
        )
        .expect("decision")
    }

    fn classify(query: &str) -> QueryClass {
        let mut engine = FsmEngine::new();
        let intake = engine.intake(query);
        engine.classify(&intake)
    }

    #[test]
    fn intent_shadow_leaves_authoritative_verdict_untouched() {
        // Byte-identical: the rule verdict with shadow on equals the verdict
        // with shadow off (field-by-field; the rule owns every field).
        let plain = classify("how do I parse JSON");
        let (shadowed, record) =
            FsmEngine::new().classify_with_shadow(&plain, Some(&choice("HowTo", 0.97)));
        assert_eq!(shadowed.intent, plain.intent);
        assert_eq!(shadowed.domain, plain.domain);
        assert_eq!(shadowed.confidence.to_bits(), plain.confidence.to_bits());
        assert_eq!(shadowed.tokens, plain.tokens);
        assert_eq!(shadowed.detected_file_paths, plain.detected_file_paths);
        assert_eq!(shadowed.detected_identifiers, plain.detected_identifiers);
        let record = record.expect("logged");
        assert_eq!(record.point, "intent");
        assert_eq!(record.label, "HowTo");
        assert!(record.would_have_acted);
        assert_eq!(record.verdict, "act");
    }

    #[test]
    fn intent_shadow_absent_without_decision() {
        let plain = classify("helloWorld");
        let (shadowed, record) = FsmEngine::new().classify_with_shadow(&plain, None);
        assert!(record.is_none());
        assert_eq!(shadowed.intent, plain.intent);
        assert_eq!(shadowed.domain, plain.domain);
        assert_eq!(shadowed.confidence.to_bits(), plain.confidence.to_bits());
        assert_eq!(shadowed.tokens, plain.tokens);
    }

    #[test]
    fn sufficiency_shadow_logs_would_have_filtered() {
        let record =
            sufficiency_shadow(4, Some(&choice("No", 0.4)), &thresholds()).expect("logged");
        assert_eq!(record.point, "sufficiency");
        assert_eq!(record.authoritative, "evidence:4");
        // 0.4 confidence escalates: the shadow would have filtered.
        assert_eq!(record.verdict, "escalate");
        assert!(!record.would_have_acted);
    }

    #[test]
    fn rerank_shadow_reports_rank_delta() {
        let prior = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let model = vec!["c".to_string(), "a".to_string()];
        let decisions = vec![
            ("a".to_string(), choice("Yes", 0.95)),
            ("c".to_string(), choice("Yes", 0.8)),
        ];
        let deltas = rerank_shadow(&prior, &model, &decisions);
        assert_eq!(deltas.len(), 3);
        let c = deltas.iter().find(|d| d.candidate == "c").expect("c");
        assert_eq!((c.prior_rank, c.model_rank), (Some(2), Some(0)));
        let b = deltas.iter().find(|d| d.candidate == "b").expect("b");
        assert_eq!((b.prior_rank, b.model_rank), (Some(1), None));
    }
}
/// Summary-route shadow: agreement between the deterministic candidates and
/// the summary route, without fusing. `would_have_acted` names the
/// task-value question (would the route add anything?) — never a model
/// verdict. Recorded through the recall loop; the deterministic result
/// stays authoritative.
#[must_use]
/// Summary-route shadow: agreement between the deterministic candidates and
/// the summary route, without fusing. `would_have_acted` names the
/// task-value question (would the route add anything?) — never a model
/// verdict. Recorded through the recall loop; the deterministic result
/// stays authoritative.
pub fn summary_route_shadow(
    authoritative: &std::collections::HashSet<&str>,
    summary: &[&str],
) -> ShadowRecord {
    let mut would_add: Vec<&str> = summary
        .iter()
        .filter(|id| !authoritative.contains(**id))
        .copied()
        .collect();
    would_add.sort_unstable();
    would_add.dedup();
    let overlap = summary
        .iter()
        .filter(|id| authoritative.contains(**id))
        .count();
    ShadowRecord {
        point: "summary_index".to_string(),
        label: if would_add.is_empty() {
            "agreement".to_string()
        } else {
            "divergence".to_string()
        },
        confidence: if summary.is_empty() {
            1.0
        } else {
            overlap as f64 / summary.len() as f64
        },
        margin: 0.0,
        latency_ms: 0,
        cached: false,
        verdict: "review".to_string(),
        would_have_acted: !would_add.is_empty(),
        authoritative: format!("{} deterministic candidates", authoritative.len()),
    }
}