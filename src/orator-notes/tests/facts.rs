//! Facts tests: append shape, rolling budget, newest protected.

use crate::facts::{append_fact, roll_summary};

#[test]
fn facts_append_markdown_lines() {
    let facts = append_fact("", "user prefers tea");
    assert_eq!(facts, "- user prefers tea\n");
    let facts = append_fact(&facts, "meeting at noon");
    assert!(facts.contains("- user prefers tea\n"));
    assert!(facts.contains("- meeting at noon\n"));
}

#[test]
fn rolling_summary_protects_the_newest_turn() {
    let newest = "User: the final decision is yes.";
    let rolled = roll_summary(&"old context ".repeat(500), newest);
    assert!(rolled.ends_with(newest));
    assert!(rolled.len() < "old context ".repeat(500).len() + newest.len());
}

/// M10-S1 characterization: newest-kept-whole edges. An over-budget
/// newest returns verbatim (no summary, no marker); an empty summary
/// returns the newest; an empty newest keeps the (possibly truncated)
/// summary behind the `…` marker.
#[test]
fn roll_summary_pins_newest_whole_and_empty_edges() {
    use crate::facts::{roll_summary, SUMMARY_BUDGET};
    use fluent_llm::tokens::estimate_tokens;
    // Over-budget newest: verbatim, nothing else.
    let newest = "y".repeat(5000);
    assert!((estimate_tokens(&newest) as usize) >= SUMMARY_BUDGET);
    assert_eq!(roll_summary("old context", &newest), newest);
    // Empty summary: newest alone.
    assert_eq!(roll_summary("", "hi"), "hi");
    // Empty newest: summary (fits, verbatim) behind the marker.
    assert_eq!(roll_summary("old", ""), "old\n…\n");
    // Exactly-at-budget newest: verbatim (>= fires, not just >).
    let at_budget = "z".repeat(4000);
    assert!((estimate_tokens(&at_budget) as usize) >= SUMMARY_BUDGET);
    assert_eq!(roll_summary("old context", &at_budget), at_budget);
}
