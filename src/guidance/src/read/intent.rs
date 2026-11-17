//! Deterministic intent routing for the read engine.
//!
//! The rule table below replaces the word-count ladder for read-plane
//! routing: exact evidence (empty, file path, single identifier)
//! decides first, then cue phrases route to planes, and everything
//! else falls through to `General`. The legacy `FsmEngine::classify`
//! is untouched so existing callers stay byte-identical — the read
//! engine is the only consumer of this table. Ambiguous queries are
//! the only ones a decision model may ever see (after calibration);
//! exact symbol/path controls never call a model.

use crate::query::strategy::QueryIntent;

/// Lowercase cue match on whole-word boundaries (M3.3: shared
/// `contains_token` core with the intent-routing separator rule).
fn contains_cue(lower: &str, cue: &str) -> bool {
    common_core::string::contains_token(lower, cue, |c: char| {
        !c.is_alphanumeric() && c != '_'
    })
}

/// Cue-phrase match: every word of the phrase appears in order.
fn contains_phrase(lower: &str, phrase: &str) -> bool {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }
    let mut rest = lower;
    for word in words {
        let Some(position) = rest.find(word) else {
            return false;
        };
        let end = position + word.len();
        let before_ok = position == 0
            || !rest[..position]
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after_ok = end >= rest.len()
            || !rest[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !(before_ok && after_ok) {
            return false;
        }
        rest = &rest[end..];
    }
    true
}

fn any_phrase(lower: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| contains_phrase(lower, phrase))
}

/// File-path evidence: a slash/backslash token or a dotted name with
/// an uppercase character (type-relative paths).
fn looks_like_path(token: &str) -> bool {
    token.contains('/')
        || token.contains('\\')
        || token.contains('.') && token.chars().any(|c| c.is_ascii_uppercase())
}

/// Identifier evidence: the shared identifier pattern.
fn looks_like_identifier(token: &str) -> bool {
    crate::query::identifier::detect_identifier_pattern(token).is_some()
}

/// Classify one query through the deterministic rule table. Pure:
/// no I/O, no model.
#[must_use]
/// Classify one query through the deterministic rule table. Pure:
/// no I/O, no model.
pub fn classify(query: &str) -> QueryIntent {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return QueryIntent::GeneralSearch;
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let lower = trimmed.to_lowercase();
    let ownership_words = [
        "owns", "crate", "owner", "live", "lives", "member", "members",
    ];
    let has_ownership_word = tokens
        .iter()
        .any(|token| ownership_words.contains(&token.to_lowercase().as_str()));
    let mechanism_cue = lower.starts_with("how ")
        || lower.starts_with("why ")
        || lower.starts_with("what happens")
        || contains_cue(&lower, "mechanism")
        || contains_cue(&lower, "pipeline");
    let path_token = tokens.iter().any(|token| looks_like_path(token));
    // Graph navigation cues win over generic verbs and path-looking
    // arguments: `callers of src/a.rs` is an edge walk, not a file read.
    if any_phrase(
        &lower,
        &[
            "callers of",
            "caller of",
            "callees of",
            "callee of",
            "calls ",
            "called by",
            "dependents of",
            "dependent of",
            "depends on",
            "dependencies of",
            "owner of",
        ],
    ) {
        return QueryIntent::Graph;
    }
    // File paths only when the query is path-shaped, not whenever an
    // ownership/mechanism question happens to mention `.guidance/target`.
    let path_shaped = path_token
        && (tokens.len() == 1
            || any_phrase(&lower, &["where is", "where lives", "open ", "path "]))
        && !has_ownership_word
        && !mechanism_cue;
    if path_shaped {
        return QueryIntent::FilePath;
    }
    if tokens.len() == 1 && tokens.iter().any(|token| looks_like_identifier(token)) {
        return QueryIntent::SingleIdentifier;
    }
    // Orientation cues route to project cards first — the funnel
    // through documentation beats guessing a symbol from prose.
    if any_phrase(
        &lower,
        &[
            "major components",
            "member projects",
            "workspace members",
            "what is this repo",
            "what is this repository",
            "what does this repo do",
            "high level",
            "codebase",
        ],
    ) {
        return QueryIntent::Orientation;
    }
    // Explicit symbol questions route to exact lookup, even with
    // generic verbs elsewhere — but ownership questions about a name
    // (`which crate owns X`) stay with the ownership map.
    if any_phrase(&lower, &["where is", "what is", "define"])
        && tokens.iter().any(|token| looks_like_identifier(token))
        && !has_ownership_word
    {
        return QueryIntent::SingleIdentifier;
    }
    // Ownership cues route to the ownership map, not to code recall.
    if any_phrase(
        &lower,
        &[
            "which crate owns",
            "which crate",
            "who owns",
            "where does",
            "where lives",
            "owns ",
            "owner of",
        ],
    ) {
        return QueryIntent::Ownership;
    }
    // Capability/provenance cues route to external cards + docs.
    if any_phrase(
        &lower,
        &[
            "where do",
            "come from",
            "what consumes",
            "which component handles",
            "capabilit",
        ],
    ) {
        return QueryIntent::Provenance;
    }
    // Mechanism questions stay code-first.
    if lower.starts_with("how ")
        || lower.starts_with("why ")
        || lower.starts_with("what happens")
        || contains_cue(&lower, "mechanism")
        || contains_cue(&lower, "pipeline")
    {
        return QueryIntent::HowTo;
    }
    QueryIntent::GeneralSearch
}

/// Whether symbol extraction runs for this intent: only `Symbol` and
/// `Graph` carry identifier scope. Every other intent treats
/// identifier-shaped tokens as plain vocabulary.
#[must_use]
/// Whether symbol extraction runs for this intent: only `Symbol` and
/// `Graph` carry identifier scope. Every other intent treats
/// identifier-shaped tokens as plain vocabulary.
pub fn extracts_symbols(intent: QueryIntent) -> bool {
    matches!(intent, QueryIntent::SingleIdentifier | QueryIntent::Graph)
}

/// Exact-evidence check: empty queries, file paths, and single
/// identifiers resolve deterministically. Single rule behind both
/// `classify` and the calibration eligibility check, so the two can
/// never drift: exact queries never reach a decision model.
#[must_use]
/// Exact-evidence check: empty queries, file paths, and single
/// identifiers resolve deterministically. Single rule behind both
/// `classify` and the calibration eligibility check, so the two can
/// never drift: exact queries never reach a decision model.
pub fn is_exact(query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    matches!(
        classify(query),
        QueryIntent::SingleIdentifier | QueryIntent::FilePath
    )
}

#[cfg(test)]
#[path = "../../tests/read_intent.rs"]
mod read_intent_tests;
