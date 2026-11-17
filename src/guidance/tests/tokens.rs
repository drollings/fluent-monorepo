use super::*;

#[test]
fn lower_dedup_push_collapses_case_variants_and_empties() {
    let mut out = Vec::new();
    lower_dedup_push(&mut out, "Find");
    lower_dedup_push(&mut out, "find");
    lower_dedup_push(&mut out, "");
    lower_dedup_push(&mut out, "gamma");
    assert_eq!(out, vec!["find".to_string(), "gamma".to_string()]);
}

#[test]
fn split_tokens_edges() {
    assert!(split_tokens("").is_empty());
    assert_eq!(
        split_tokens("Foo.Bar"),
        vec!["foo".to_string(), "bar".to_string()]
    );
    // Underscore is not alphanumeric: it splits too.
    assert_eq!(
        split_tokens("foo_bar"),
        vec!["foo".to_string(), "bar".to_string()]
    );
    assert_eq!(
        split_tokens("a::b c-d"),
        vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string()
        ]
    );
    // Duplicates preserved; empties dropped.
    assert_eq!(split_tokens("x..x"), vec!["x".to_string(), "x".to_string()]);
}

#[test]
fn symbol_names_edges() {
    assert!(symbol_names("").is_empty());
    assert_eq!(
        symbol_names("foo bar foo"),
        vec!["foo".to_string(), "bar".to_string()]
    );
    assert_eq!(symbol_names("~temp"), vec!["~temp".to_string()]);
    assert!(symbol_names("a:b:c").is_empty());
    assert_eq!(symbol_names("café"), vec!["caf".to_string()]);
}

#[test]
fn symbol_name_from_token_edges() {
    assert_eq!(symbol_name_from_token("foo::"), Some("foo".to_string()));
    assert_eq!(symbol_name_from_token("::foo"), Some("foo".to_string()));
    assert_eq!(
        symbol_name_from_token("Foo::Bar::baz"),
        Some("Bar::baz".to_string())
    );
    assert_eq!(symbol_name_from_token(""), None);
    assert_eq!(symbol_name_from_token("::"), None);
}
