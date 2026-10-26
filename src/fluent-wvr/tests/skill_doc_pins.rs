//! Compiled pins for skill-doc claims that previously drifted from code.
//!
//! The `describe()` numbers shape is pinned at runtime by the schema tests;
//! this suite pins the *documented* contract instead: the closed
//! `#[field(...)]` vocabulary, the planned-not-implemented version
//! annotations, and the no-blanket-impl rule stated as a coherence limit
//! (not unsoundness). If the guide regresses, this gate fails.
//!
//! The vocabulary list must match `parse_field_attrs` in
//! `src/fluent-wvr-macros/src/lib.rs` exactly: the test fails when an
//! argument ships without documentation, and (via the derive's
//! closed-vocabulary error) when documentation names an argument that no
//! longer exists.

const SKILL: &str = include_str!("../../../doc/skills/fluent-wvr/SKILL.md");

/// Every argument `parse_field_attrs` accepts. Validation/emission first,
/// then presentation-only `x-` hints.
const CLOSED_VOCABULARY: [&str; 24] = [
    "skip",
    "desc",
    "min",
    "max",
    "clamp",
    "required",
    "format",
    "max_len",
    "sanitize",
    "pattern",
    "coerce",
    "parse",
    "variants",
    "empty_is_none",
    "nested",
    "items",
    "fixed",
    "visible_when",
    "axis",
    "tab",
    "group",
    "tooltip",
    "step",
    "inline",
];

#[test]
fn skill_records_closed_field_vocabulary() {
    for arg in CLOSED_VOCABULARY {
        assert!(
            SKILL.contains(&format!("`{arg}`")),
            "skill guide must record `#[field({arg})]` in the closed vocabulary"
        );
    }
}

#[test]
fn skill_marks_version_annotations_planned_not_implemented() {
    assert!(
        SKILL.contains("version_added"),
        "guide must still mention the reserved version annotations"
    );
    assert!(
        SKILL.contains("Planned, not implemented"),
        "guide must mark version_* as planned, not implemented"
    );
    assert!(
        SKILL.contains("fails to compile"),
        "guide must say version_* use fails to compile, not a silent no-op"
    );
}

#[test]
fn skill_states_no_blanket_impl_as_coherence_limit() {
    assert!(
        SKILL.contains("no blanket"),
        "guide must keep the no-blanket-impl rule"
    );
    assert!(
        SKILL.contains("E0119") || SKILL.contains("coherence"),
        "guide must state the coherence limit behind the rule"
    );
    assert!(
        SKILL.contains("impl_component!"),
        "guide must name the macro mechanism for the rule"
    );
    assert!(
        !SKILL.contains("blanket impl is unsound"),
        "guide must not regress to the unsoundness wording"
    );
}

#[test]
fn skill_numbers_shape_never_string_encoded() {
    assert!(
        !SKILL.contains(r#""minimum": ""#),
        "guide snippets must not show string-encoded minimum bounds"
    );
    assert!(
        !SKILL.contains(r#""maximum": ""#),
        "guide snippets must not show string-encoded maximum bounds"
    );
}
