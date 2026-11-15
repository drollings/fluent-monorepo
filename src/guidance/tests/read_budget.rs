//! Per-intent budget contracts: allocation table and truncation.
//! Included via `#[path]` forwarder from `read/budget.rs`, per repo
//! convention. Pure: no I/O, no model.

use super::*;
use crate::query::strategy::QueryIntent;

#[test]
fn allocation_table_pins_section_budgets() {
    assert_eq!(
        budgets_for(QueryIntent::SingleIdentifier),
        SectionBudgets {
            project: 0,
            evidence: 0,
            code: 400,
            total: 400
        }
    );
    assert_eq!(
        budgets_for(QueryIntent::Graph),
        SectionBudgets {
            project: 100,
            evidence: 0,
            code: 900,
            total: 1000
        }
    );
    assert_eq!(
        budgets_for(QueryIntent::Ownership),
        SectionBudgets {
            project: 300,
            evidence: 300,
            code: 200,
            total: 800
        }
    );
    assert_eq!(
        budgets_for(QueryIntent::Orientation),
        SectionBudgets {
            project: 400,
            evidence: 400,
            code: 200,
            total: 1000
        }
    );
    assert_eq!(
        budgets_for(QueryIntent::HowTo),
        SectionBudgets {
            project: 100,
            evidence: 300,
            code: 700,
            total: 1100
        }
    );
    assert_eq!(
        budgets_for(QueryIntent::Provenance),
        SectionBudgets {
            project: 300,
            evidence: 500,
            code: 200,
            total: 1000
        }
    );
    assert_eq!(
        budgets_for(QueryIntent::GeneralSearch),
        SectionBudgets {
            project: 100,
            evidence: 300,
            code: 700,
            total: 1100
        }
    );
    // Legacy code-first intents keep the mechanism envelope.
    assert_eq!(budgets_for(QueryIntent::Conceptual).total, 1100);
}

#[test]
fn totals_stay_within_human_and_orchestrator_envelopes() {
    for intent in [
        QueryIntent::SingleIdentifier,
        QueryIntent::Graph,
        QueryIntent::Ownership,
        QueryIntent::Orientation,
        QueryIntent::HowTo,
        QueryIntent::Conceptual,
        QueryIntent::Provenance,
        QueryIntent::GeneralSearch,
        QueryIntent::FilePath,
        QueryIntent::CapabilityQuery,
        QueryIntent::MultiKeyword,
    ] {
        let budgets = budgets_for(intent);
        assert!(budgets.total <= 4000, "{intent:?}");
        assert!(
            budgets.project + budgets.evidence + budgets.code <= budgets.total,
            "{intent:?}: sections must fit the total"
        );
    }
}

#[test]
fn enforcement_truncates_over_budget_text() {
    let text = "word ".repeat(1000);
    let full = enforce_budget(&text, 10000);
    assert_eq!(full, text);
    let cut = enforce_budget(&text, 10);
    assert!(cut.len() < text.len(), "over-budget truncates");
    assert!(cut.ends_with("..."), "{cut:?}");
    assert!(enforce_budget(&text, 0).is_empty());
    assert_eq!(tokens_for(""), 0);
    assert!(tokens_for("hello world") > 0);
    assert!(tokens_for(&text) > tokens_for("hello world"));
}
