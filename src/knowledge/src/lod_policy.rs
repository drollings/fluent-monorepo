//! Pure LOD policy: fidelity bands, budgets, worker context, relevance.
//!
//! Moved verbatim from `fluent-router`'s `ledger/prompt.rs` (M10.2, store-free
//! subset) + `ledger/tiering.rs::parse_tiers` (tier-text parse). No I/O, no
//! model calls, no store — fully ledger-free unit-testable. The store-coupled
//! selectors/assembler live in `selector`/`assembler` over the content seam.

use crate::lod::{Lod, LOD5_LABEL};

/// The fidelity band a worker accepts. Intermediate nodes render between
/// `max` (coarsest, e.g. `Lod::LOD5`) and `min` (finest, e.g. `Lod::LOD1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The fidelity band a worker accepts. Intermediate nodes render between
/// `max` (coarsest, e.g. `Lod::LOD5`) and `min` (finest, e.g. `Lod::LOD1`).
pub struct LodSpec {
    /// The finest fidelity an intermediate node may render at.
    pub min: Lod,
    /// The coarsest fidelity an intermediate node may render at.
    pub max: Lod,
}

impl LodSpec {
    /// A full-fidelity band over the standard lazy tiers (LOD1 finest, LOD5
    /// coarsest).
    pub fn full() -> Self {
        Self {
            min: Lod::LOD1,
            max: Lod::LOD5,
        }
    }

    /// The discrete number of tiers in the band (inclusive of both ends).
    fn span(self) -> u8 {
        self.max.as_u8().saturating_sub(self.min.as_u8())
    }

    /// The uniform mid-tier default when no relevance signal is provided.
    ///
    /// `pub`: the router's assembler goldens use it as a behavior oracle
    /// (uniform mid-tier, degrade-at-least-to-mid).
    pub fn midpoint(self) -> Lod {
        let mid = self.min.as_u8() + self.span() / 2;
        Lod::try_from(mid).unwrap_or(self.min)
    }

    /// A `Lod` in the band interpolated by a relevance rank `r` in `0..n`
    /// (0 = most relevant → finest, `n-1` = least relevant → coarsest).
    ///
    /// `pub`: assembler goldens pin interpolation bounds through it.
    pub fn interpolate(self, r: usize, n: usize) -> Lod {
        let span = usize::from(self.span());
        let denom = n.saturating_sub(1).max(1);
        let offset = (span * r) / denom;
        let level = self.min.as_u8() + offset as u8;
        Lod::try_from(level.min(LOD5_LABEL)).unwrap_or(self.min)
    }
}

/// A worker's context-window budget.
///
/// `max_chars` is the total character ceiling; `reserve_head_tail` is the
/// amount guaranteed to the first/last LOD0 anchors (they are included
/// regardless — even overrunning the reserve). Intermediates compete for the
/// remaining budget.
#[derive(Debug, Clone, Copy)]
/// A worker's context-window budget.
/// 
/// `max_chars` is the total character ceiling; `reserve_head_tail` is the
/// amount guaranteed to the first/last LOD0 anchors (they are included
/// regardless — even overrunning the reserve). Intermediates compete for the
/// remaining budget.
pub struct PromptBudget {
    pub max_chars: usize,
    pub reserve_head_tail: usize,
}

impl PromptBudget {
    /// A budget of `max_chars` with no reserved head/tail.
    pub fn new(max_chars: usize) -> Self {
        Self {
            max_chars,
            reserve_head_tail: 0,
        }
    }

    /// Build a budget from a token count using a conservative `chars_per_token`
    /// (no tokenizer dependency — a straight character estimate).
    ///
    /// M4: the token→chars conversion delegates to the shared
    /// `fluent_llm::tokens::estimate_chars_for_tokens` (exact integer
    /// equality — no behavior change). The head/tail reserve semantics stay
    /// here.
    pub fn from_tokens(tokens: usize, chars_per_token: usize) -> Self {
        Self::new(fluent_llm::tokens::estimate_chars_for_tokens(
            tokens,
            chars_per_token,
        ))
    }

