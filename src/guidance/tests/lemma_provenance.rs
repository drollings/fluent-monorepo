use crate::query::ingest::query_lemmas;

/// Test-only classification of where a lemma token was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LemmaProvenance {
    /// Token appears in comment or prose text.
    CommentOrProse,
    /// Token appears only in code body text.
    BodyOnly,
    /// Token appears in neither input.
    Absent,
}

/// Classify one token against comment/prose text and body text.
#[must_use]
pub fn provenance_of(token: &str, comment_text: &str, body_text: &str) -> LemmaProvenance {
    let needle = token.to_lowercase();
    let in_comment = comment_text
        .split(|c: char| !c.is_alphanumeric())
        .any(|part| part.eq_ignore_ascii_case(&needle));
    if in_comment {
        return LemmaProvenance::CommentOrProse;
    }
    let in_body = body_text
        .split(|c: char| !c.is_alphanumeric())
        .any(|part| part.eq_ignore_ascii_case(&needle));
    if in_body {
        return LemmaProvenance::BodyOnly;
    }
    LemmaProvenance::Absent
}

#[test]
fn baseline_lemmas_include_body_identifiers() {
    // Current ingestion lemmatizes whole-file text, so identifiers that
    // only occur in the body still produce lemma rows today.
    let lemmas = query_lemmas(None, "export function AlphaSymbol() { return 1; }");
    assert!(
        lemmas.contains(&"alphasymbol".to_string()),
        "baseline keeps body identifiers: {lemmas:?}"
    );
    assert!(
        lemmas.contains(&"export".to_string()),
        "baseline keeps keywords adjacent to bodies: {lemmas:?}"
    );
}

#[test]
fn provenance_classifier_separates_comment_from_body() {
    let comment = "Adds one to the total.";
    let body = "pub fn add(value: i32) -> i32 { value + 1 }";
    assert_eq!(
        provenance_of("adds", comment, body),
        LemmaProvenance::CommentOrProse
    );
    assert_eq!(
        provenance_of("value", comment, body),
        LemmaProvenance::BodyOnly
    );
    assert_eq!(
        provenance_of("missing", comment, body),
        LemmaProvenance::Absent
    );
}

#[test]
fn provenance_classifier_matches_lemma_normalization() {
    // Lowercase alphanumeric matching mirrors the lemma filter.
    assert_eq!(
        provenance_of("HELLO", "hello world", "fn main() {}"),
        LemmaProvenance::CommentOrProse
    );
    assert_eq!(
        provenance_of("main", "hello world", "fn main() {}"),
        LemmaProvenance::BodyOnly
    );
}

fn scope_block(text: &str) -> crate::ast_parser::CommentBlock {
    crate::ast_parser::CommentBlock {
        node_key: "test".to_string(),
        node_kind: "function_item".to_string(),
        name: "test".to_string(),
        scope: String::new(),
        start_byte: 0,
        end_byte: text.len(),
        start_line: 1,
        end_line: 1,
        text: text.to_string(),
        comment_hash: common_core::hash::blake3_hex(text.as_bytes()),
    }
}

#[test]
fn every_comment_lemma_occurs_in_comment_text() {
    let comment = "Adds one to the total; retry the operation.";
    let body = "pub fn unrelated_body_token_xyz() {}";
    let blocks = [scope_block(comment)];
    let lemmas = crate::query::ingest::lemmas_for_comment_blocks(&blocks, None);
    assert!(!lemmas.is_empty(), "comment must yield lemmas");
    for lemma in &lemmas {
        assert_eq!(
            provenance_of(lemma, comment, body),
            LemmaProvenance::CommentOrProse,
            "lemma {lemma:?} must occur in comment text"
        );
    }
}
