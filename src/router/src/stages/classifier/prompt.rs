//! Prompt fragments shared by the flat classifier prompt and the
//! classification-tree node prompt.
//!
//! Both prompts render a sorted "Available routes" list and substitute the
//! `{coherence_threshold}` / `{safety_threshold}` placeholders with `{:.2}`
//! values. The two output schemas differ (`action` vs `route`), so only these
//! fragments are shared — the schema bodies stay with their owners.

use std::fmt::Write;

/// One route line in the shared "Available routes" block.
pub(crate) struct RouteEntry<'a> {
    pub name: &'a str,
    pub description: &'a str,
}

/// Render the sorted "Available routes" block, including the trailing blank
/// line. `bullet` is `"  - "` in the flat classifier prompt and `"- "` in
/// the tree node prompt; sorting and the empty-description rule are identical
/// in both.
pub(crate) fn render_available_routes(entries: &[RouteEntry<'_>], bullet: &str) -> String {
    let mut sorted: Vec<&RouteEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(b.name));
    let mut out = String::from("Available routes:\n");
    for e in sorted {
        if e.description.is_empty() {
            let _ = writeln!(out, "{bullet}{}", e.name);
        } else {
            let _ = writeln!(out, "{bullet}{}: {}", e.name, e.description);
        }
    }
    out.push('\n');
    out
}

/// Substitute `{coherence_threshold}` / `{safety_threshold}` with `{:.2}`
/// values. Serves the flat preamble template and the tree node's
/// incoherence/unsafe guidance line alike.
pub(crate) fn substitute_thresholds(
    template: &str,
    coherence_threshold: f64,
    safety_threshold: f64,
) -> String {
    template
        .replace(
            "{coherence_threshold}",
            &format!("{coherence_threshold:.2}"),
        )
        .replace("{safety_threshold}", &format!("{safety_threshold:.2}"))
}
