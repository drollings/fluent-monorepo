//! Per-intent token budgets for the read engine.
//!
//! Budgets are engineering limits (truncation, never relevance) drawn
//! from the measured allocation table. Enforcement rides the shared
//! `fluent_llm::tokens::TokenBudget` — no custom estimator.

use fluent_llm::tokens::{estimate_tokens, TokenBudget};

use crate::query::strategy::QueryIntent;

/// Per-section token allocation for one intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Per-section token allocation for one intent.
pub struct SectionBudgets {
    /// Project-map cards.
    pub project: usize,
    /// Docs and capability cards.
    pub evidence: usize,
    /// Code findings.
    pub code: usize,
    /// Total answer target.
    pub total: usize,
}

impl SectionBudgets {
    /// Spend ledger for this allocation (one shared budget per answer).
    #[must_use]
    pub fn ledger(&self) -> TokenBudget {
        TokenBudget(self.total)
    }
}

/// Allocation table: symbol queries are tiny, orientation spends on
/// cards, mechanism spends on code. Totals stay within the human
/// (4000) and orchestrator (2000) envelopes.
#[must_use]
/// Allocation table: symbol queries are tiny, orientation spends on
/// cards, mechanism spends on code. Totals stay within the human
/// (4000) and orchestrator (2000) envelopes.
pub fn budgets_for(intent: QueryIntent) -> SectionBudgets {
    match intent {
        QueryIntent::SingleIdentifier => SectionBudgets {
            project: 0,
            evidence: 0,
            code: 400,
            total: 400,
        },
        QueryIntent::Graph => SectionBudgets {
            project: 100,
            evidence: 0,
            code: 900,
            total: 1000,
        },
        QueryIntent::Ownership => SectionBudgets {
            project: 300,
            evidence: 300,
            code: 200,
            total: 800,
        },
        QueryIntent::Orientation => SectionBudgets {
            project: 400,
            evidence: 400,
            code: 200,
            total: 1000,
        },
        QueryIntent::HowTo | QueryIntent::Conceptual | QueryIntent::GeneralSearch => {
            SectionBudgets {
                project: 100,
                evidence: 300,
                code: 700,
                total: 1100,
            }
        }
        QueryIntent::Provenance => SectionBudgets {
            project: 300,
            evidence: 500,
            code: 200,
            total: 1000,
        },
        QueryIntent::FilePath | QueryIntent::CapabilityQuery | QueryIntent::MultiKeyword => {
            SectionBudgets {
                project: 0,
                evidence: 300,
                code: 700,
                total: 1000,
            }
        }
    }
}

/// Truncate rendered text to a section budget, estimating with the
/// shared token counter.
#[must_use]
/// Truncate rendered text to a section budget, estimating with the
/// shared token counter.
pub fn enforce_budget(text: &str, budget_tokens: usize) -> String {
    let budget = TokenBudget(budget_tokens);
    budget.truncate_to_budget(text)
}

/// Estimated tokens for rendered text (accounting input).
#[must_use]
/// Estimated tokens for rendered text (accounting input).
pub fn tokens_for(text: &str) -> u64 {
    estimate_tokens(text)
}

#[cfg(test)]
#[path = "../../tests/read_budget.rs"]
mod read_budget_tests;

#[cfg(test)]
#[path = "../../tests/budget_honesty.rs"]
mod budget_honesty_tests;
