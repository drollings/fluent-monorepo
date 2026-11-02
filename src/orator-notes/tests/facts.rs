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
