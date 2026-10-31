use crate::prefix::{split_prefix, CuriePrefix};
use std::str::FromStr;

#[test]
fn known_iri_bases_map_to_closed_variants() {
    assert_eq!(split_prefix("yago:City"), Some((CuriePrefix::Yago, "City")));
    assert_eq!(
        split_prefix("schema:Person"),
        Some((CuriePrefix::Schema, "Person"))
    );
}

#[test]
fn unknown_iri_passes_through_unchanged() {
    assert_eq!(
        crate::normalize::iri_to_curie("http://example.org/Thing"),
        "http://example.org/Thing".to_string()
    );
    assert_eq!(
        crate::normalize::iri_to_curie("http://schema.org/Person"),
        "schema:Person".to_string()
    );
    assert_eq!(
        crate::normalize::iri_to_curie("http://yago-knowledge.org/resource/City_Q515"),
        "yago:City_Q515".to_string()
    );
}

#[test]
fn display_fromstr_round_trip_closed_set() {
    for prefix in [CuriePrefix::Yago, CuriePrefix::Schema] {
        let text = prefix.to_string();
        assert_eq!(CuriePrefix::from_str(&text).unwrap(), prefix);
        assert_eq!(CuriePrefix::from_str(&text.to_uppercase()).unwrap(), prefix);
    }
}

#[test]
fn open_fallback_preserves_original_text() {
    let other = CuriePrefix::from_str("ex").unwrap();
    assert_eq!(other, CuriePrefix::Other("ex".to_string()));
    assert_eq!(other.as_str(), "ex");
    assert_eq!(other.to_string(), "ex");
    assert_eq!(
        split_prefix("ex:Thing"),
        Some((CuriePrefix::Other("ex".to_string()), "Thing"))
    );
    assert_eq!(split_prefix("no-colon-here"), None);
}

#[test]
fn display_output_reparses_to_same_variant() {
    for prefix in [
        CuriePrefix::Yago,
        CuriePrefix::Schema,
        CuriePrefix::Other("custom".to_string()),
    ] {
        let reparsed = CuriePrefix::from_str(&prefix.to_string()).unwrap();
        assert_eq!(reparsed, prefix);
    }
}
