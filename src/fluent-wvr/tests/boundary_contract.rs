//! Boundary contract: the guarantees `repair_boundary` / `extract_members` /
//! `decode_boundary` publish over adversarial input.
//!
//! - Identity on valid JSON, idempotence, no-panic on arbitrary bytes,
//!   string-content preservation, unknown-member handling, bounded
//!   work/stack on hostile input (properties, `proptest`).
//! - No key smuggling past `set_field` validation; failed members keep
//!   defaults and stay out of the decoded-names list.
//! - Quote table: bare `String` without `coerce` keeps quotes (pinned spec);
//!   `coerce = "trim, strip_quotes"` strips; `json_escape` round-trips.

use fluent_wvr::prelude::*;
use proptest::prelude::*;
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Default, fluent_wvr::FieldAccess, fluent_wvr::Describable)]
struct Contract {
    #[field(desc = "bare action, no coerce pipeline")]
    action: String,
    #[field(desc = "stripped mode", coerce = "trim, strip_quotes")]
    mode: String,
    #[field(desc = "escaped note", coerce = "json_escape")]
    note: String,
    #[field(desc = "bounded port", min = 1, max = 65535)]
    port: u16,
}

fn contract_opts() -> BoundaryOptions<'static> {
    BoundaryOptions::for_schema(Contract::default().field_names())
}

/// A JSON value generator capped in depth and breadth: the JSON-shaped
/// subset the identity property ranges over. Floats are finite so they
/// serialize; `serde_json` may emit exponents (`1e21`) — the walker must
/// still pass them through verbatim.
fn arb_json() -> impl Strategy<Value = serde_json::Value> {
    // No raw control characters: the documented contract escapes those
    // (preserving the parsed value but not the exact bytes), so text
    // identity ranges over the control-free subset. Value-level preservation
    // across the whole Unicode space is pinned by
    // `string_contents_preserved`.
    let text = ".{0,32}".prop_filter("no raw controls", |s: &String| {
        !s.chars().any(char::is_control)
    });
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::from),
        any::<i64>().prop_map(serde_json::Value::from),
        any::<u64>().prop_map(serde_json::Value::from),
        any::<f64>()
            .prop_filter("finite", |f: &f64| f.is_finite())
            .prop_map(|f| serde_json::Number::from_f64(f)
                .map_or(serde_json::Value::Null, serde_json::Value::from)),
        text.prop_map(serde_json::Value::from),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::from),
            prop::collection::hash_map("[a-z]{1,8}", inner, 0..4)
                .prop_map(|m| m.into_iter().collect::<serde_json::Value>()),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (a) The walker never panics on arbitrary bytes, arbitrary UTF-8, or
    /// unterminated string shapes — in any mode.
    #[test]
    fn never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let raw = String::from_utf8_lossy(&bytes);
        let lenient = BoundaryOptions::lenient();
        let _ = repair_boundary(&raw, &lenient);
        let schema = contract_opts();
        let _ = repair_boundary(&raw, &schema);
        let _ = extract_members(&raw, &schema);
        let _ = decode_boundary::<Contract>(&raw, &schema);
    }

    #[test]
    fn never_panics_on_arbitrary_utf8(s in any::<String>()) {
        let lenient = BoundaryOptions::lenient();
        let _ = repair_boundary(&s, &lenient);
        let schema = contract_opts();
        let _ = repair_boundary(&s, &schema);
        let _ = extract_members(&s, &schema);
        let _ = decode_boundary::<Contract>(&s, &schema);
    }

    /// (b) Identity on valid JSON: canonical serializations pass through
    /// byte-identical.
    #[test]
    fn identity_on_valid_json(value in arb_json()) {
        let text = serde_json::to_string(&value).expect("serializes");
        let repaired = repair_boundary(&text, &BoundaryOptions::lenient());
        prop_assert_eq!(repaired.text, text);
    }

    /// (c) Idempotence: repairing repaired output is a fixpoint.
    #[test]
    fn idempotent_on_adversarial(s in ".{0,512}") {
        let opts = BoundaryOptions::lenient();
        let once = repair_boundary(&s, &opts);
        let twice = repair_boundary(&once.text, &opts);
        prop_assert_eq!(once.text, twice.text);
    }

    /// (d) String contents survive repair: any string round-trips through a
    /// repaired document with its value intact (escapes are preserved
    /// verbatim, so the parsed value is unchanged).
    #[test]
    fn string_contents_preserved(s in ".{0,128}") {
        let text = format!("{{\"k\":{}}}", serde_json::to_string(&s).expect("serializes"));
        let repaired = repair_boundary(&text, &BoundaryOptions::lenient());
        let value: serde_json::Value =
            serde_json::from_str(&repaired.text).expect("repaired text must parse");
        prop_assert_eq!(&value["k"], &serde_json::Value::from(s));
    }
}

