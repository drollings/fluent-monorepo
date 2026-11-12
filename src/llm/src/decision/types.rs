//! Decision types + pure policy (no I/O).
//!
//! Producer confidence (`confidence` = top-1 probability, `margin` = top-1
//! minus top-2) measures the model's self-doubt, never task-value
//! correctness. Thresholds are per decision point and calibrated before
//! they gate or cache anything.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Which bounded shape a decision takes.
///
/// ```rust
/// use fluent_llm::decision::DecisionKind;
///
/// assert_eq!(DecisionKind::Choice.as_str(), "choice");
/// assert_eq!(
///     DecisionKind::parse("noul"),
///     Ok(DecisionKind::Noul)
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Which bounded shape a decision takes.
/// 
/// ```rust
/// use fluent_llm::decision::DecisionKind;
/// 
/// assert_eq!(DecisionKind::Choice.as_str(), "choice");
/// assert_eq!(
/// DecisionKind::parse("noul"),
/// Ok(DecisionKind::Noul)
/// );
/// ```
pub enum DecisionKind {
    Choice,
    Score,
    Noul,
}

impl DecisionKind {
    /// Canonical spelling for config and field access.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Choice => "choice",
            Self::Score => "score",
            Self::Noul => "noul",
        }
    }

    /// Parse a canonical spelling; garbage names all three spellings.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_lowercase().as_str() {
            "choice" => Ok(Self::Choice),
            "score" => Ok(Self::Score),
            "noul" => Ok(Self::Noul),
            _ => Err(format!(
                "unknown decision kind: {raw:?} (expected choice|score|noul)"
            )),
        }
    }
}

/// One bounded judgment with its full distribution.
///
/// ```rust
/// use fluent_llm::decision::Decision;
///
/// let d = Decision::from_distribution(
///     "B",
///     vec![("B".to_string(), 0.8), ("A".to_string(), 0.2)],
///     "model".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("distribution");
/// assert!((d.confidence - 0.8).abs() < 1e-9);
/// assert!((d.margin - 0.6).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One bounded judgment with its full distribution.
/// 
/// ```rust
/// use fluent_llm::decision::Decision;
/// 
/// let d = Decision::from_distribution(
/// "B",
/// vec![("B".to_string(), 0.8), ("A".to_string(), 0.2)],
/// "model".to_string(),
/// 1,
/// 0,
/// false,
/// )
/// .expect("distribution");
/// assert!((d.confidence - 0.8).abs() < 1e-9);
/// assert!((d.margin - 0.6).abs() < 1e-9);
/// ```
pub struct Decision {
    pub label: String,
    pub distribution: Vec<(String, f64)>,
    /// Producer confidence: top-1 probability (self-doubt, not correctness).
    pub confidence: f64,
    /// Top-1 minus top-2 probability gap.
    pub margin: f64,
    /// True when the distribution is an uncalibrated text-fallback prior
    /// rather than a measured logprobs majority: capped below `Act` and
    /// never cached as a calibrated result.
    #[serde(default)]
    pub degraded: bool,
    pub model: String,
    pub question_version: u64,
    pub latency_ms: u64,
    pub cached: bool,
}

impl Decision {
    /// Build from a label plus its probability distribution.
    ///
    /// `distribution` need not be sorted; probabilities should sum to ~1.0.
    /// Fails with `Decode` when the label is absent or the distribution is empty.
    pub fn from_distribution(
        label: &str,
        distribution: Vec<(String, f64)>,
        model: String,
        question_version: u64,
        latency_ms: u64,
        cached: bool,
    ) -> Result<Self, DecisionError> {
        if distribution.is_empty() {
            return Err(DecisionError::Decode("empty distribution".to_string()));
        }
        if !distribution.iter().any(|(l, _)| l == label) {
            return Err(DecisionError::Decode(format!(
                "label {label:?} not in distribution"
            )));
        }
        let mut sorted = distribution.clone();
        sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
        let top = sorted[0].1;
        let second = sorted.get(1).map_or(0.0, |(_, p)| *p);
        Ok(Self {
            label: label.to_string(),
            distribution,
            confidence: top,
            margin: (top - second).max(0.0),
            degraded: false,
            model,
            question_version,
            latency_ms,
            cached,
        })
    }
}

