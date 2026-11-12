//! Fixed canary prompts with expected distributions.
//!
//! An alias pointing at new weights is detected here, not in production
//! traffic: run the suite, verify each decision against its expectation
//! within tolerance, and fingerprint the verified set. Drift (or a degraded
//! fallback standing in for a measurement) is a typed `CanaryDrift`, never
//! a silent fallback. The fingerprint scopes registry lookups and cache
//! keys to the weights actually probed.

use common_core::golden::compare_series;

use crate::decision::Decision;

/// A `CanaryCase`: full prompt text, option labels, and the expected
/// per-label masses in label order with an absolute tolerance.
#[derive(Debug, Clone, PartialEq)]
/// A `CanaryCase`: full prompt text, option labels, and the expected
/// per-label masses in label order with an absolute tolerance.
pub struct CanaryCase {
    pub id: &'static str,
    pub prompt: String,
    pub options: Vec<String>,
    pub expected: Vec<(String, f64)>,
    pub tolerance: f64,
}

/// The fixed two-case suite. Prompts force single tokens the logprobs path
/// discriminates; expectations are tight majorities, not model output.
///
/// ```rust
/// use fluent_llm::decision::canary_suite;
///
/// let suite = canary_suite();
/// assert_eq!(suite.len(), 2);
/// assert!(suite.iter().all(|c| c.options.len() >= 2));
/// ```
#[must_use]
/// The fixed two-case suite. Prompts force single tokens the logprobs path
/// discriminates; expectations are tight majorities, not model output.
/// 
/// ```rust
/// use fluent_llm::decision::canary_suite;
/// 
/// let suite = canary_suite();
/// assert_eq!(suite.len(), 2);
/// assert!(suite.iter().all(|c| c.options.len() >= 2));
/// ```
pub fn canary_suite() -> Vec<CanaryCase> {
    vec![
        CanaryCase {
            id: "forced-a",
            prompt: "Reply with only the letter A. No other text.".to_string(),
            options: vec!["A".to_string(), "B".to_string()],
            expected: vec![("A".to_string(), 0.98), ("B".to_string(), 0.02)],
            tolerance: 0.05,
        },
        CanaryCase {
            id: "forced-yes",
            prompt: "Reply with only Yes. No other text.".to_string(),
            options: vec!["Yes".to_string(), "No".to_string()],
            expected: vec![("Yes".to_string(), 0.97), ("No".to_string(), 0.03)],
            tolerance: 0.05,
        },
    ]
}

/// Typed drift condition (`CanaryDrift`): the weights moved (or never measured).
#[derive(Debug, Clone, PartialEq)]
/// Typed drift condition (`CanaryDrift`): the weights moved (or never measured).
pub struct CanaryDrift {
    pub case_id: String,
    pub detail: String,
}

/// Verify one measured decision against its case. Degraded fallbacks are
/// not measurements and always drift.
///
/// ```rust
/// use fluent_llm::decision::{canary_suite, verify_canary, Decision};
///
/// let suite = canary_suite();
/// let d = Decision::from_distribution(
///     "A",
///     vec![("A".to_string(), 0.97), ("B".to_string(), 0.03)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("decision");
/// assert!(verify_canary(&d, &suite[0]).is_ok());
/// ```
pub fn verify_canary(decision: &Decision, case: &CanaryCase) -> Result<(), CanaryDrift> {
    let refused = |detail: String| {
        Err(CanaryDrift {
            case_id: case.id.to_string(),
            detail,
        })
    };
    if decision.degraded {
        return refused("degraded fallback is not a measurement".to_string());
    }
    let mut actual: Vec<Option<f64>> = Vec::with_capacity(case.expected.len());
    for (label, _) in &case.expected {
        let mass = decision
            .distribution
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, p)| *p);
        actual.push(mass);
    }
    if actual.iter().any(Option::is_none) {
        return refused("expected label missing from distribution".to_string());
    }
    let want: Vec<Option<f64>> = case.expected.iter().map(|(_, p)| Some(*p)).collect();
    let report = compare_series(&actual, &want, case.tolerance);
    if report.matches {
        Ok(())
    } else {
        refused(format!(
            "max drift {:.4} over tolerance",
            report.max_abs_diff
        ))
    }
}

/// Fingerprint a verified canary set: blake3 over framed (label, mass to
/// two decimals) pairs. Same response bytes fingerprint identically;
/// weight changes move masses and the fingerprint follows.
///
/// ```rust
/// use fluent_llm::decision::{canary_signature, Decision};
///
/// let mk = |mass: f64| {
///     Decision::from_distribution(
///         "A",
///         vec![("A".to_string(), mass), ("B".to_string(), 1.0 - mass)],
///         "m".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("decision")
/// };
/// assert_eq!(canary_signature(&[mk(0.97)]), canary_signature(&[mk(0.97)]));
/// assert_ne!(canary_signature(&[mk(0.97)]), canary_signature(&[mk(0.60)]));
/// ```
#[must_use]
/// Fingerprint a verified canary set: blake3 over framed (label, mass to
/// two decimals) pairs. Same response bytes fingerprint identically;
/// weight changes move masses and the fingerprint follows.
/// 
/// ```rust
/// use fluent_llm::decision::{canary_signature, Decision};
/// 
/// let mk = |mass: f64| {
/// Decision::from_distribution(
/// "A",
/// vec![("A".to_string(), mass), ("B".to_string(), 1.0 - mass)],
/// "m".to_string(),
/// 1,
/// 0,
/// false,
/// )
/// .expect("decision")
/// };
/// assert_eq!(canary_signature(&[mk(0.97)]), canary_signature(&[mk(0.97)]));
/// assert_ne!(canary_signature(&[mk(0.97)]), canary_signature(&[mk(0.60)]));
/// ```
pub fn canary_signature(decisions: &[Decision]) -> String {
    let mut bytes = Vec::new();
    for decision in decisions {
        for (label, mass) in &decision.distribution {
            let quantized = (mass * 100.0).round() as i64;
            bytes.extend_from_slice(&(label.len() as u64).to_le_bytes());
            bytes.extend_from_slice(label.as_bytes());
            bytes.extend_from_slice(&quantized.to_le_bytes());
        }
    }
    common_core::hash::blake3_hex(&bytes)
}