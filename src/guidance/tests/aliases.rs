use super::*;

#[test]
fn test_empty_aliases() {
    assert_eq!(SemanticAliases::new().expand("hello"), vec!["hello"]);
}
#[test]
fn test_single_alias() {
    let mut a = SemanticAliases::new();
    a.insert("hello", vec!["hi".into(), "greetings".into()]);
    let expanded = a.expand("hello");
    assert!(expanded.contains(&"hello".to_string()));
    assert!(expanded.contains(&"hi".to_string()));
}
#[test]
fn test_from_json() {
    let a = SemanticAliases::from_json(r#"{"hello": ["hi", "hey"]}"#).unwrap();
    assert_eq!(a.get("hello").unwrap().len(), 2);
}
#[test]
fn test_expand_query_multi() {
    let mut a = SemanticAliases::new();
    a.insert("fn", vec!["function".into()]);
    a.insert("arg", vec!["argument".into()]);
    assert!(a.expand_query("fn arg").len() >= 2);
}
