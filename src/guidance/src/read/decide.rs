//! Native decision adapter for the read plane.
//!
//! Doc and capability candidates are judged with bounded native
//! `choice` questions through `DecideBackend` — never free-text JSON.
//! The judge returns labels only; before the capability point passes
//! calibration, everything is `Considered` and never feeds
//! synthesis or traversal (fail-closed, rank stays authoritative).

use fluent_llm::decision::{DecideBackend, DecideRequest, DecisionKind, DecisionOption};

/// One label family for the generic candidate judge. Each family
/// preserves its prompt bytes, option order, and fail-closed default;
/// the generic only batches and aligns.
pub trait CandidateLabel: Copy + Clone + PartialEq + Eq + std::fmt::Debug {
    /// Label text sent as the option and read back.
    fn as_str(self) -> &'static str;
    /// Parse a model label, fail-closed to the family default.
    fn from_label(label: &str) -> Self;
    /// Ordered options with descriptions (order is the contract).
    fn options() -> Vec<DecisionOption>;
    /// One state line for the shared `Query + numbered excerpts` block.
    fn state_line(index: usize, id_label: &str, excerpt: &str) -> String;
    /// Per-candidate prompt (byte-preserved per family).
    fn prompt(index: usize) -> String;
    /// Per-candidate question naming the candidate id.
    fn question(index: usize, id_label: &str) -> String;
}

/// One judged candidate (generic verdict).
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateVerdict<L> {
    /// Candidate id.
    pub id: String,
    /// Judge label (fail-closed to the family default).
    pub label: L,
    /// Producer self-doubt (ordering and logs only, never a gate).
    pub confidence: f64,
}

/// Judge verdict for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocLabel {
    /// Directly answers the query from its excerpt.
    Strong,
    /// Related but not decisive, or unjudged.
    Considered,
}

impl CandidateLabel for DocLabel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Considered => "considered",
        }
    }

    fn from_label(label: &str) -> Self {
        // M4.3: shared closed match; fail-closed default preserved.
        match fluent_llm::decision::match_text_label(label, &["strong"]) {
            Some(_) => Self::Strong,
            None => Self::Considered,
        }
    }

    fn options() -> Vec<DecisionOption> {
        vec![
            DecisionOption {
                label: "strong".to_string(),
                description: "The excerpt directly answers the query.".to_string(),
            },
            DecisionOption {
                label: "considered".to_string(),
                description: "Related but not decisive, or insufficient.".to_string(),
            },
        ]
    }

    fn state_line(index: usize, id_label: &str, excerpt: &str) -> String {
        format!("Candidate {} [{id_label}]: {excerpt}\n", index + 1)
    }

    fn prompt(index: usize) -> String {
        format!("Does candidate {} answer the query?", index + 1)
    }

    fn question(index: usize, id_label: &str) -> String {
        format!("Judge candidate {} [{id_label}]", index + 1)
    }
}

impl DocLabel {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        (*self).as_str()
    }
}

/// One judged candidate.
pub type DocVerdict = CandidateVerdict<DocLabel>;

/// Native doc/capability judge over one `DecideBackend`.
pub type DocJudge<'a> = CandidateJudge<'a, DocLabel>;

/// Generic candidate judge over one `DecideBackend`: one batched
/// `choice` per candidate, aligned by id. Transport or decode failures
/// return the family fail-closed label — ranking continues unjudged
/// rather than failing the read. The input cap below is a cost cap,
/// never a relevance threshold.
pub struct CandidateJudge<'a, L: CandidateLabel> {
    backend: &'a dyn DecideBackend,
    model: String,
    timeout_ms: u64,
    _marker: std::marker::PhantomData<L>,
}

/// Judge input cost cap: findings times excerpt chars. A cost cap,
/// never a relevance threshold — truncation reports a gap.
pub const JUDGE_MAX_FINDINGS: usize = 20;
/// Max excerpt chars per judged candidate (cost cap, not relevance).
pub const JUDGE_MAX_EXCERPT_CHARS: usize = 600;

impl<'a, L: CandidateLabel> CandidateJudge<'a, L> {
    /// Borrow a decision backend (usually the `decision` role over
    /// `/v1/systemone`).
    #[must_use]
    pub fn new(backend: &'a dyn DecideBackend, model: String, timeout_ms: u64) -> Self {
        Self {
            backend,
            model,
            timeout_ms,
            _marker: std::marker::PhantomData,
        }
    }

    /// Judge candidates with one batched `choice` per candidate.
    pub fn judge(&self, query: &str, candidates: &[(String, String)]) -> Vec<CandidateVerdict<L>> {
        if candidates.is_empty() {
            return Vec::new();
        }
        let capped: Vec<(String, String)> = candidates
            .iter()
            .take(JUDGE_MAX_FINDINGS)
            .map(|(id, excerpt)| {
                let excerpt = if excerpt.len() > JUDGE_MAX_EXCERPT_CHARS {
                    common_core::string::truncate_utf8(excerpt, JUDGE_MAX_EXCERPT_CHARS)
                } else {
                    excerpt.clone()
                };
                (id.clone(), excerpt)
            })
            .collect();
        let mut state = format!("Query: {query}\n");
        for (index, (label, excerpt)) in capped.iter().enumerate() {
            state.push_str(&L::state_line(index, label, excerpt));
        }
        let requests: Vec<DecideRequest> = capped
            .iter()
            .enumerate()
            .map(|(index, (label, _))| DecideRequest {
                prompt: L::prompt(index),
                state: state.clone(),
                question: L::question(index, label),
                options: L::options(),
                kind: DecisionKind::Choice,
                version: 1,
                timeout_ms: self.timeout_ms,
                model: self.model.clone(),
            })
            .collect();
        let ids: Vec<String> = capped.iter().map(|(id, _)| id.clone()).collect();
        let outcomes = fluent_llm::client::block_on(self.backend.decide_batch(requests));
        ids.into_iter()
            .zip(outcomes)
            .map(|(id, outcome)| match outcome {
                Ok(decision) => CandidateVerdict {
                    id: id.clone(),
                    label: L::from_label(&decision.label),
                    confidence: decision.confidence,
                },
                Err(_) => CandidateVerdict {
                    id,
                    label: L::from_label(""),
                    confidence: 0.0,
                },
            })
            .collect()
    }
}

