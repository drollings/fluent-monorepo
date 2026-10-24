//! AUDIT-H4: derive coverage of non-scalar / Option-numeric types.

use fluent_wvr::prelude::*;

// Option<u32> with bounds: bounds are silently dropped (is_numeric_type fails on "Option<...>")
#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct OptionNumeric {
    #[field(min = 1, max = 10)]
    count: Option<u32>,
    name: String,
}
impl WorkUnit for OptionNumeric {
    fn name(&self) -> &str {
        "optnum"
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
fluent_wvr::impl_component!(OptionNumeric);

// Option<String> with max_len/pattern: string guards only apply when !is_option
#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct OptionStringConstrained {
    #[field(max_len = 3, pattern = "hi")]
    nick: Option<String>,
}
impl WorkUnit for OptionStringConstrained {
    fn name(&self) -> &str {
        "optstr"
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
fluent_wvr::impl_component!(OptionStringConstrained);

#[test]
fn h4_option_numeric_bounds_enforced() {
    let mut c = OptionNumeric {
        count: None,
        name: "x".into(),
    };
    // 999 >> max 10 -> Constraint (fixed).
    assert!(c.set_field("count", "999").is_err());
    assert_eq!(c.count, None, "value unchanged after rejected write");
    c.set_field("count", "5").unwrap();
    assert_eq!(c.count, Some(5));
    // schema emits numbers (fixed H2).
    let s = c.describe();
    assert!(s["properties"]["count"]["minimum"].is_number());
    assert!(s["properties"]["count"]["maximum"].is_number());
}

#[test]
fn h4_option_string_guards_enforced() {
    let mut c = OptionStringConstrained { nick: None };
    // max_len=3 violated, pattern "hi" absent -> Constraint (fixed).
    assert!(c.set_field("nick", "way too long without pattern").is_err());
    assert_eq!(c.nick, None);
    c.set_field("nick", "hi!").unwrap();
    assert_eq!(c.nick.as_deref(), Some("hi!"));
}

// Vec / nested struct / enum: compile-fail cases are documented by source.
// Vec<String> has no FromStr -> `value.parse::<Vec<String>>()` fails to compile.
// We pin the *reason* here: the derive emits a generic parse without a clear message.
// (trybuild would assert the error; here we assert the positive: ArcIntern works.)

#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct InternedField {
    key: fluent_wvr::ArcIntern<str>,
}
impl WorkUnit for InternedField {
    fn name(&self) -> &str {
        "interned"
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
fluent_wvr::impl_component!(InternedField);

#[test]
fn h4_arcintern_supported() {
    let mut c = InternedField {
        key: ArcIntern::from("a"),
    };
    c.set_field("key", "b").unwrap();
    assert_eq!(&*c.key, "b");
}
