//! Pure LLM prompt builders shared across router stages.
//!
//! Moved verbatim from `fluent-router` (M6.3): `render_available_routes` +
//! `substitute_thresholds` (+ `RouteEntry`) from
//! `stages/classifier/prompt.rs` (shared by the flat classifier prompt, the
//! tree node prompt, and the config-compiled variants), and
//! `build_self_assessment_prompt` from `target_match.rs`. Pure
//! string-in/string-out builders with no backend, config, or ledger coupling —
//! the stage that owns the route table supplies the entries.
//!
//! What stays router-local (M6.3): the rigor blue/red/judge protocol prompts
//! (`routes/rigor/prompts.rs` — bound to `RedObjection`/`RigorContext`/ledger
//! node ids), the escalation team-mode gap/draft/judge prompts
//! (`dispatch/escalation/modes.rs` — inline in the ladder flow), and the
//! classifier system-prompt assembly (route table + thresholds + overlay
//! context owned by the stage). Prompt goldens pinning those live in the
//! router (`tests/stages_classifier.rs`,
//! `classifier_prompt_unchanged_without_overlay_hints`).

use std::fmt::Write;

/// One route line in the shared "Available routes" block.
pub struct RouteEntry<'a> {
    pub name: &'a str,
    pub description: &'a str,
}

/// Render the sorted "Available routes" block, including the trailing blank
/// line. `bullet` is `"  - "` in the flat classifier prompt and `"- "` in
/// the tree node prompt; sorting and the empty-description rule are identical
/// in both.
pub fn render_available_routes(entries: &[RouteEntry<'_>], bullet: &str) -> String {
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
pub fn substitute_thresholds(
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

/// Build the target self-assessment prompt: rate the request complexity
/// 0–10 as exactly one JSON object.
pub fn build_self_assessment_prompt(user_text: &str) -> String {
    format!(
        "You are asked to rate the complexity of the user's request on a scale of\n\
         0 (trivial) to 10 (requires the most capable model available).\n\n\
         User request: {user_text}\n\n\
         Output exactly one JSON object:\n\
         \x20 {{\"complexity\": <integer 0-10>, \"reason\": \"<brief justification>\"}}\n\
         Only output JSON, no other text."
    )
}

#[cfg(test)]
#[path = "../tests/prompt.rs"]
mod tests;
