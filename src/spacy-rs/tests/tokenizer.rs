use super::*;
use crate::lang::en;

fn en_tokenizer() -> Tokenizer {
    let vocab = Arc::new(Vocab::new(en::lexicon_config()));
    en::tokenizer(vocab).expect("english tokenizer builds")
}

fn orths(doc: &Doc) -> Vec<String> {
    let strings = doc.vocab().strings();
    doc.tokens()
        .iter()
        .map(|t| t.lexeme.orth_text(strings))
        .collect()
}

#[test]
fn tokenizes_simple_sentence() {
    let t = en_tokenizer();
    let doc = t.tokenize("Hello world").expect("tokenizes");
    assert_eq!(orths(&doc), ["Hello", "world"]);
    assert_eq!(doc.tokens()[0].idx, 0);
    assert_eq!(doc.tokens()[1].idx, 6);
    assert!(doc.tokens()[0].spacy);
    assert!(!doc.tokens()[1].spacy);
    assert_eq!(doc.tokens()[0].sent_start, crate::doc::SentStart::Start);
}

#[test]
fn multiple_spaces_make_a_space_token() {
    let t = en_tokenizer();
    let doc = t.tokenize("a  b").expect("tokenizes");
    assert_eq!(orths(&doc), ["a", " ", "b"]);
    assert!(doc.tokens()[0].spacy);
}

#[test]
fn contraction_special_case() {
    let t = en_tokenizer();
    let doc = t.tokenize("I can't go").expect("tokenizes");
    assert_eq!(orths(&doc), ["I", "ca", "n't", "go"]);
    let nnt = &doc.tokens()[2];
    assert_eq!(nnt.norm, hash_utf8("not"), "special-case NORM override");
}

#[test]
fn hyphen_infix_splits() {
    let t = en_tokenizer();
    let doc = t.tokenize("dyn-o-mite").expect("tokenizes");
    assert_eq!(orths(&doc), ["dyn", "-", "o", "-", "mite"]);
}

#[test]
fn hyphen_dash_run_stays_one_token() {
    let t = en_tokenizer();
    let doc = t.tokenize("foo---bar").expect("tokenizes");
    assert_eq!(orths(&doc), ["foo", "---", "bar"]);
}

#[test]
fn leading_space_is_tokenized() {
    let t = en_tokenizer();
    let doc = t.tokenize(" hello").expect("tokenizes");
    assert_eq!(orths(&doc), [" ", "hello"]);
}

#[test]
fn trailing_single_space_is_absorbed() {
    let t = en_tokenizer();
    let doc = t.tokenize("hello ").expect("tokenizes");
    assert_eq!(orths(&doc), ["hello"]);
    assert!(doc.tokens()[0].spacy);
}

#[test]
fn newline_is_a_token() {
    let t = en_tokenizer();
    let doc = t.tokenize("hello\nworld").expect("tokenizes");
    assert_eq!(orths(&doc), ["hello", "\n", "world"]);
    assert!(!doc.tokens()[0].spacy, "newline is not absorbed as spacy");
    assert!(!doc.tokens()[1].spacy);
    assert!(!doc.tokens()[2].spacy);
    assert_eq!(doc.tokens()[1].idx, 5);
    assert_eq!(doc.tokens()[2].idx, 6);
}

#[test]
fn multi_token_matcher_special_case() {
    let t = en_tokenizer();
    let doc = t.tokenize("y'all are great").expect("tokenizes");
    assert_eq!(orths(&doc), ["y'", "all", "are", "great"]);
    assert_eq!(doc.tokens()[1].idx, 2, "in-span idx recomputed");
    assert_eq!(doc.tokens()[2].idx, 6, "post-span idx preserved");
}

#[test]
fn times_abbreviation_special_case() {
    let t = en_tokenizer();
    let doc = t.tokenize("at 5a.m. sharp").expect("tokenizes");
    assert_eq!(orths(&doc), ["at", "5", "a.m.", "sharp"]);
}

#[test]
fn empty_and_whitespace_text() {
    let t = en_tokenizer();
    assert_eq!(t.tokenize("").expect("empty").len(), 0);
    let doc = t.tokenize("   ").expect("spaces");
    assert_eq!(orths(&doc), ["   "]);
    let doc = t.tokenize("  a   ").expect("wrapped spaces");
    assert_eq!(orths(&doc), ["  ", "a", "  "]);
    assert!(doc.tokens()[1].spacy);
}

