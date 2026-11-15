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

#[test]
fn prefilter_accepts_card_backed_citations() {
    // Whole-card citations (verified doc and project cards) name shown
    // evidence with no line range: the path alone binds, so card-backed
    // sentences survive the prefilter instead of abstaining.
    let snippets = vec![(
        "h".to_string(),
        "src/a.rs".to_string(),
        1u32,
        3u32,
        "t".to_string(),
    )];
    let cards = vec!["doc/alpha.md".to_string(), "Cargo.toml".to_string()];
    assert_eq!(
        constrain_answer_to_evidence_with_cards("Alpha syncs (doc/alpha.md:3).", &snippets, &cards),
        "Alpha syncs (doc/alpha.md:3)."
    );
    assert_eq!(
        constrain_answer_to_evidence_with_cards("Ships X and Y (Cargo.toml).", &snippets, &cards),
        "Ships X and Y (Cargo.toml)."
    );
    let unknown = constrain_answer_to_evidence_with_cards("See src/zzz.rs:9.", &snippets, &cards);
    assert!(
        unknown.starts_with("The excerpts do not support"),
        "{unknown}"
    );
}

#[test]
fn gating_keeps_card_backed_claims_labelled() {
    // Card-bound claims gate exactly like snippet-bound ones: kept
    // with the support label, never silently verified.
    let claims = vec![Claim {
        text: "Ships X and Y (Cargo.toml).".to_string(),
        citation: Some("Cargo.toml".to_string()),
        support: Support::Unverified,
    }];
    let gated = gate_answer(&claims).expect("card-backed claims are kept");
    assert!(gated.contains("[unverified]"), "{gated}");
}