/// Finding relevance verdict: relevant, irrelevant, or insufficient.
/// `Insufficient` renders as a gap, never as evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingLabel {
    Relevant,
    Irrelevant,
    Insufficient,
}

impl CandidateLabel for FindingLabel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Relevant => "relevant",
            Self::Irrelevant => "irrelevant",
            Self::Insufficient => "insufficient",
        }
    }

    fn from_label(label: &str) -> Self {
        // M4.3: shared closed match; fail-closed default preserved.
        match fluent_llm::decision::match_text_label(label, &["relevant", "irrelevant"]) {
            Some("relevant") => Self::Relevant,
            Some("irrelevant") => Self::Irrelevant,
            _ => Self::Insufficient,
        }
    }

    fn options() -> Vec<DecisionOption> {
        vec![
            DecisionOption {
                label: "relevant".to_string(),
                description: "The finding directly answers the query.".to_string(),
            },
            DecisionOption {
                label: "irrelevant".to_string(),
                description: "The finding does not answer the query.".to_string(),
            },
            DecisionOption {
                label: "insufficient".to_string(),
                description: "Cannot tell from the excerpt alone.".to_string(),
            },
        ]
    }

    fn state_line(index: usize, id_label: &str, excerpt: &str) -> String {
        format!("Finding {} [{id_label}]: {excerpt}\n", index + 1)
    }

    fn prompt(index: usize) -> String {
        format!("Is finding {} relevant to the query?", index + 1)
    }

    fn question(index: usize, id_label: &str) -> String {
        format!("Judge finding {} [{id_label}]", index + 1)
    }
}

impl FindingLabel {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        (*self).as_str()
    }
}

/// One finding verdict.
pub type FindingVerdict = CandidateVerdict<FindingLabel>;

/// Native finding-relevance judge: one bounded `choice` per finding
/// (`state` carries the query plus the numbered findings; `criteria`
/// describe relevant/irrelevant/insufficient). Batched and aligned:
/// one multi-question POST per query, verdicts in request order.
/// Transport or decode failures return all-`Insufficient` — shadow
/// stays honest without failing the read.
pub type FindingJudge<'a> = CandidateJudge<'a, FindingLabel>;

/// Claim-entailment verdict: the cited excerpt states the claim,
/// contradicts it, or cannot settle it. `Insufficient` renders as a
/// gap, never as evidence; `Contradicted` drops only when the
/// support point calibrates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportLabel {
    Supported,
    Contradicted,
    Insufficient,
}

impl CandidateLabel for SupportLabel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Contradicted => "contradicted",
            Self::Insufficient => "insufficient",
        }
    }

    fn from_label(label: &str) -> Self {
        // M4.3: shared closed match; fail-closed default preserved.
        match fluent_llm::decision::match_text_label(label, &["supported", "contradicted"]) {
            Some("supported") => Self::Supported,
            Some("contradicted") => Self::Contradicted,
            _ => Self::Insufficient,
        }
    }

    fn options() -> Vec<DecisionOption> {
        vec![
            DecisionOption {
                label: "supported".to_string(),
                description: "The excerpt states the claim.".to_string(),
            },
            DecisionOption {
                label: "contradicted".to_string(),
                description: "The excerpt refutes the claim.".to_string(),
            },
            DecisionOption {
                label: "insufficient".to_string(),
                description: "The excerpt cannot settle the claim.".to_string(),
            },
        ]
    }

    fn state_line(index: usize, id_label: &str, excerpt: &str) -> String {
        format!("Claim {} [{id_label}]: {excerpt}\n", index + 1)
    }

    fn prompt(index: usize) -> String {
        format!("Does the excerpt support claim {}?", index + 1)
    }

    fn question(index: usize, id_label: &str) -> String {
        format!("Judge claim {} [{id_label}]", index + 1)
    }
}

impl SupportLabel {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        (*self).as_str()
    }
}

/// One entailment verdict.
pub type SupportVerdict = CandidateVerdict<SupportLabel>;

/// Native entailment judge: one bounded `choice` per bound claim
/// (claim text plus its cited excerpt). Batched and aligned.
/// Transport or decode failures return all-`Insufficient`.
pub type SupportJudge<'a> = CandidateJudge<'a, SupportLabel>;

#[cfg(test)]
#[path = "../../tests/read_judge.rs"]
mod read_judge_tests;

#[cfg(test)]
#[path = "../../tests/finding_relevance.rs"]
mod finding_relevance_tests;

#[cfg(test)]
#[path = "../../tests/judge_prompt.rs"]
mod judge_prompt_tests;
