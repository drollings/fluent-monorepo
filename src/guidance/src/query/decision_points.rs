//! Decision points for the guidance query pipeline.
//!
//! Each point pairs a pinned `QuestionSpec` with the shared ladder and
//! authorizes through the calibration registry. Nothing here acts:
//! the seed-empty registry authorizes shadow-only, caches stay cold,
//! and the deterministic result remains authoritative.

use fluent_llm::decision::{
    authorize, authorize_advisory, authorize_pinned, decide_with_ladder, CacheKeyInput,
    CalibrationRegistry, CalibrationStatus, Decision, DecisionBreaker, DecisionCache,
    DecisionError, QuestionSpec, ThresholdSet,
};

use super::shadow::{sufficiency_shadow, ShadowRecord};

pub const INTENT_POINT: &str = "intent";
pub const SUFFICIENCY_POINT: &str = "sufficiency";
pub const EXCERPT_POINT: &str = "excerpt";
pub const STALENESS_POINT: &str = "staleness";
pub const SUMMARY_INDEX_POINT: &str = "summary_index";

pub const INTENT_VERSION: u64 = 1;
pub const SUFFICIENCY_VERSION: u64 = 1;
pub const EXCERPT_VERSION: u64 = 1;
pub const STALENESS_VERSION: u64 = 1;
pub const SUMMARY_INDEX_VERSION: u64 = 1;

fn spec(
    id: &str,
    version: u64,
    kind: &str,
    prompt: &str,
    options_csv: &str,
    act: f64,
    review: f64,
    timeout_ms: u64,
) -> QuestionSpec {
    // Caches enable only with a passing calibration entry; until then
    // every observation is shadow, never stored truth.
    QuestionSpec::new()
        .id(id.to_string())
        .version(version)
        .kind(kind.to_string())
        .prompt_template(prompt.to_string())
        .options_csv(options_csv.to_string())
        .act_threshold(act)
        .review_threshold(review)
        .timeout_ms(timeout_ms)
        .cacheable(false)
        .build()
}

/// Intent Choice over the `QueryIntent` labels (single tokens only).
#[must_use]
/// Intent Choice over the `QueryIntent` labels (single tokens only).
pub fn intent_spec() -> QuestionSpec {
    spec(
        INTENT_POINT,
        INTENT_VERSION,
        "choice",
        "Classify the query by what the asker wants.",
        "IdentifierLookup,SingleIdentifier,CapabilityQuery,FilePath,HowTo,Conceptual,MultiKeyword,GeneralSearch",
        0.9,
        0.7,
        2000,
    )
}

/// Sufficiency route: answer, synthesize, or stay silent.
#[must_use]
/// Sufficiency route: answer, synthesize, or stay silent.
pub fn sufficiency_spec() -> QuestionSpec {
    spec(
        SUFFICIENCY_POINT,
        SUFFICIENCY_VERSION,
        "choice",
        "Given the excerpts, route: answer directly, synthesize, or stay silent.",
        "Answer,Synthesize,Unanswerable",
        0.9,
        0.7,
        5000,
    )
}

/// Excerpt relevance as ordinal score rungs (rank order, not magnitude).
#[must_use]
/// Excerpt relevance as ordinal score rungs (rank order, not magnitude).
pub fn excerpt_spec() -> QuestionSpec {
    spec(
        EXCERPT_POINT,
        EXCERPT_VERSION,
        "score",
        "Rate how relevant the excerpt is to the query.",
        "reject,weak,ok,strong",
        0.9,
        0.7,
        5000,
    )
}

/// Commentary staleness: does the diff alter the described behavior.
#[must_use]
/// Commentary staleness: does the diff alter the described behavior.
pub fn staleness_spec() -> QuestionSpec {
    spec(
        STALENESS_POINT,
        STALENESS_VERSION,
        "noul",
        "Does this diff alter the behavior the comment describes?",
        "Yes,No",
        0.9,
        0.7,
        5000,
    )
}

/// Summary index route: may generated-comment recall contribute
/// candidates. Authorization only, never a model verdict: the route reads
/// a derived table and cites source ranges.
#[must_use]
/// Summary index route: may generated-comment recall contribute
/// candidates. Authorization only, never a model verdict: the route reads
/// a derived table and cites source ranges.
pub fn summary_index_spec() -> QuestionSpec {
    spec(
        SUMMARY_INDEX_POINT,
        SUMMARY_INDEX_VERSION,
        "choice",
        "May the summary index contribute recall candidates?",
        "Act,Shadow",
        0.9,
        0.7,
        5000,
    )
}

