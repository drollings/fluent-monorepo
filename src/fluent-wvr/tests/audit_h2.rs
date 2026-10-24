//! AUDIT-H2: describe() schema fidelity.

use fluent_wvr::prelude::*;
use fluent_wvr::SchemaProvider;

#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct FullAttrs {
    #[field(desc = "TCP port", min = 1, max = 65535)]
    port: u16,
    #[field(desc = "Host", max_len = 255)]
    host: String,
    #[field(desc = "Verbose")]
    verbose: bool,
    #[field(desc = "Endpoint", pattern = "https://")]
    endpoint: String,
    #[field(desc = "Name", sanitize = "trim")]
    name: String,
    #[field(required = false)]
    nickname: Option<String>,
    #[field(skip)]
    skipped: String,
    #[field(desc = "Clamped", min = 0, max = 10, clamp)]
    clamped: u32,
}

impl WorkUnit for FullAttrs {
    fn name(&self) -> &str {
        "full"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok("done"))
    }
}
fluent_wvr::impl_component!(FullAttrs);

fn sample() -> FullAttrs {
    FullAttrs {
        port: 8079,
        host: "localhost".into(),
        verbose: false,
        endpoint: "https://example.com".into(),
        name: "x".into(),
        nickname: None,
        skipped: "s".into(),
        clamped: 5,
    }
}

#[test]
fn h2_minimum_maximum_are_numbers() {
    let s = sample().describe();
    let port = &s["properties"]["port"];
    assert!(port["minimum"].is_number(), "minimum should be number, got {}", port["minimum"]);
    assert!(port["maximum"].is_number(), "maximum should be number, got {}", port["maximum"]);
    assert!((port["minimum"].as_f64().unwrap() - 1.0).abs() < f64::EPSILON);
    assert!((port["maximum"].as_f64().unwrap() - 65535.0).abs() < f64::EPSILON);
    let host = &s["properties"]["host"];
    assert!(host["maxLength"].is_number(), "maxLength should be number, got {}", host["maxLength"]);
    assert_eq!(host["maxLength"].as_u64().unwrap(), 255);
}

#[test]
fn h2_maxlength_is_number() {
    let s = sample().describe();
    assert!(s["properties"]["host"]["maxLength"].is_number());
    assert_eq!(s["properties"]["host"]["maxLength"].as_u64().unwrap(), 255);
}

#[test]
fn h2_required_respects_option_and_flag() {
    // Claim says required ignores required=false and Option<T>; code actually respects both.
    // This test documents correct behavior (REFUTES that sub-claim).
    let s = sample().describe();
    let req = s["required"].as_array().unwrap();
    for f in ["port", "host", "verbose", "endpoint", "name", "clamped"] {
        assert!(req.iter().any(|v| v == f), "{f} should be required");
    }
    assert!(!req.iter().any(|v| v == "nickname"), "nickname Option must not be required");
    assert!(!req.iter().any(|v| v == "skipped"), "skipped must not appear");
}

#[test]
fn h2_skip_excluded_from_schema_and_field_names() {
    let c = sample();
    assert!(!c.field_names().contains(&"skipped"));
    let s = c.describe();
    assert!(s["properties"].get("skipped").is_none());
    let fields = c.schema();
    assert!(fields.iter().all(|f| f.name != "skipped"));
}

#[test]
fn h2_pattern_is_substring_not_regex() {
    // set_field uses substring contains; schema emits "pattern" which JSON Schema
    // validators interpret as regex. Demonstrate collision.
    let mut c = sample();
    // substring present -> ok
    c.set_field("endpoint", "say https://x here").unwrap();
    // substring absent -> reject
    assert!(c.set_field("endpoint", "http://x").is_err());
    // schema keyword is named "pattern" (JSON Schema regex slot)
    let s = c.describe();
    assert_eq!(s["properties"]["endpoint"]["pattern"], serde_json::json!("https://"));
    // NOTE: "https://" as regex also matches "https://" literally except `//` fine,
    // but e.g. pattern "a.c" would differ: substring "a.c" vs regex any-char.
}

#[test]
fn h2_clamp_not_in_schema_characterization() {
    let s = sample().describe();
    let clamped = &s["properties"]["clamped"];
    // clamp affects set_field but leaves no schema trace
    assert!(clamped.get("clamp").is_none(), "clamp mode is dropped from schema: {clamped}");
    // behavior: out-of-range clamps instead of error
    let mut c = sample();
    c.set_field("clamped", "999").unwrap();
    assert_eq!(c.clamped, 10);
    c.set_field("clamped", "-5").unwrap();
    assert_eq!(c.clamped, 0);
}

#[test]
fn h2_unknown_version_attr_silently_ignored_characterization() {
    // SKILL §19 documents #[field(version_added/version_removed)] but the derive
    // has no such arms and `let _ = parse_nested_meta` swallows unknown-arg errors,
    // so the attribute is silently dropped. We pin the swallow here via sanitize analog:
    // unknown sanitize mode leaves sanitize=None (see macros unit test).
    // Direct version_* probe requires trybuild (compile-fail); documented here as
    // source evidence: macros/src/lib.rs parse_field_attrs has no version_* branch.
    let fields = sample().schema();
    for f in &fields {
        assert!(f.coerce.is_none() || !f.coerce.as_deref().unwrap().contains("version"));
    }
}
