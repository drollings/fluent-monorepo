//! Schema edges: `pattern`/`clamp` keyword contract and lossless bound raws.
//!
//! `pattern` stays substring-enforced under its JSON Schema `pattern*` slot
//! (documented, pinned); `clamp` is enforced in `set_field` and leaves an
//! `x-clamp` trace in the schema (exactly one annotation); `version_*`
//! stays loudly refused. `FieldSchema` carries the exact decimal spellings
//! (`min_raw`/`max_raw`) alongside the legacy lossy `f64` bounds.

use fluent_wvr::prelude::*;
use fluent_wvr::SchemaProvider;

fn variant_ids() -> Vec<String> {
    vec!["a".to_string()]
}

#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct KitchenSink {
    #[field(desc = "TCP port", min = 1, max = 65535)]
    port: u16,
    #[field(desc = "Clamped dial", min = 0, max = 10, clamp)]
    clamped: u32,
    #[field(desc = "Host", max_len = 64)]
    host: String,
    #[field(desc = "Name", sanitize = "trim")]
    name: String,
    #[field(desc = "Endpoint", pattern = "https://")]
    endpoint: String,
    #[field(desc = "Mode", coerce = "trim, strip_quotes")]
    mode: String,
    #[field(desc = "Ratio", parse = "number")]
    ratio: f64,
    #[field(desc = "Kind", variants = "variant_ids")]
    kind: String,
    #[field(desc = "URL", format = "url")]
    url: String,
    #[field(required = false)]
    nickname: Option<String>,
    #[field(required = false, empty_is_none = false)]
    email: Option<String>,
    #[field(skip)]
    internal: u64,
}

fn sink() -> KitchenSink {
    KitchenSink {
        port: 8079,
        clamped: 5,
        host: "localhost".into(),
        name: "x".into(),
        endpoint: "https://example.com".into(),
        mode: "fast".into(),
        ratio: 0.5,
        kind: "a".into(),
        url: "https://example.com".into(),
        nickname: None,
        email: None,
        internal: 0,
    }
}

/// M7.t1: every emitted `minimum`/`maximum`/`maxLength` is a JSON number;
/// every `required` entry names a non-`skip` field; `Option<T>` and explicit
/// `required = false` never appear in `required`.
#[test]
fn meta_schema_shape_is_numeric_and_required_is_exact() {
    let schema = sink().describe();
    let properties = schema["properties"].as_object().expect("properties");
    for (name, property) in properties {
        for keyword in ["minimum", "maximum", "maxLength"] {
            if let Some(bound) = property.get(keyword) {
                assert!(
                    bound.is_number(),
                    "{keyword} of {name} must be a JSON number, got {bound}"
                );
            }
        }
    }
    assert!(schema["properties"]["port"]["minimum"].is_number());
    assert!(schema["properties"]["port"]["maximum"].is_number());
    assert!(schema["properties"]["host"]["maxLength"].is_number());
    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("required")
        .iter()
        .map(|v| v.as_str().expect("required entries are strings"))
        .collect();
    for name in &required {
        assert!(
            sink().field_names().contains(name),
            "{name} in required must name a non-skip field"
        );
    }
    assert!(!required.contains(&"nickname"));
    assert!(!required.contains(&"email"));
    assert!(!required.contains(&"internal"));
    let fields = sink().schema();
    assert!(fields.iter().all(|f| f.name != "internal"));
}

/// M7.t2: `pattern` round-trips as the enforced substring, and the schema
/// documents it under the same `pattern` slot.
#[test]
fn pattern_contract_is_substring_in_schema_and_enforcement() {
    let mut sink = sink();
    sink.set_field("endpoint", "say https://x here")
        .expect("contains");
    assert!(sink.set_field("endpoint", "http://x").is_err());
    let schema = sink.describe();
    assert_eq!(
        schema["properties"]["endpoint"]["pattern"],
        serde_json::json!("https://")
    );
}

/// M7.t2: `clamp` fields carry exactly one schema annotation — `x-clamp`
/// with the bounds — and enforcement still clamps instead of erroring.
#[test]
fn clamp_contract_is_single_x_clamp_annotation() {
    let schema = sink().describe();
    let clamped = &schema["properties"]["clamped"];
    let annotation = clamped.get("x-clamp").expect("x-clamp trace");
    assert_eq!(annotation, &serde_json::json!([0.0, 10.0]));
    // Exactly one clamp trace: no parallel `clamp` keyword beside it.
    assert!(
        clamped.get("clamp").is_none(),
        "single annotation only: {clamped}"
    );
    let mut sink = sink();
    sink.set_field("clamped", "999")
        .expect("clamps, not errors");
    assert_eq!(sink.clamped, 10);
    sink.set_field("clamped", "-5").expect("clamps, not errors");
    assert_eq!(sink.clamped, 0);
}

#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct EdgeBounds {
    #[field(desc = "full u64 range", min = 0, max = 18446744073709551615)]
    big: u64,
    #[field(desc = "full i64 range", min = -9223372036854775808, max = 9223372036854775807)]
    small: i64,
}

/// M7.t3: the raw bound strings name the integer edges exactly while the
/// legacy `f64` fields document their precision limit (within one ulp —
/// they saturate to the edge rather than naming a neighbor).
#[test]
fn raw_bounds_name_integer_edges_exactly() {
    let fields = EdgeBounds { big: 0, small: 0 }.schema();
    let big = fields.iter().find(|f| f.name == "big").expect("big");
    assert_eq!(big.min_raw.as_deref(), Some("0"));
    assert_eq!(big.max_raw.as_deref(), Some("18446744073709551615"));
    assert_eq!(
        big.max_raw
            .as_deref()
            .unwrap()
            .parse::<u64>()
            .expect("exact"),
        u64::MAX
    );
    assert!(
        (big.max.expect("f64") as u64).wrapping_sub(u64::MAX) <= 1,
        "legacy f64 saturates to the edge: {}",
        big.max.expect("f64")
    );
    let small = fields.iter().find(|f| f.name == "small").expect("small");
    assert_eq!(small.min_raw.as_deref(), Some("-9223372036854775808"));
    assert_eq!(small.max_raw.as_deref(), Some("9223372036854775807"));
    assert_eq!(
        small
            .min_raw
            .as_deref()
            .unwrap()
            .parse::<i64>()
            .expect("exact"),
        i64::MIN
    );
    assert_eq!(
        small
            .max_raw
            .as_deref()
            .unwrap()
            .parse::<i64>()
            .expect("exact"),
        i64::MAX
    );
    assert_eq!(small.min.expect("f64") as i64, i64::MIN);
}

/// M7.t3 control: the edges themselves are accepted at runtime.
#[test]
fn edge_values_are_accepted() {
    let mut edges = EdgeBounds { big: 0, small: 0 };
    edges
        .set_field("big", "18446744073709551615")
        .expect("u64::MAX accepted");
    assert_eq!(edges.big, u64::MAX);
    edges
        .set_field("small", "-9223372036854775808")
        .expect("i64::MIN accepted");
    assert_eq!(edges.small, i64::MIN);
}
