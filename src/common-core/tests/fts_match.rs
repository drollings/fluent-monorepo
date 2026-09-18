//! Golden outputs for [`common_core::string::fts_match_tokens`] and
//! [`common_core::string::fts_quote`]: the shared FTS5 match-expression
//! tokenizer. Contract: Unicode-alphanumeric runs, lowercased, deduped in
//! first-seen order; quoting escapes `"` by doubling. Both recall arms
//! (search-vector fragments, coral nodes) compose these — any refactor must
//! keep every vector byte-identical.

use common_core::string::{fts_match_tokens, fts_quote};

#[test]
fn fts_match_tokens_goldens() {
    let cases: &[(&str, &[&str])] = &[
        ("", &[]),
        ("!!!", &[]),
        ("   ", &[]),
        ("hello", &["hello"]),
        ("Hello WORLD", &["hello", "world"]),
        ("find AlphaSymbol", &["find", "alphasymbol"]),
        ("bug-triage_42", &["bug", "triage", "42"]),
        ("hello hello HELLO", &["hello"]),
        ("café-au-lait", &["café", "au", "lait"]),
        ("naïve naïve", &["naïve"]),
        ("a/b\\c:d", &["a", "b", "c", "d"]),
        ("日本語", &["日本語"]),
        ("report sales Q3", &["report", "sales", "q3"]),
    ];
    for (input, want) in cases {
        assert_eq!(&fts_match_tokens(input), want, "input: {input:?}");
    }
}

#[test]
fn fts_quote_goldens() {
    assert_eq!(fts_quote("hello"), "\"hello\"");
    assert_eq!(fts_quote("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(fts_quote(""), "\"\"");
}