#[test]
fn filter_special_spans_prefers_longest() {
    // Overlapping spans where "cannot" (0,2) is longer than "can" (0,1):
    // longest-first resolution keeps the longer span and drops the covered
    // one. A same-span tie keeps the first-listed rule.
    let rule_long = Arc::new(SpecialRule {
        key: "cannot".into(),
        tokens: vec![],
        phrase: vec![hash_utf8("cannot")],
    });
    let rule_short = Arc::new(SpecialRule {
        key: "can".into(),
        tokens: vec![],
        phrase: vec![hash_utf8("can")],
    });
    let matches = vec![
        (Arc::clone(&rule_short), 0, 1),
        (Arc::clone(&rule_long), 0, 2),
    ];
    let filtered = filter_special_spans(matches);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].0.key, "cannot");
    assert_eq!((filtered[0].1, filtered[0].2), (0, 2));
}

/// Rule tables freeze at build: post-freeze registration is refused and the
/// decided output is unchanged — the tokenizer table is append-at-build,
/// never last-wins at runtime (rule genesis rebuilds the tokenizer).
#[test]
fn post_freeze_registration_is_refused_and_output_unchanged() {
    let t = en_tokenizer();
    let before = orths(&t.tokenize("gonna").expect("tokenizes"));
    let err = t
        .add_special_case("gonna", &[("gon", None), ("na", None)])
        .expect_err("post-freeze registration must fail");
    assert!(
        err.to_string().contains("frozen"),
        "error names the freeze: {err}"
    );
    assert_eq!(
        orths(&t.tokenize("gonna").expect("tokenizes")),
        before,
        "refused registration changes nothing"
    );
}

// ── Special-case mode golden pins ──
//
// `tokenize_affixes(text, bool)` is driven directly (child-module access to
// the private span path) to pin both legs before the bool becomes a mode:
// on plain affix text the legs agree exactly; on special-case text only the
// enabled leg expands; `faster_heuristics` off keeps public output identical.

fn affixes_with(text: &str, mode: SpecialCaseMode) -> Vec<String> {
    let t = en_tokenizer();
    orths(
        &t.tokenize_affixes(text, mode)
            .expect("affix pass tokenizes"),
    )
}

#[ignore = "heavy (~7s): eight English tokenizer builds per run; run via `make test-heavy`"]
#[test]
fn affix_text_agrees_with_specials_on_and_off() {
    for text in ["dyn-o-mite", "hello!", "well-known fact", "(parenthetical)"] {
        assert_eq!(
            affixes_with(text, SpecialCaseMode::Enabled),
            affixes_with(text, SpecialCaseMode::Disabled),
            "no specials present, so the flag is a no-op on {text:?}"
        );
    }
    assert_eq!(
        affixes_with("dyn-o-mite", SpecialCaseMode::Enabled),
        ["dyn", "-", "o", "-", "mite"]
    );
}

#[ignore = "heavy (~2s): paired English tokenizer builds; run via `make test-heavy`"]
#[test]
fn special_text_expands_only_with_specials_on() {
    assert_eq!(
        affixes_with("can't", SpecialCaseMode::Enabled),
        ["ca", "n't"]
    );
    let off = affixes_with("can't", SpecialCaseMode::Disabled);
    assert_ne!(off, ["ca", "n't"], "disabled leg skips the specials table");
    assert_eq!(off.join("").replace(' ', ""), "can't", "lossless split");
}

#[ignore = "heavy (~5s): six English tokenizer builds per run; run via `make test-heavy`"]
#[test]
fn whitespace_paths_agree_with_specials_on_and_off() {
    for text in ["a  b", " hello ", "tab\tseparated"] {
        assert_eq!(
            affixes_with(text, SpecialCaseMode::Enabled),
            affixes_with(text, SpecialCaseMode::Disabled),
            "whitespace handling never consults specials on {text:?}"
        );
    }
}

fn en_tokenizer_no_heuristics() -> Tokenizer {
    let vocab = Arc::new(Vocab::new(en::lexicon_config()));
    let config = TokenizerConfig {
        prefix_pattern: Some(en::patterns::PREFIX_PATTERN.to_string()),
        suffix_pattern: Some(en::patterns::SUFFIX_PATTERN.to_string()),
        infix_pattern: Some(en::patterns::INFIX_PATTERN.to_string()),
        token_match: None,
        url_match: Some(crate::lang::url::URL_PATTERN.to_string()),
        faster_heuristics: false,
        max_cache_size: 10_000,
    };
    Tokenizer::new(vocab, &config, en::exceptions::RULES).expect("builds")
}

#[ignore = "heavy (~2.4s): full fast plus heuristic-free tokenizer builds over the corpus; run via `make test-heavy`"]
#[test]
fn faster_heuristics_off_keeps_public_output() {
    let fast = en_tokenizer();
    let slow = en_tokenizer_no_heuristics();
    for text in [
        "I can't go",
        "dyn-o-mite",
        "y'all are great",
        "at 5a.m. sharp",
        "Hello world",
    ] {
        assert_eq!(
            orths(&slow.tokenize(text).expect("tokenizes")),
            orths(&fast.tokenize(text).expect("tokenizes")),
            "heuristics flag must not change tokens on {text:?}"
        );
    }
}