/// How a decision attempt failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// How a decision attempt failed.
pub enum DecisionError {
    /// Transport failure (timeout, refused, 5xx): task-value transient —
    /// the only variant the ladder may retry past.
    #[error("decision unavailable: {0}")]
    Unavailable(String),
    /// Permanent rejection (4xx, bad request): never retried, never cached.
    #[error("decision rejected: {0}")]
    Rejected(String),
    /// The response could not be decoded into a label.
    #[error("decision decode failed: {0}")]
    Decode(String),
}

impl DecisionError {
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl From<crate::protocol::LlmError> for DecisionError {
    fn from(e: crate::protocol::LlmError) -> Self {
        if e.is_retryable() {
            Self::Unavailable(e.to_string())
        } else {
            Self::Rejected(e.to_string())
        }
    }
}

/// Per-decision-point thresholds mapping confidence to a verdict.
///
/// `act`/`review` are producer-confidence gates; they earn task-value trust
/// only through calibration on labeled cases plus a must-not-fire control
/// group — never by assertion.
///
/// ```rust
/// use fluent_llm::decision::ThresholdSet;
///
/// assert!(ThresholdSet { act: 0.9, review: 0.7 }.validate().is_ok());
/// assert!(ThresholdSet { act: 0.5, review: 0.8 }.validate().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
/// Per-decision-point thresholds mapping confidence to a verdict.
/// 
/// `act`/`review` are producer-confidence gates; they earn task-value trust
/// only through calibration on labeled cases plus a must-not-fire control
/// group — never by assertion.
/// 
/// ```rust
/// use fluent_llm::decision::ThresholdSet;
/// 
/// assert!(ThresholdSet { act: 0.9, review: 0.7 }.validate().is_ok());
/// assert!(ThresholdSet { act: 0.5, review: 0.8 }.validate().is_err());
/// ```
pub struct ThresholdSet {
    pub act: f64,
    pub review: f64,
}

impl ThresholdSet {
    /// Check bounds (`[0,1]`) and ordering (`act >= review`).
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [("act", self.act), ("review", self.review)] {
            if !v.is_finite() || v < 0.0 || v > 1.0 {
                return Err(format!("threshold {name} out of range [0,1]: {v}"));
            }
        }
        if self.act < self.review {
            return Err(format!(
                "act ({}) must be >= review ({})",
                self.act, self.review
            ));
        }
        Ok(())
    }
}

/// Task-value verdict for one decision at its own thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Task-value verdict for one decision at its own thresholds.
pub enum Verdict {
    Act,
    Review,
    Escalate,
}

/// Map a decision plus its own thresholds to a verdict (pure).
///
/// ```rust
/// use fluent_llm::decision::{decide_verdict, Decision, ThresholdSet, Verdict};
///
/// let d = Decision::from_distribution(
///     "B",
///     vec![("B".to_string(), 0.95), ("A".to_string(), 0.05)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("decision");
/// let t = ThresholdSet { act: 0.9, review: 0.7 };
/// assert_eq!(decide_verdict(&d, &t), Verdict::Act);
/// ```
///
/// A degraded decision (uncalibrated text-fallback prior) is capped below
/// `Act`: it can ask for review, never act on invented confidence.
#[must_use]
/// Map a decision plus its own thresholds to a verdict (pure).
/// 
/// ```rust
/// use fluent_llm::decision::{decide_verdict, Decision, ThresholdSet, Verdict};
/// 
/// let d = Decision::from_distribution(
/// "B",
/// vec![("B".to_string(), 0.95), ("A".to_string(), 0.05)],
/// "m".to_string(),
/// 1,
/// 0,
/// false,
/// )
/// .expect("decision");
/// let t = ThresholdSet { act: 0.9, review: 0.7 };
/// assert_eq!(decide_verdict(&d, &t), Verdict::Act);
/// ```
/// 
/// A degraded decision (uncalibrated text-fallback prior) is capped below
/// `Act`: it can ask for review, never act on invented confidence.
pub fn decide_verdict(decision: &Decision, thresholds: &ThresholdSet) -> Verdict {
    let verdict = if decision.confidence >= thresholds.act {
        Verdict::Act
    } else if decision.confidence >= thresholds.review {
        Verdict::Review
    } else {
        Verdict::Escalate
    };
    if decision.degraded && verdict == Verdict::Act {
        Verdict::Review
    } else {
        verdict
    }
}

