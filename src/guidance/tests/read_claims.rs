//! Claim extraction, citation binding, support gating, and the
//! moved path/range prefilter. Included via `#[path]` forwarder from
//! `read/claims.rs`, per repo convention. Pure: no I/O, no model.

use super::*;

#[test]
fn sentences_split_on_terminal_punctuation() {
    assert_eq!(
        extract_sentences("First. Second! Third? Tail"),
        vec!["First.", "Second!", "Third?", "Tail"]
    );
    assert_eq!(extract_sentences(""), Vec::<String>::new());
    assert_eq!(extract_sentences("  "), Vec::<String>::new());
}

#[test]
fn claims_bind_to_named_citations_only() {
    let claims = bind_claims(
        "Alpha adds one (src/a.rs:1-3). Baking is nice.",
        &["src/a.rs:1".to_string(), "src/b.rs:9".to_string()],
    );
    assert_eq!(claims.len(), 2);
    assert!(claims[0].citation.is_some());
    assert!(claims[1].citation.is_none());
    assert!(claims
        .iter()
        .all(|claim| claim.support == Support::Unverified));
}

#[test]
fn gating_drops_unsupported_and_labels_the_rest() {
    let claims = vec![
        Claim {
            text: "Kept but unverified.".to_string(),
            citation: Some("src/a.rs:1".to_string()),
            support: Support::Unverified,
        },
        Claim {
            text: "Dropped contradiction.".to_string(),
            citation: None,
            support: Support::Contradicted,
        },
        Claim {
            text: "Dropped insufficient.".to_string(),
            citation: None,
            support: Support::Insufficient,
        },
    ];
    let gated = gate_answer(&claims).expect("kept sentence");
    assert!(gated.contains("[unverified]"), "{gated}");
    assert!(!gated.contains("Dropped"), "{gated}");
    let empty = gate_answer(&claims[1..]);
    assert!(empty.is_none(), "no supported sentence means evidence-only");
}

#[test]
fn moved_prefilter_keeps_byte_identical_behavior() {
    // Moved verbatim: cited paths inside shown ranges pass, unknown
    // files and out-of-range lines fall back to the abstain template.
    let snippets = vec![(
        "h".to_string(),
        "src/a.rs".to_string(),
        1u32,
        3u32,
        "t".to_string(),
    )];
    assert_eq!(
        constrain_answer_to_evidence("See src/a.rs:2.", &snippets),
        "See src/a.rs:2."
    );
    let fallback = constrain_answer_to_evidence("See src/zzz.rs:9.", &snippets);
    assert!(
        fallback.starts_with("The excerpts do not support"),
        "{fallback}"
    );
    assert!(fallback.contains("src/a.rs:1-3"), "{fallback}");
    let out_of_range = constrain_answer_to_evidence("See src/a.rs:9.", &snippets);
    assert!(
        out_of_range.starts_with("The excerpts do not support"),
        "{out_of_range}"
    );
}