/// Fingerprint binding a summary-index authorization to the model and
/// prompt that produced the measured summaries.
#[must_use]
/// Fingerprint binding a summary-index authorization to the model and
/// prompt that produced the measured summaries.
pub fn summary_index_fingerprint(model: &str) -> String {
    format!("{model}@prompt-v{}", crate::summary::SUMMARY_PROMPT_VERSION)
}

/// Fail-closed authorization: a passing entry on all three keys
/// calibrates, anything else (including the seed-empty registry) is
/// shadow-only with the reason named.
#[must_use]
/// Fail-closed authorization: a passing entry on all three keys
/// calibrates, anything else (including the seed-empty registry) is
/// shadow-only with the reason named.
pub fn authorize_point(point: &str, fingerprint: &str, version: u64) -> CalibrationStatus {
    authorize_point_with(&CalibrationRegistry::empty(), point, fingerprint, version)
}

/// Registry-taking companion: the loop loads `state/calibration.json`
/// once and consults it here; the empty-registry call above is the
/// cold-start form with identical fail-closed semantics.
#[must_use]
/// Registry-taking companion: the loop loads `state/calibration.json`
/// once and consults it here; the empty-registry call above is the
/// cold-start form with identical fail-closed semantics.
pub fn authorize_point_with(
    registry: &CalibrationRegistry,
    point: &str,
    fingerprint: &str,
    version: u64,
) -> CalibrationStatus {
    authorize(registry, point, fingerprint, version)
}

/// Advisory authorization: ordering and priority only, never side
/// effects. Same evidence bar as `authorize_point`, narrower grant.
#[must_use]
/// Advisory authorization: ordering and priority only, never side
/// effects. Same evidence bar as `authorize_point`, narrower grant.
pub fn authorize_point_advisory(point: &str, fingerprint: &str, version: u64) -> CalibrationStatus {
    authorize_point_advisory_with(&CalibrationRegistry::empty(), point, fingerprint, version)
}

/// Registry-taking companion to `authorize_point_advisory`.
#[must_use]
/// Registry-taking companion to `authorize_point_advisory`.
pub fn authorize_point_advisory_with(
    registry: &CalibrationRegistry,
    point: &str,
    fingerprint: &str,
    version: u64,
) -> CalibrationStatus {
    authorize_advisory(registry, point, fingerprint, version)
}

/// Dataset-pinned authorization: the entry must record the expected
/// dataset hash or the point stays measuring.
#[must_use]
/// Dataset-pinned authorization: the entry must record the expected
/// dataset hash or the point stays measuring.
pub fn authorize_point_pinned(
    point: &str,
    fingerprint: &str,
    version: u64,
    dataset: &str,
) -> CalibrationStatus {
    authorize_point_pinned_with(
        &CalibrationRegistry::empty(),
        point,
        fingerprint,
        version,
        dataset,
    )
}

/// Registry-taking companion to `authorize_point_pinned`.
#[must_use]
/// Registry-taking companion to `authorize_point_pinned`.
pub fn authorize_point_pinned_with(
    registry: &CalibrationRegistry,
    point: &str,
    fingerprint: &str,
    version: u64,
    dataset: &str,
) -> CalibrationStatus {
    authorize_pinned(registry, point, fingerprint, version, dataset)
}

/// Fingerprint-scoped cache key for one decision evaluation.
#[must_use]
#[allow(clippy::too_many_arguments)]
/// Fingerprint-scoped cache key for one decision evaluation.
pub fn decision_key(
    point: &str,
    state: &str,
    question: &str,
    spec: &QuestionSpec,
    model: &str,
    endpoint: &str,
    fingerprint: Option<String>,
) -> String {
    CacheKeyInput {
        state: state.to_string(),
        question: question.to_string(),
        question_id: point.to_string(),
        version: spec.version,
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        extras: serde_json::Value::Null,
        think: None,
        fingerprint,
    }
    .key()
}

