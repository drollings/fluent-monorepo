//! Single-token logprobs decoder + text fallback.
//!
//! The endpoint silently ignores grammar/logit constraints, so decoding is
//! prompt-constrain + decode-validate: sum tokenizer variants (leading-space,
//! case) into canonical labels from `content` logprobs only.
//! `reasoning_content` is never an answer.

use crate::decision::types::{Decision, DecisionError};

/// Canonical token form: leading whitespace is a tokenizer artifact.
fn canonical(token: &str) -> &str {
    token.trim_start()
}

/// Map a raw token to an expected label: exact trimmed match first, then
/// case-insensitive. `None` when the token names no candidate.
fn match_label<'a>(token: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let t = canonical(token);
    if let Some(hit) = candidates.iter().copied().find(|c| *c == t) {
        return Some(hit);
    }
    candidates
        .iter()
        .copied()
        .find(|c| c.eq_ignore_ascii_case(t))
}

/// One top-logprob entry: `(token, probability)`.
fn entry_prob(entry: &serde_json::Value) -> Option<(&str, f64)> {
    let token = entry.get("token")?.as_str()?;
    let logprob = entry.get("logprob")?.as_f64()?;
    if !logprob.is_finite() {
        return None;
    }
    Some((token, logprob.exp()))
}

/// Extract `(content, reasoning_content, top_logprobs)` from a chat response.
fn parts(response: &serde_json::Value) -> (String, String, Vec<serde_json::Value>) {
    let choice = response
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_default();
    let content = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let reasoning = choice
        .get("message")
        .and_then(|m| m.get("reasoning_content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let tops = choice
        .get("logprobs")
        .and_then(|l| l.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("top_logprobs"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    (content, reasoning, tops)
}

/// Decode with summed variant masses normalized over the expected labels.
fn from_tops(
    tops: &[serde_json::Value],
    candidates: &[&str],
    model: &str,
    version: u64,
) -> Option<Decision> {
    if tops.is_empty() || candidates.is_empty() {
        return None;
    }
    let mut masses = vec![0.0f64; candidates.len()];
    let mut seen = vec![false; candidates.len()];
    for entry in tops {
        // One corrupt entry (NaN, missing fields) is skipped; the rest
        // still decode. Aborting the whole distribution on one bad row
        // would downgrade every partially-malformed response.
        let Some((token, p)) = entry_prob(entry) else {
            continue;
        };
        if let Some(label) = match_label(token, candidates) {
            let i = candidates.iter().position(|c| *c == label)?;
            masses[i] += p;
            seen[i] = true;
        }
    }
    // Every expected label must appear among the top entries; otherwise the
    // single token cannot discriminate and the caller falls back to text.
    if seen.iter().any(|s| !s) {
        return None;
    }
    let total: f64 = masses.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let distribution: Vec<(String, f64)> = candidates
        .iter()
        .zip(masses.iter())
        .map(|(c, m)| (c.to_string(), m / total))
        .collect();
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|a, b| masses[*b].total_cmp(&masses[*a]));
    let label = candidates[order[0]].to_string();
    Decision::from_distribution(&label, distribution, model.to_string(), version, 0, false).ok()
}

#[derive(Default, fluent_wvr::FieldAccess, fluent_wvr::Describable)]
struct LabelDoc {
    #[field(desc = "Decoded single-token label")]
    label: String,
}

/// Text fallback for JSON-wrapped or bare answers, via the shared tolerant
/// codec (`parse_typed`, then `decode_boundary_typed`). Confidence is
/// downgraded: text agreement is weaker than a logprobs majority.
fn fallback(
    content: &str,
    candidates: &[&str],
    model: &str,
    version: u64,
) -> Result<Decision, DecisionError> {
    let stripped = crate::thinking::strip_think_block(content);
    let text = stripped.trim();
    if text.is_empty() {
        return Err(DecisionError::Decode("empty content".to_string()));
    }
    if let Some(label) = match_label(text, candidates) {
        return downgraded(label, candidates, model, version);
    }
    if let Ok(value) =
        crate::parse::parse_typed::<serde_json::Value>(text, &serde_json::Value::Null, |_| {})
    {
        let hits: Vec<&str> = match &value {
            serde_json::Value::String(s) => vec![s.as_str()],
            serde_json::Value::Object(o) => o.values().filter_map(|v| v.as_str()).collect(),
            serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
            _ => vec![],
        };
        for h in hits {
            if let Some(label) = match_label(h.trim(), candidates) {
                return downgraded(label, candidates, model, version);
            }
        }
    }
    if let Ok((doc, _)) = fluent_wvr::decode_boundary_typed::<LabelDoc>(text) {
        if let Some(label) = match_label(doc.label.trim(), candidates) {
            return downgraded(label, candidates, model, version);
        }
    }
    Err(DecisionError::Decode(format!(
        "text fallback matched no candidate in {text:?}"
    )))
}

/// Downgraded decision: the label holds 0.5, the rest splits evenly.
/// Marked degraded: an uncalibrated prior, never a measured majority.
fn downgraded(
    label: &str,
    candidates: &[&str],
    model: &str,
    version: u64,
) -> Result<Decision, DecisionError> {
    let rest = if candidates.len() > 1 {
        0.5 / (candidates.len() - 1) as f64
    } else {
        0.0
    };
    let distribution: Vec<(String, f64)> = candidates
        .iter()
        .map(|c| {
            if *c == label {
                (c.to_string(), 0.5)
            } else {
                (c.to_string(), rest)
            }
        })
        .collect();
    let mut decision =
        Decision::from_distribution(label, distribution, model.to_string(), version, 0, false)?;
    decision.degraded = true;
    Ok(decision)
}

/// Decode a Choice response over `candidates`.
///
/// ```rust
/// use fluent_llm::decision::decode_choice;
///
/// let body = serde_json::json!({
///     "choices": [{
///         "message": {"role": "assistant", "content": "B"},
///         "logprobs": {"content": [{
///             "token": "B", "logprob": -0.02,
///             "top_logprobs": [
///                 {"token": "B", "logprob": -0.02},
///                 {"token": "A", "logprob": -4.0},
///             ],
///         }]},
///     }],
/// });
/// assert_eq!(decode_choice(&body, &["A", "B"], "m", 1).expect("decode").label, "B");
/// ```
pub fn decode_choice(
    response: &serde_json::Value,
    candidates: &[&str],
    model: &str,
    question_version: u64,
) -> Result<Decision, DecisionError> {
    decode(response, candidates, model, question_version)
}

/// Decode an ordinal Score response over ordered `rungs` (rank order =
/// distribution order). Shares the variant-sum core with Choice; rank
/// semantics come from `score_moment`, not the label.
///
/// ```rust
/// use fluent_llm::decision::{decode_score, score_moment};
///
/// let body = serde_json::json!({
///     "choices": [{
///         "message": {"role": "assistant", "content": "5"},
///         "logprobs": {"content": [{
///             "token": "5", "logprob": -0.02,
///             "top_logprobs": [
///                 {"token": "5", "logprob": -0.02},
///                 {"token": "4", "logprob": -4.0},
///             ],
///         }]},
///     }],
/// });
/// let d = decode_score(&body, &["4", "5"], "m", 1).expect("score");
/// assert!(score_moment(&d).expect("moment").expected > 0.5);
/// ```
pub fn decode_score(
    response: &serde_json::Value,
    rungs: &[&str],
    model: &str,
    question_version: u64,
) -> Result<Decision, DecisionError> {
    if rungs.len() < 2 {
        return Err(DecisionError::Decode(
            "score needs at least two rungs".to_string(),
        ));
    }
    decode(response, rungs, model, question_version)
}

/// Decode a yes/no response.
///
/// ```rust
/// use fluent_llm::decision::decode_noul;
///
/// let body = serde_json::json!({
///     "choices": [{"message": {"role": "assistant", "content": "Yes"}}],
/// });
/// assert_eq!(decode_noul(&body, "m", 1).expect("fallback").label, "Yes");
/// ```
pub fn decode_noul(
    response: &serde_json::Value,
    model: &str,
    question_version: u64,
) -> Result<Decision, DecisionError> {
    decode(response, &["Yes", "No"], model, question_version)
}

fn decode(
    response: &serde_json::Value,
    candidates: &[&str],
    model: &str,
    question_version: u64,
) -> Result<Decision, DecisionError> {
    let (content, reasoning, tops) = parts(response);
    // Reasoning hijack: the single token went to `reasoning_content`, so the
    // content logprobs cannot discriminate — straight to text fallback (which
    // refuses the empty content with `Decode`, never a label).
    if content.trim().is_empty() && !reasoning.trim().is_empty() {
        return fallback(&content, candidates, model, question_version);
    }
    if let Some(d) = from_tops(&tops, candidates, model, question_version) {
        return Ok(d);
    }
    fallback(&content, candidates, model, question_version)
}
