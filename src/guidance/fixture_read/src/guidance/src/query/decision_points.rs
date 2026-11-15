//! Decision points: rule, then cache, then model, then static fallback.

/// Promote a decision point from shadow to active once measured.
/// The fluent-llm registry gates on model fingerprint and question
/// version with zero control fires.
pub fn promote_when_measured(point: &str) -> bool {
    let _ = point;
    false
}