/// (d) Golden pairs: `lenient()` alters structure outside strings only —
/// control-character escaping and single-quote conversion are the two
/// documented exceptions.
#[test]
fn lenient_content_preservation_goldens() {
    let pairs = [
        // Untouched: commas, spaces, and escapes inside strings are content.
        (
            r#"{"action":"a,b","count":1}"#,
            r#"{"action":"a,b","count":1}"#,
        ),
        (r#"{"a":"p\"q"}"#, r#"{"a":"p\"q"}"#),
        (r#"{"a":"keep  spaces "}"#, r#"{"a":"keep  spaces "}"#),
        // Single-quote conversion changes quotes, never contents.
        ("{'action':'hi'}", r#"{"action":"hi"}"#),
        // Raw control characters are escaped (the documented exception).
        ("{\"a\":\"x\x01y\"}", "{\"a\":\"x\\u0001y\"}"),
        ("{\"a\":\"tab\there\"}", "{\"a\":\"tab\\there\"}"),
    ];
    for (raw, want) in pairs {
        let repaired = repair_boundary(raw, &BoundaryOptions::lenient());
        assert_eq!(repaired.text, want, "raw: {raw:?}");
        // ... and the repaired text parses.
        assert!(
            serde_json::from_str::<serde_json::Value>(&repaired.text).is_ok(),
            "repaired must parse: {:?}",
            repaired.text
        );
    }
}

/// (e) `for_schema` ignores unknown members — including hostile spellings.
#[test]
fn for_schema_ignores_unknown_and_hostile_members() {
    let raw = r#"{action: "hi", port: 1, unknown: "x", __proto__: "p", constructor: "c"}"#;
    let members = extract_members(raw, &contract_opts()).expect("members");
    let keys: Vec<&str> = members.iter().map(|(k, _)| k.as_str()).collect();
    assert!(keys.contains(&"action"));
    assert!(keys.contains(&"port"));
    assert!(!keys.contains(&"unknown"), "unknown ignored: {keys:?}");
    assert!(!keys.contains(&"__proto__"), "hostile ignored: {keys:?}");
    assert!(!keys.contains(&"constructor"), "hostile ignored: {keys:?}");
}

/// (f) Bounded work/stack: deeply nested and megabyte-scale inputs complete
/// with output proportional to input. The walker is iterative (no recursion
/// over nesting), single-pass, so hostile depth cannot overflow the stack
/// and repair cannot blow up in size.
#[test]
fn bounded_work_on_deeply_nested_input() {
    let raw = format!("{{\"a\":{}}}", "[".repeat(20_000));
    let repaired = repair_boundary(&raw, &BoundaryOptions::lenient());
    assert!(
        repaired.text.len() <= raw.len() + 16,
        "output must stay proportional: {} vs {}",
        repaired.text.len(),
        raw.len()
    );
}

#[test]
fn bounded_work_on_megabyte_input() {
    let raw = format!(
        "[{}]",
        (0..250_000)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    assert!(raw.len() > 1_000_000, "fixture must be megabyte-scale");
    let repaired = repair_boundary(&raw, &BoundaryOptions::lenient());
    assert_eq!(repaired.text, raw, "valid JSON passes through at scale");
}

/// A `FieldAccess` spy: records every name `decode_boundary` routes to
/// `set_field`, proving unknown/duplicate/hostile keys never reach the
/// field beyond the schema's `field_names`.
///
/// Hand-rolled (not `StubComponent`) because observation requires
/// intercepting the call itself; `StubComponent` has no per-field hook.
#[derive(Default)]
struct Spy {
    seen: Arc<Mutex<Vec<String>>>,
    port: u16,
}

impl Spy {
    fn names(&self) -> Vec<String> {
        self.seen.lock().expect("spy lock").clone()
    }
}

impl FieldAccess for Spy {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        self.seen.lock().expect("spy lock").push(name.to_string());
        if name == "port" {
            let parsed: u16 = value
                .parse()
                .map_err(|_| FieldError::Parse(format!("invalid u16 for 'port': {value}")))?;
            if !(1..=100).contains(&parsed) {
                return Err(FieldError::Constraint(
                    "port: value above maximum 100".into(),
                ));
            }
            self.port = parsed;
        }
        Ok(())
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "port" => Ok(self.port.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        Contract::default().field_names()
    }
}

impl Describable for Spy {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}

/// (t2) Unknown, duplicate, and hostile keys never reach `set_field`
/// off-schema: every routed name is in the schema's `field_names`.
#[test]
fn no_key_smuggling_past_field_names() {
    let raw = r#"{action: "hi", port: 7, unknown: "x", __proto__: "p", constructor: "c", port: 8}"#;
    let opts = contract_opts();
    let (spy, decoded) = decode_boundary::<Spy>(raw, &opts).expect("decodes");
    let allowed = Contract::default().field_names();
    for name in spy.names() {
        assert!(
            allowed.contains(&name.as_str()),
            "smuggled past validation: {name:?}"
        );
    }
    // In-schema duplicates route (last wins); hostile keys never appear.
    assert!(!spy.names().iter().any(|n| n == "unknown"));
    assert!(!spy.names().iter().any(|n| n == "__proto__"));
    assert_eq!(spy.port, 8, "last duplicate wins");
    assert!(decoded.contains(&"port".to_string()));
}

/// (t2) A member that fails `set_field` keeps its default and is absent
/// from the decoded-names list — never a fabricated pass.
#[test]
fn failed_member_keeps_default_and_stays_unnamed() {
    let raw = r#"{action: "hi", port: 99999}"#;
    let (contract, decoded) =
        decode_boundary::<Contract>(raw, &contract_opts()).expect("action decodes");
    assert_eq!(contract.action, "\"hi\"", "action keeps its value");
    assert_eq!(contract.port, 0, "failed port keeps its default");
    assert!(decoded.contains(&"action".to_string()));
    assert!(
        !decoded.contains(&"port".to_string()),
        "failed member must be absent: {decoded:?}"
    );
}

/// (t3) Quote table, pinned spec: a bare `String` without `coerce` keeps
/// its surrounding quotes — the member strings are JSON fragments and the
/// field's own pipeline (here: none) owns quote handling.
#[test]
fn quote_table_bare_string_keeps_quotes() {
    let (contract, _) =
        decode_boundary::<Contract>(r#"{action: "hi"}"#, &contract_opts()).expect("decodes");
    assert_eq!(contract.action, "\"hi\"");
}

/// (t3) `coerce = "trim, strip_quotes"` strips the quotes the bare field keeps.
/// Inner spaces are content and survive; the pipeline trims the outside first.
#[test]
fn quote_table_strip_quotes_coercion() {
    let (contract, _) =
        decode_boundary::<Contract>(r#"{mode: "slow"}"#, &contract_opts()).expect("decodes");
    assert_eq!(contract.mode, "slow");
    let (contract, _) =
        decode_boundary::<Contract>(r#"{mode: " padded "}"#, &contract_opts()).expect("decodes");
    assert_eq!(contract.mode, " padded ");
}

/// (t3) `json_escape` round-trips: raw control characters escape, and the
/// escaped form parses back to the original content.
#[test]
fn quote_table_json_escape_round_trip() {
    let raw = "{note: \"a\tb\"}";
    let (contract, _) = decode_boundary::<Contract>(raw, &contract_opts()).expect("decodes");
    assert_eq!(contract.note, "\"a\\tb\"");
    let back: String = serde_json::from_str(&contract.note).expect("escaped form must parse");
    assert_eq!(back, "a\tb");
}