/// Thresholds a shadow record is logged against for one spec.
#[must_use]
/// Thresholds a shadow record is logged against for one spec.
pub fn shadow_thresholds(spec: &QuestionSpec) -> ThresholdSet {
    spec.thresholds()
}

/// Map an intent-spec label back to its `QueryIntent`, if it names one.
#[must_use]
/// Map an intent-spec label back to its `QueryIntent`, if it names one.
pub fn intent_label_to_intent(label: &str) -> Option<super::strategy::QueryIntent> {
    use super::strategy::QueryIntent;
    match label {
        "IdentifierLookup" => Some(QueryIntent::IdentifierLookup),
        "SingleIdentifier" => Some(QueryIntent::SingleIdentifier),
        "CapabilityQuery" => Some(QueryIntent::CapabilityQuery),
        "FilePath" => Some(QueryIntent::FilePath),
        "HowTo" => Some(QueryIntent::HowTo),
        "Conceptual" => Some(QueryIntent::Conceptual),
        "MultiKeyword" => Some(QueryIntent::MultiKeyword),
        "GeneralSearch" => Some(QueryIntent::GeneralSearch),
        _ => None,
    }
}

/// Sufficiency route for one evidence set. The hard gate runs first:
/// ungrounded evidence routes `Unanswerable` no matter what the model
/// says. Grounded evidence without a verdict answers from excerpts
/// (today's behavior); a verdict routes three ways, and an unknown
/// label can never silence grounded evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Sufficiency route for one evidence set. The hard gate runs first:
/// ungrounded evidence routes `Unanswerable` no matter what the model
/// says. Grounded evidence without a verdict answers from excerpts
/// (today's behavior); a verdict routes three ways, and an unknown
/// label can never silence grounded evidence.
pub enum Sufficiency {
    Answer,
    Synthesize,
    Unanswerable,
}

/// Route one evidence set, logging the shadow record when a verdict
/// was consulted. Pure: no I/O, no cache, no side effects.
#[must_use]
/// Route one evidence set, logging the shadow record when a verdict
/// was consulted. Pure: no I/O, no cache, no side effects.
pub fn route_sufficiency(
    evidence_count: usize,
    grounded: bool,
    decision: Option<&Decision>,
    thresholds: &ThresholdSet,
) -> (Sufficiency, Option<ShadowRecord>) {
    let record = sufficiency_shadow(evidence_count, decision, thresholds);
    if !grounded {
        return (Sufficiency::Unanswerable, record);
    }
    let Some(decision) = decision else {
        return (Sufficiency::Answer, record);
    };
    let route = match decision.label.as_str() {
        "Synthesize" => Sufficiency::Synthesize,
        "Unanswerable" => Sufficiency::Unanswerable,
        _ => Sufficiency::Answer,
    };
    (route, record)
}

/// Relevance gate over scored candidates: keep every candidate at or
/// above `threshold`, preserving input order. A filter, never a rerank —
/// the legacy 0.5 operating point passes through byte-identically.
#[must_use]
/// Relevance gate over scored candidates: keep every candidate at or
/// above `threshold`, preserving input order. A filter, never a rerank —
/// the legacy 0.5 operating point passes through byte-identically.
pub fn relevance_gate(candidates: &[(&str, f64)], threshold: f64) -> Vec<String> {
    candidates
        .iter()
        .filter(|(_, score)| *score >= threshold)
        .map(|(name, _)| (*name).to_string())
        .collect()
}

/// Whitespace-stripped signature: the cheap exact comparison behind
/// eligibility. Cosmetic-only diffs never reach a verdict.
#[must_use]
/// Whitespace-stripped signature: the cheap exact comparison behind
/// eligibility. Cosmetic-only diffs never reach a verdict.
pub fn normalize_signature(signature: &str) -> String {
    signature.split_whitespace().collect::<String>()
}

/// Whether a member's commentary is eligible for a staleness verdict.
/// Human-written comments are never nominated; whitespace-only diffs on
/// generated comments are not nominated; everything else generated with
/// changed bytes is model-eligible (shadow-only until authorized).
#[must_use]
/// Whether a member's commentary is eligible for a staleness verdict.
/// Human-written comments are never nominated; whitespace-only diffs on
/// generated comments are not nominated; everything else generated with
/// changed bytes is model-eligible (shadow-only until authorized).
pub fn staleness_eligible(comment_generated: bool, old_sig: &str, new_sig: &str) -> bool {
    comment_generated && normalize_signature(old_sig) != normalize_signature(new_sig)
}

