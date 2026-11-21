//! Truncator tests: one char-count cap for every tool and error path.
//!
//! Small outputs pass verbatim; over-cap outputs keep the head plus a
//! total-length marker. Single home; tool and session crates compose it.

use crate::truncate::{truncate_tool_output, TOOL_OUTPUT_CHARS};

#[test]
fn small_outputs_pass_verbatim() {
    assert_eq!(truncate_tool_output("hi"), "hi");
    assert_eq!(truncate_tool_output(""), "");
}

#[test]
fn boundary_passes_verbatim() {
    let exact = "z".repeat(TOOL_OUTPUT_CHARS);
    assert_eq!(truncate_tool_output(&exact), exact);
}

#[test]
fn over_cap_keeps_head_with_total_marker() {
    let big = "z".repeat(TOOL_OUTPUT_CHARS + 500);
    let capped = truncate_tool_output(&big);
    assert!(capped.starts_with(&"z".repeat(TOOL_OUTPUT_CHARS)));
    assert!(
        capped.contains(&format!("{} total chars", TOOL_OUTPUT_CHARS + 500)),
        "{capped}"
    );
    assert!(capped.chars().count() < big.chars().count());
}

#[test]
fn truncate_marker_present() {
    let over = "y".repeat(TOOL_OUTPUT_CHARS + 10);
    let cut = truncate_tool_output(&over);
    assert!(cut.contains("(truncated,"), "loud cut: {cut}");
    assert!(cut.contains(&format!("{} total chars", TOOL_OUTPUT_CHARS + 10)));
    assert_eq!(
        truncate_tool_output(&"z".repeat(TOOL_OUTPUT_CHARS)),
        "z".repeat(TOOL_OUTPUT_CHARS)
    );
}