/// P(yes) mass of a Noul decision: the normalized weight on the `Yes` label.
/// `None` when the distribution names no `Yes` label.
///
/// ```rust
/// use fluent_llm::decision::{noul_p_yes, Decision};
///
/// let d = Decision::from_distribution(
///     "No",
///     vec![("Yes".to_string(), 0.05), ("No".to_string(), 0.95)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("decision");
/// assert!((noul_p_yes(&d).expect("mass") - 0.05).abs() < 1e-9);
/// ```
#[must_use]
/// P(yes) mass of a Noul decision: the normalized weight on the `Yes` label.
/// `None` when the distribution names no `Yes` label.
/// 
/// ```rust
/// use fluent_llm::decision::{noul_p_yes, Decision};
/// 
/// let d = Decision::from_distribution(
/// "No",
/// vec![("Yes".to_string(), 0.05), ("No".to_string(), 0.95)],
/// "m".to_string(),
/// 1,
/// 0,
/// false,
/// )
/// .expect("decision");
/// assert!((noul_p_yes(&d).expect("mass") - 0.05).abs() < 1e-9);
/// ```
pub fn noul_p_yes(decision: &Decision) -> Option<f64> {
    let total: f64 = decision.distribution.iter().map(|(_, p)| p).sum();
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    decision
        .distribution
        .iter()
        .find(|(label, _)| label == "Yes")
        .map(|(_, p)| (p / total).clamp(0.0, 1.0))
}

/// Ordinal moment of a Score distribution in rung-index units (`ScoreMoment`).
///
/// ```rust
/// use fluent_llm::decision::{score_moment, Decision};
///
/// let d = Decision::from_distribution(
///     "4",
///     vec![("3".to_string(), 0.2), ("4".to_string(), 0.8)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("decision");
/// let moment = score_moment(&d).expect("moment");
/// assert!((moment.expected - 0.8).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
/// Ordinal moment of a Score distribution in rung-index units (`ScoreMoment`).
/// 
/// ```rust
/// use fluent_llm::decision::{score_moment, Decision};
/// 
/// let d = Decision::from_distribution(
/// "4",
/// vec![("3".to_string(), 0.2), ("4".to_string(), 0.8)],
/// "m".to_string(),
/// 1,
/// 0,
/// false,
/// )
/// .expect("decision");
/// let moment = score_moment(&d).expect("moment");
/// assert!((moment.expected - 0.8).abs() < 1e-9);
/// ```
pub struct ScoreMoment {
    /// Expected rung index (0-based over distribution order).
    pub expected: f64,
    /// Standard deviation around it; wide spreads deserve review.
    pub std: f64,
}

/// Expected value and spread over distribution order. `None` on an empty
/// or non-positive distribution. Adjacent-rung errors move the mean
/// slightly; distant ones move it far — the ordinal weighting.
#[must_use]
/// Expected value and spread over distribution order. `None` on an empty
/// or non-positive distribution. Adjacent-rung errors move the mean
/// slightly; distant ones move it far — the ordinal weighting.
pub fn score_moment(decision: &Decision) -> Option<ScoreMoment> {
    if decision.distribution.is_empty() {
        return None;
    }
    let total: f64 = decision.distribution.iter().map(|(_, p)| p).sum();
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let expected: f64 = decision
        .distribution
        .iter()
        .enumerate()
        .map(|(i, (_, p))| i as f64 * p / total)
        .sum();
    if !expected.is_finite() {
        return None;
    }
    let variance: f64 = decision
        .distribution
        .iter()
        .enumerate()
        .map(|(i, (_, p))| {
            let gap = i as f64 - expected;
            gap * gap * p / total
        })
        .sum();
    Some(ScoreMoment {
        expected,
        std: variance.max(0.0).sqrt(),
    })
}

