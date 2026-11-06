//! Markdown facts and the rolling summary.
//!
//! Long-lived memory is a Markdown facts file (one claim per line,
//! newest appended) plus a rolling summary kept under a token budget:
//! the newest turn is protected, truncation eats from the middle, and
//! the system rule is never what gets evicted.

use fluent_llm::tokens::{estimate_tokens, TokenBudget};

/// Budget for the rolling summary in tokens.
pub const SUMMARY_BUDGET: usize = 1_000;

/// Append one fact line to the Markdown facts file content.
#[must_use]
pub fn append_fact(facts: &str, fact: &str) -> String {
    let mut out = facts.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("- ");
    out.push_str(fact.trim());
    out.push('\n');
    out
}

/// Roll `summary` plus `newest_turn` under the budget: the newest turn
/// is always kept whole; the older summary truncates from the middle
/// (head and tail survive, the middle compresses to a marker).
///
/// M10-S4 audit (keep-decision): the summary head already composes the
/// shared `TokenBudget::truncate_to_budget`; the remaining join is a
/// two-text shape (truncated summary + whole newest) with no
/// single-text `truncate_middle` equivalent — expressing it through
/// the splitter would change bytes (ratio-based head vs char-count
/// head, `"..."` + `"\n…\n"` double marker), so it stays. Treat any
/// future middle-vs-head change here as behavior work, never a
/// refactor fold-in.
#[must_use]
pub fn roll_summary(summary: &str, newest_turn: &str) -> String {
    let newest_tokens = estimate_tokens(newest_turn) as usize;
    if newest_tokens >= SUMMARY_BUDGET {
        return newest_turn.to_string();
    }
    let remaining = SUMMARY_BUDGET - newest_tokens;
    let kept = TokenBudget(remaining).truncate_to_budget(summary);
    if kept.is_empty() {
        return newest_turn.to_string();
    }
    format!("{kept}\n…\n{newest_turn}")
}

#[cfg(test)]
#[path = "../tests/facts.rs"]
mod tests;
