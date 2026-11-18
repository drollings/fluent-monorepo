use super::*;

#[test]
fn test_hash_iri_deterministic() {
    let iri = "http://example.org/foo";
    assert_eq!(hash_iri(iri), hash_iri(iri));
}

#[test]
fn test_hash_iri_different() {
    assert_ne!(
        hash_iri("http://example.org/foo"),
        hash_iri("http://example.org/bar")
    );
}

/// Cross-crate parity: `interlingua`'s private `hash_iri` copy was
/// deduplicated onto the same `common_core::hash::blake3_hash` kernel,
/// so both homes must agree bit-for-bit (compared through the public
/// 48-bit id surface, the only shared observable).
#[test]
fn test_hash_iri_matches_types_kernel() {
    for iri in [
        "http://schema.org/Person",
        "http://example.org/foo",
        "urn:blank:abc123",
        "",
    ] {
        assert_eq!(
            fluent_types::yago_class_id_for_iri(iri).local_id(),
            fluent_types::local_id_of(hash_iri(iri)),
            "kernel parity for {iri:?}"
        );
    }
}

#[test]
fn test_hash_blank_node_deterministic() {
    assert_eq!(
        hash_blank_node("scope1", "b1"),
        hash_blank_node("scope1", "b1")
    );
}

#[test]
fn test_hash_blank_node_different_scopes() {
    assert_ne!(
        hash_blank_node("scope1", "b1"),
        hash_blank_node("scope2", "b1")
    );
}

#[test]
fn test_normalize_integer() {
    let tv = normalize_literal("42", None, Some(&format!("{NS_XSD}integer")));
    assert_eq!(tv, TypedValue::Integer(42));
}

#[test]
fn test_normalize_decimal() {
    let tv = normalize_literal("3.14", None, Some(&format!("{NS_XSD}decimal")));
    assert!(matches!(tv, TypedValue::Double(_)));
}

#[test]
fn test_normalize_boolean_true() {
    let tv = normalize_literal("true", None, Some(&format!("{NS_XSD}boolean")));
    assert_eq!(tv, TypedValue::Boolean(true));
}

#[test]
fn test_normalize_boolean_false() {
    let tv = normalize_literal("false", None, Some(&format!("{NS_XSD}boolean")));
    assert_eq!(tv, TypedValue::Boolean(false));
}

#[test]
fn test_normalize_lang_string() {
    let tv = normalize_literal("bonjour", Some("fr"), None);
    assert_eq!(tv, TypedValue::LangString);
}

#[test]
fn test_normalize_plain_string() {
    let tv = normalize_literal("hello", None, None);
    assert_eq!(tv, TypedValue::String);
}

#[test]
fn test_detect_xsd_type_parity() {
    // Every historical datatype maps exactly as before (zero-alloc now).
    let xsd = NS_XSD;
    let cases = [
        (None, XsdType::String),
        (Some(format!("{xsd}string")), XsdType::String),
        (Some(format!("{xsd}integer")), XsdType::Integer),
        (Some(format!("{xsd}int")), XsdType::Integer),
        (Some(format!("{xsd}long")), XsdType::Integer),
        (Some(format!("{xsd}short")), XsdType::Integer),
        (Some(format!("{xsd}decimal")), XsdType::Decimal),
        (Some(format!("{xsd}float")), XsdType::Double),
        (Some(format!("{xsd}double")), XsdType::Double),
        (Some(format!("{xsd}boolean")), XsdType::Boolean),
        (Some(format!("{xsd}dateTime")), XsdType::DateTime),
        (Some(format!("{xsd}date")), XsdType::DateTime),
        (Some(format!("{xsd}bogus")), XsdType::Other),
        (
            Some("http://other.example/ns#integer".to_string()),
            XsdType::Other,
        ),
        (Some(xsd.to_string()), XsdType::Other),
    ];
    for (dt, expected) in cases {
        assert_eq!(detect_xsd_type(dt.as_deref()), expected, "{dt:?}");
    }
}

#[test]
fn test_normalize_datetime_golden() {
    let dt = |v: &str| normalize_literal(v, None, Some(&format!("{NS_XSD}dateTime")));
    // 1970-01-01T00:00:00Z is the epoch itself (proves we compute, not stub).
    assert_eq!(dt("1970-01-01T00:00:00Z"), TypedValue::DateTime(0));
    assert_eq!(dt("1970-01-01T00:00:01Z"), TypedValue::DateTime(1));
    // Known reference: 2024-02-29 (leap day) at noon UTC.
    assert_eq!(dt("2024-02-29T12:00:00Z"), TypedValue::DateTime(1709208000));
    // Offsets shift the instant: +02:00 means two hours earlier in UTC.
    assert_eq!(
        dt("2024-02-29T12:00:00+02:00"),
        TypedValue::DateTime(1709208000 - 7200)
    );
    // A missing zone means UTC; fractions do not move the second.
    assert_eq!(dt("2024-02-29T12:00:00"), TypedValue::DateTime(1709208000));
    assert_eq!(
        dt("2024-02-29T12:00:00.999Z"),
        TypedValue::DateTime(1709208000)
    );
    // A bare date means midnight UTC.
    assert_eq!(dt("2024-02-29"), TypedValue::DateTime(1709164800));
}

#[test]
fn test_normalize_datetime_invalid_is_other() {
    let dt = |v: &str| normalize_literal(v, None, Some(&format!("{NS_XSD}dateTime")));
    // Fail closed: never the silent epoch 0, never a guess.
    for bad in [
        "not a date",
        "2024-13-01T00:00:00Z",
        "2023-02-29T00:00:00Z",
        "2024-01-01T25:00:00Z",
        "2024-01-01T",
        "2024-1-1",
        "",
    ] {
        assert_eq!(dt(bad), TypedValue::Other, "{bad:?}");
    }
    // Non-dateTime literals normalize exactly as before (control group).
    assert_eq!(
        normalize_literal("42", None, Some(&format!("{NS_XSD}integer"))),
        TypedValue::Integer(42)
    );
    assert_eq!(
        normalize_literal("nope", None, Some(&format!("{NS_XSD}integer"))),
        TypedValue::Other
    );
    assert_eq!(
        normalize_literal("true", None, Some(&format!("{NS_XSD}boolean"))),
        TypedValue::Boolean(true)
    );
    assert_eq!(normalize_literal("hello", None, None), TypedValue::String);
    assert_eq!(
        normalize_literal("x", None, Some(&format!("{NS_XSD}bogus"))),
        TypedValue::Other
    );
}
