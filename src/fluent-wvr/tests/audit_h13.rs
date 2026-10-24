//! AUDIT-H13: boundary decoder guarantees.

use fluent_wvr::prelude::*;

#[derive(Default, fluent_wvr::FieldAccess, fluent_wvr::Describable)]
struct Mini {
    action: String,
    count: u32,
}
impl WorkUnit for Mini {
    fn name(&self) -> &str {
        "mini"
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
fluent_wvr::impl_component!(Mini);

#[test]
fn h13_never_panics_on_arbitrary_bytes() {
    let big_a = "a".repeat(10_000);
    let big_brace = "{".repeat(500);
    let inputs = vec![
        "",
        "{",
        "}}}",
        "\u{0}\u{7ff}",
        "{action: 'hi', count: 3, trailing,}",
        "// comment\n{action=\"x\"}",
        "{action: undefined, count: NaN}",
        big_a.as_str(),
        big_brace.as_str(),
    ];
    for raw in inputs {
        let opts = BoundaryOptions::lenient();
        let _ = repair_boundary(raw, &opts);
        let schema_opts = BoundaryOptions::for_schema(Mini::default().field_names());
        let _ = extract_members(raw, &schema_opts);
    }
}

#[test]
fn h13_identity_on_valid_json() {
    let valid = r#"{"action":"hi","count":3}"#;
    let repair = repair_boundary(valid, &BoundaryOptions::lenient());
    // strict parse of repaired must equal strict parse of original
    let a: serde_json::Value = serde_json::from_str(valid).unwrap();
    let b: serde_json::Value = serde_json::from_str(&repair.text).unwrap();
    assert_eq!(a, b, "identity on valid JSON: {}", repair.text);
}

#[test]
fn h13_idempotent() {
    let messy = "{action: 'hi', count: 3,}";
    let once = repair_boundary(messy, &BoundaryOptions::lenient());
    let twice = repair_boundary(&once.text, &BoundaryOptions::lenient());
    assert_eq!(once.text, twice.text, "repair must be idempotent");
}

#[test]
fn h13_lenient_does_not_alter_string_contents_characterization() {
    // Control chars inside strings are escaped (documented), but other contents preserved.
    let raw = r#"{"action":"a,b","count":1}"#;
    let r = repair_boundary(raw, &BoundaryOptions::lenient());
    let v: serde_json::Value = serde_json::from_str(&r.text).unwrap();
    assert_eq!(v["action"], "a,b");
}

#[test]
fn h13_for_schema_ignores_unknown_members() {
    let raw = r#"{action: "hi", count: 3, unknown: "x"}"#;
    let opts = BoundaryOptions::for_schema(Mini::default().field_names());
    let members = extract_members(raw, &opts).unwrap();
    assert!(members.iter().any(|(k, _)| k == "action"));
    assert!(!members.iter().any(|(k, _)| k == "unknown"), "unknown ignored: {members:?}");
    // decode keeps defaults for missing? here both present
    let (mini, decoded) = decode_boundary::<Mini>(raw, &opts).unwrap();
    assert!(decoded.contains(&"action".to_string()));
    // CHARACTERIZATION: member value retains JSON quotes for String fields
    // (no coerce pipeline on this field), so action is "\"hi\"" not "hi".
    assert_eq!(mini.action, "\"hi\"", "characterization: quotes retained");
}