/// Which rung answered one ladder evaluation. Efficiency accounting
/// only: the deterministic result stays authoritative regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which rung answered one ladder evaluation. Efficiency accounting
/// only: the deterministic result stays authoritative regardless.
pub enum LadderOutcome {
    /// Deterministic rule decided; no model call made.
    Rule,
    /// Fingerprint-scoped cache hit; no model call made.
    CacheHit,
    /// Model answered (shadow-only until authorized).
    Model,
    /// Static fallback answered after `Unavailable`.
    Fallback,
    /// Degraded or drifted verdict kept out of Act and the cache.
    Dropped,
}

/// Decision-layer counters over existing primitives: atomics plus the
/// shared `LatencyHistogram`. No second metrics system; snapshots
/// render as one additive object in health output.
pub struct DecisionMetrics {
    rule_wins: std::sync::atomic::AtomicU64,
    cache_hits: std::sync::atomic::AtomicU64,
    model_answers: std::sync::atomic::AtomicU64,
    fallbacks: std::sync::atomic::AtomicU64,
    drops: std::sync::atomic::AtomicU64,
    shadow_disagreements: std::sync::atomic::AtomicU64,
    /// Inputs flagged by the untrusted-input screen. The flag observes;
    /// routing decisions are recorded on their own rungs.
    suspicious: std::sync::atomic::AtomicU64,
    latency: std::sync::Arc<common_core::metrics::LatencyHistogram>,
}

