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
    let manifest = vec![
        ("[C1]".to_string(), "src/a.rs:1".to_string()),
        ("[C2]".to_string(), "src/b.rs:9".to_string()),
    ];
    let claims = bind_claims("Alpha adds one (src/a.rs:1-3). Baking is nice.", &manifest);
    assert_eq!(claims.len(), 2);
    assert!(claims[0].citation.is_some());
    assert!(claims[1].citation.is_none());
    assert!(claims
        .iter()
        .all(|claim| claim.support == Support::Unverified));
}

#[test]
fn claims_resolve_evidence_tokens_to_canonical_citations() {
    let manifest = vec![
        ("[C1]".to_string(), "src/a.rs:1-3".to_string()),
        ("[D1]".to_string(), "doc/alpha.md#notes".to_string()),
    ];
    let claims = bind_claims(
        "Alpha adds one [C1]. Notes say so [D1]. Invented claims [C9].",
        &manifest,
    );
    assert_eq!(claims.len(), 3);
    assert_eq!(claims[0].citation.as_deref(), Some("src/a.rs:1-3"));
    assert_eq!(claims[1].citation.as_deref(), Some("doc/alpha.md#notes"));
    assert!(claims[2].citation.is_none(), "unknown token binds nothing");
    let gated = gate_answer(&claims).expect("token-bound claims are kept");
    assert!(
        !gated.contains("[C9]"),
        "unbound sentences never render: {gated}"
    );
    assert!(
        gated.contains("src/a.rs:1-3") || gated.contains("Alpha adds one"),
        "{gated}"
    );
}

#[test]
fn invented_paths_drop_through_the_gate() {
    let manifest = vec![("[C1]".to_string(), "src/a.rs:1-3".to_string())];
    let claims = bind_claims("Lives in invented.rs:99.", &manifest);
    assert!(claims.iter().all(|claim| claim.citation.is_none()));
    assert!(
        gate_answer(&claims).is_none(),
        "uncited prose renders nothing"
    );
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

#[test]
fn prefilter_marks_identifier_free_claims_insufficient() {
    // Deterministic entailment prefilter: the cited range must share
    // an identifier with the claim, else no model call is spent.
    let manifest = vec![("[C1]".to_string(), "src/a.rs:1-3".to_string())];
    let evidence = vec![(
        "src/a.rs:1-3".to_string(),
        "pub fn alpha() {}\nadds one".to_string(),
    )];
    let mut claims = bind_claims("Alpha adds one [C1]. Baking is nice.", &manifest);
    assert_eq!(claims.len(), 2);
    prefilter_support(&mut claims, &evidence);
    assert_eq!(
        claims[0].support,
        Support::Unverified,
        "shared identifier survives"
    );
    assert_eq!(
        claims[1].support,
        Support::Unverified,
        "unbound claims pass through for the gate to drop"
    );
    let mut claims = bind_claims("Baking is nice [C1].", &manifest);
    prefilter_support(&mut claims, &evidence);
    assert_eq!(
        claims[0].support,
        Support::Insufficient,
        "no shared identifier means no model call"
    );
    assert!(
        gate_answer(&claims).is_none(),
        "prefiltered claims never render"
    );
}
