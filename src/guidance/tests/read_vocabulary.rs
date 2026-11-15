//! Single-vocabulary contracts: term extraction, alias expansion,
//! light-verb removal, symbol extraction, deterministic ordering.
//! Included via `#[path]` forwarder from `read/vocabulary.rs`, per
//! repo convention. Pure: no I/O, no model.

use super::*;
use search_vector::aliases::SemanticAliases;

#[test]
fn query_terms_match_the_shared_funnel_rule() {
    assert_eq!(query_terms(""), Vec::<String>::new());
    assert_eq!(
        query_terms("What are the major components"),
        vec!["major".to_string(), "components".to_string()]
    );
    // Length floor and stopwords agree with the funnel exactly.
    assert_eq!(
        query_terms("how does guidance sync decide"),
        crate::query::funnel::significant_terms("how does guidance sync decide")
    );
}

#[test]
fn light_verbs_leave_unless_they_are_all_that_remains() {
    assert_eq!(
        content_terms("how does the router decide where to rank code hits"),
        vec![
            "router".to_string(),
            "rank".to_string(),
            "code".to_string(),
            "hits".to_string()
        ]
    );
    // Only verbs: dropping everything answers nothing, so they stay.
    assert_eq!(
        content_terms("decide how to run"),
        vec!["decide".to_string(), "run".to_string()]
    );
}

#[test]
fn alias_expansion_is_first_seen_ordered() {
    let json = r#"{"hits": ["strike"], "code": ["source"]}"#;
    let aliases = SemanticAliases::from_json(json).expect("aliases");
    assert_eq!(
        expand_terms("code hits", &aliases),
        vec![
            "code".to_string(),
            "source".to_string(),
            "hits".to_string(),
            "strike".to_string()
        ]
    );
    // Repository-neutral default: no aliases, terms pass through.
    assert_eq!(
        expand_terms("code hits", &SemanticAliases::new()),
        vec!["code".to_string(), "hits".to_string()]
    );
}

#[test]
fn symbol_terms_come_from_significant_terms_only() {
    // The shared tokenizer splits snake_case, so multi-part
    // identifiers arrive as parts — identical to the funnel's
    // `symbols` stage, never a second rule.
    assert_eq!(
        symbol_terms("where is clean_build_cache"),
        vec![
            "clean".to_string(),
            "build".to_string(),
            "cache".to_string()
        ]
    );
    // Scaffolding never reaches extraction: `what`/`is`/`the` drop out
    // in the term stage, so only content words are candidates.
    assert_eq!(
        symbol_terms("what is the router dispatch table"),
        vec![
            "router".to_string(),
            "dispatch".to_string(),
            "table".to_string()
        ]
    );
    assert_eq!(symbol_terms("the a an"), Vec::<String>::new());
}

#[test]
fn ordering_is_first_seen_deterministic() {
    assert_eq!(
        expand_terms("beta alpha beta", &SemanticAliases::new()),
        vec!["beta".to_string(), "alpha".to_string()]
    );
}