impl DecisionMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rule_wins: std::sync::atomic::AtomicU64::new(0),
            cache_hits: std::sync::atomic::AtomicU64::new(0),
            model_answers: std::sync::atomic::AtomicU64::new(0),
            fallbacks: std::sync::atomic::AtomicU64::new(0),
            drops: std::sync::atomic::AtomicU64::new(0),
            shadow_disagreements: std::sync::atomic::AtomicU64::new(0),
            suspicious: std::sync::atomic::AtomicU64::new(0),
            latency: std::sync::Arc::new(common_core::metrics::LatencyHistogram::new()),
        }
    }

    /// Count one ladder evaluation by the rung that answered it.
    pub fn record(&self, outcome: LadderOutcome) {
        use std::sync::atomic::Ordering;
        match outcome {
            LadderOutcome::Rule => self.rule_wins.fetch_add(1, Ordering::Relaxed),
            LadderOutcome::CacheHit => self.cache_hits.fetch_add(1, Ordering::Relaxed),
            LadderOutcome::Model => self.model_answers.fetch_add(1, Ordering::Relaxed),
            LadderOutcome::Fallback => self.fallbacks.fetch_add(1, Ordering::Relaxed),
            LadderOutcome::Dropped => self.drops.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// Count one shadow observation; disagreement (authoritative verdict
    /// differs) is the calibration signal, never a promotion by itself.
    pub fn record_shadow(&self, agreed: bool) {
        if !agreed {
            use std::sync::atomic::Ordering;
            self.shadow_disagreements.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Count one input flagged by the untrusted-input screen. Observes
    /// only: the routing rung is recorded separately by the caller.
    pub fn record_suspicious(&self) {
        use std::sync::atomic::Ordering;
        self.suspicious.fetch_add(1, Ordering::Relaxed);
    }

    /// Observe one finished evaluation latency in milliseconds.
    pub fn observe_latency_ms(&self, duration_ms: u64) {
        self.latency.observe(duration_ms);
    }

    /// Point-in-time counters plus p50/p99 over the shared histogram.
    #[must_use]
    pub fn snapshot(&self) -> DecisionMetricsSnapshot {
        use std::sync::atomic::Ordering;
        DecisionMetricsSnapshot {
            rule_wins: self.rule_wins.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            model_answers: self.model_answers.load(Ordering::Relaxed),
            fallbacks: self.fallbacks.load(Ordering::Relaxed),
            drops: self.drops.load(Ordering::Relaxed),
            shadow_disagreements: self.shadow_disagreements.load(Ordering::Relaxed),
            suspicious: self.suspicious.load(Ordering::Relaxed),
            latency_p50_ms: self.latency.estimate_percentile(50.0),
            latency_p99_ms: self.latency.estimate_percentile(99.0),
            latency_samples: self.latency.count(),
        }
    }
}

impl Default for DecisionMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Run one ladder evaluation with metrics attached: exactly one
/// `record` plus one latency sample per call, shadow optional.
/// The rung is read off the ladder's own result (rule presence,
/// cache-hit marking, degraded flag, factory outcome) — never a
/// second walk, and the shared ladder stays untouched.
#[allow(clippy::too_many_arguments)]
/// Run one ladder evaluation with metrics attached: exactly one
/// `record` plus one latency sample per call, shadow optional.
/// The rung is read off the ladder's own result (rule presence,
/// cache-hit marking, degraded flag, factory outcome) — never a
/// second walk, and the shared ladder stays untouched.
pub async fn evaluate_with_metrics<MF, Fut>(
    metrics: &DecisionMetrics,
    rule: Option<Decision>,
    cache: &DecisionCache,
    key: String,
    cacheable: bool,
    breaker: &DecisionBreaker,
    model: MF,
    fallback: Decision,
    shadow_agreed: Option<bool>,
) -> Result<Decision, DecisionError>
where
    MF: FnOnce() -> Fut + Send,
    Fut: std::future::Future<Output = Result<Decision, DecisionError>> + Send,
{
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let start = std::time::Instant::now();
    let rule_won = rule.is_some();
    let model_ran = Arc::new(AtomicBool::new(false));
    let model_ok = Arc::new(AtomicBool::new(false));
    let ran_flag = Arc::clone(&model_ran);
    let ok_flag = Arc::clone(&model_ok);
    let outcome = decide_with_ladder(
        rule,
        cache,
        key,
        cacheable,
        breaker,
        move || {
            ran_flag.store(true, Ordering::Relaxed);
            let ok_flag = Arc::clone(&ok_flag);
            async move {
                let result = model().await;
                ok_flag.store(result.is_ok(), Ordering::Relaxed);
                result
            }
        },
        fallback,
    )
    .await;
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    metrics.observe_latency_ms(elapsed_ms);
    if let Some(agreed) = shadow_agreed {
        metrics.record_shadow(agreed);
    }
    match outcome {
        Ok(decision) => {
            // Rule presence decides first: the ladder always prefers it.
            // Otherwise the decision's own markings say which rung won.
            if rule_won {
                metrics.record(LadderOutcome::Rule);
            } else if decision.cached {
                metrics.record(LadderOutcome::CacheHit);
            } else if decision.degraded {
                metrics.record(LadderOutcome::Dropped);
            } else if model_ran.load(Ordering::Relaxed) && model_ok.load(Ordering::Relaxed) {
                metrics.record(LadderOutcome::Model);
            } else {
                metrics.record(LadderOutcome::Fallback);
            }
            Ok(decision)
        }
        // Terminal rejection: nothing answered, nothing cached.
        Err(error) => {
            metrics.record(LadderOutcome::Dropped);
            Err(error)
        }
    }
}

/// Serializable counters for health/benchmark output (additive key).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// Serializable counters for health/benchmark output (additive key).
pub struct DecisionMetricsSnapshot {
    pub rule_wins: u64,
    pub cache_hits: u64,
    pub model_answers: u64,
    pub fallbacks: u64,
    pub drops: u64,
    pub shadow_disagreements: u64,
    /// Flagged by the untrusted-input screen (defaulted: old journals
    /// predate the counter).
    #[serde(default)]
    pub suspicious: u64,
    pub latency_p50_ms: u64,
    pub latency_p99_ms: u64,
    pub latency_samples: u64,
}

#[cfg(test)]
#[path = "../../tests/decision_points.rs"]
mod decision_points_tests;

#[cfg(test)]
#[path = "../../tests/sufficiency_decision.rs"]
mod sufficiency_decision_tests;

#[cfg(test)]
#[path = "../../tests/excerpt_decision.rs"]
mod excerpt_decision_tests;

#[cfg(test)]
#[path = "../../tests/staleness_decision.rs"]
mod staleness_decision_tests;

#[cfg(test)]
#[path = "../../tests/decision_metrics.rs"]
mod decision_metrics_tests;

#[cfg(test)]
#[path = "../../tests/corpus_seeds.rs"]
mod corpus_seeds_tests;