/// Directional Score verdict on the expected rung: act on a confident high
/// (`expected >= act_above`) or confident low (`expected <= act_below`),
/// review the middle. Degraded decisions cap below `Act`, as in
/// [`decide_verdict`]; invalid bounds or a moment-less decision escalate.
///
/// ```rust
/// use fluent_llm::decision::{decide_score_verdict, Decision, Verdict};
///
/// let d = Decision::from_distribution(
///     "5",
///     vec![("4".to_string(), 0.1), ("5".to_string(), 0.9)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("decision");
/// assert_eq!(decide_score_verdict(&d, 0.5, -1.0), Verdict::Act);
/// ```
#[must_use]
/// Directional Score verdict on the expected rung: act on a confident high
/// (`expected >= act_above`) or confident low (`expected <= act_below`),
/// review the middle. Degraded decisions cap below `Act`, as in
/// [`decide_verdict`]; invalid bounds or a moment-less decision escalate.
/// 
/// ```rust
/// use fluent_llm::decision::{decide_score_verdict, Decision, Verdict};
/// 
/// let d = Decision::from_distribution(
/// "5",
/// vec![("4".to_string(), 0.1), ("5".to_string(), 0.9)],
/// "m".to_string(),
/// 1,
/// 0,
/// false,
/// )
/// .expect("decision");
/// assert_eq!(decide_score_verdict(&d, 0.5, -1.0), Verdict::Act);
/// ```
pub fn decide_score_verdict(decision: &Decision, act_above: f64, act_below: f64) -> Verdict {
    let valid = act_above.is_finite() && act_below.is_finite() && act_above > act_below;
    if !valid {
        return Verdict::Escalate;
    }
    let Some(moment) = score_moment(decision) else {
        return Verdict::Escalate;
    };
    let verdict = if moment.expected >= act_above || moment.expected <= act_below {
        Verdict::Act
    } else {
        Verdict::Review
    };
    if decision.degraded && verdict == Verdict::Act {
        Verdict::Review
    } else {
        verdict
    }
}
/// Directional Noul verdict from P(yes) against separate accept/reject
/// thresholds: act on a confident yes (`p >= accept`) or a confident no
/// (`p <= reject`), review the middle. Top-1 confidence alone cannot do
/// this — a 0.95-No and a 0.95-Yes look identical to it.
///
/// ```rust
/// use fluent_llm::decision::{decide_noul_verdict, Decision, Verdict};
///
/// let d = Decision::from_distribution(
///     "No",
///     vec![("Yes".to_string(), 0.05), ("No".to_string(), 0.95)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("decision");
/// assert_eq!(decide_noul_verdict(&d, 0.9, 0.1), Verdict::Act);
/// ```
///
/// Boundaries fire (`>= accept`, `<= reject`), matching the Choice policy.
/// A degraded decision is capped below `Act`, as in [`decide_verdict`].
/// A decision with no `Yes` mass cannot be directed and escalates.
#[must_use]
/// Directional Noul verdict from P(yes) against separate accept/reject
/// thresholds: act on a confident yes (`p >= accept`) or a confident no
/// (`p <= reject`), review the middle. Top-1 confidence alone cannot do
/// this — a 0.95-No and a 0.95-Yes look identical to it.
/// 
/// ```rust
/// use fluent_llm::decision::{decide_noul_verdict, Decision, Verdict};
/// 
/// let d = Decision::from_distribution(
/// "No",
/// vec![("Yes".to_string(), 0.05), ("No".to_string(), 0.95)],
/// "m".to_string(),
/// 1,
/// 0,
/// false,
/// )
/// .expect("decision");
/// assert_eq!(decide_noul_verdict(&d, 0.9, 0.1), Verdict::Act);
/// ```
/// 
/// Boundaries fire (`>= accept`, `<= reject`), matching the Choice policy.
/// A degraded decision is capped below `Act`, as in [`decide_verdict`].
/// A decision with no `Yes` mass cannot be directed and escalates.
pub fn decide_noul_verdict(decision: &Decision, accept: f64, reject: f64) -> Verdict {
    let valid = [accept, reject]
        .iter()
        .all(|t| t.is_finite() && (0.0..=1.0).contains(t))
        && accept > reject;
    if !valid {
        return Verdict::Escalate;
    }
    let Some(p) = noul_p_yes(decision) else {
        return Verdict::Escalate;
    };
    let verdict = if p >= accept || p <= reject {
        Verdict::Act
    } else {
        Verdict::Review
    };
    if decision.degraded && verdict == Verdict::Act {
        Verdict::Review
    } else {
        verdict
    }
}