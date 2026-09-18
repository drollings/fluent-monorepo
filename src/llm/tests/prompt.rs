use super::*;

fn entry<'a>(name: &'a str, description: &'a str) -> RouteEntry<'a> {
    RouteEntry { name, description }
}

#[test]
fn render_sorts_and_bullets() {
    let out = render_available_routes(
        &[entry("zebra", "last"), entry("apple", "first"), entry("mango", "")],
        "  - ",
    );
    assert_eq!(
        out,
        "Available routes:\n  - apple: first\n  - mango\n  - zebra: last\n\n"
    );
}

#[test]
fn render_tree_bullet_and_trailing_blank() {
    let out = render_available_routes(&[entry("b", "d")], "- ");
    assert_eq!(out, "Available routes:\n- b: d\n\n");
    assert!(out.ends_with("\n\n"));
}

#[test]
fn render_empty_entries_is_header_only() {
    assert_eq!(render_available_routes(&[], "  - "), "Available routes:\n\n");
}

#[test]
fn substitute_thresholds_two_decimals() {
    let out = substitute_thresholds(
        "c={coherence_threshold} s={safety_threshold}",
        0.7,
        0.5,
    );
    assert_eq!(out, "c=0.70 s=0.50");
}

#[test]
fn substitute_thresholds_leaves_unknown_placeholders() {
    let out = substitute_thresholds("keep {other}", 0.1, 0.2);
    assert_eq!(out, "keep {other}");
}

#[test]
fn self_assessment_prompt_embeds_text_and_schema() {
    let out = build_self_assessment_prompt("summarize this");
    assert!(out.contains("summarize this"));
    assert!(out.contains("\"complexity\""));
    assert!(out.contains("Only output JSON, no other text."));
}