    /// A budget from a token count at the conservative 4 chars/token default.
    ///
    /// M4: the `4` is the locked, documented default — changing it to a real
    /// tokenizer estimate is a behavior change (filed separately, M4.5).
    pub fn from_tokens_default(tokens: usize) -> Self {
        Self::from_tokens(tokens, 4)
    }

    /// The budget available to intermediate nodes: the ceiling minus what the
    /// first/last anchors actually consume (saturating), bounded by the
    /// reserve. Intermediate text is kept within `max_chars - head/tail cost`.
    ///
    /// `pub`: budget goldens pin saturation edges through it.
    pub fn intermediate_budget(&self, head_tail_cost: usize) -> usize {
        let by_reserve = self.max_chars.saturating_sub(self.reserve_head_tail);
        let by_actual = self.max_chars.saturating_sub(head_tail_cost);
        by_reserve.min(by_actual)
    }
}

/// A worker's system instructions: its speciality and explicit instructions.
#[derive(Debug, Clone, Default)]
/// A worker's system instructions: its speciality and explicit instructions.
pub struct WorkerContext {
    /// The worker's speciality (e.g. `"code reviewer"`), used to focus the
    /// system prompt.
    pub speciality: String,
    /// The worker's explicit instructions, from config or a model entry.
    pub instructions: String,
}

impl WorkerContext {
    /// A new worker context with the given speciality and instructions.
    pub fn new(speciality: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            speciality: speciality.into(),
            instructions: instructions.into(),
        }
    }

    /// The composed system prompt.
    ///
    /// `pub` (not `pub(crate)`): the router's coordinator composes it
    /// alongside the assembler output — same cross-crate visibility the
    /// assembler itself has.
    pub fn system_prompt(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.speciality.is_empty() {
            parts.push(format!("You are a {}.", self.speciality));
        }
        if !self.instructions.is_empty() {
            parts.push(self.instructions.clone());
        }
        parts.join("\n\n")
    }
}

/// The relevance signal for fidelity selection: `None` (uniform fidelity) or a
/// query/task embedding `Vec<f32>`.
#[derive(Debug, Clone)]
/// The relevance signal for fidelity selection: `None` (uniform fidelity) or a
/// query/task embedding `Vec<f32>`.
pub enum RelevanceSignal {
    /// Uniform fidelity — every intermediate renders at the band midpoint.
    None,
    /// A query/task embedding; intermediates are ranked by cosine similarity
    /// to it.
    Query(Vec<f32>),
}

impl RelevanceSignal {
    /// A query embedding signal.
    pub fn query(embedding: Vec<f32>) -> Self {
        Self::Query(embedding)
    }

    pub(crate) fn embedding(&self) -> Option<&[f32]> {
        match self {
            Self::Query(v) => Some(v),
            Self::None => None,
        }
    }
}

/// Parse the two-line `SUMMARY:`/`DESCRIPTION:` model output into a
/// `(summary, description)` pair, tolerating missing/empty/malformed lines
/// (fail-open: summary falls back to the trimmed full text, description to
/// `None`).
///
/// Moved verbatim from the router's tier worker (M10.2 — pure tier-text
/// parse, no store, no backend).
pub fn parse_tiers(text: &str) -> (Option<String>, Option<String>) {
    let mut summary: Option<String> = None;
    let mut desc: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("SUMMARY:") {
            summary = Some(rest.trim().to_string());
        } else if let Some(rest) = t.strip_prefix("DESCRIPTION:") {
            desc = Some(rest.trim().to_string());
        }
    }
    let summary = summary.or_else(|| Some(text.trim().to_string()));
    (summary, desc)
}

#[cfg(test)]
#[path = "../tests/lod_policy.rs"]
mod tests;