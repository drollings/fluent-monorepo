//! AUDIT-H3: numeric parsing goes through f64.

use fluent_wvr::prelude::*;

#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct ConstrainedU16 {
    #[field(min = 1, max = 65535)]
    port: u16,
}
impl WorkUnit for ConstrainedU16 {
    fn name(&self) -> &str {
        "c16"
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
fluent_wvr::impl_component!(ConstrainedU16);

#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct ConstrainedU64 {
    #[field(min = 0, max = 18446744073709551615)]
    big: u64,
}
impl WorkUnit for ConstrainedU64 {
    fn name(&self) -> &str {
        "c64"
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
fluent_wvr::impl_component!(ConstrainedU64);

#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct PlainU64 {
    big: u64,
}
impl WorkUnit for PlainU64 {
    fn name(&self) -> &str {
        "p64"
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
fluent_wvr::impl_component!(PlainU64);

#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
#[allow(dead_code)]
struct ConstrainedI64 {
    #[field(min = -9223372036854775808, max = 9223372036854775807)]
    v: i64,
}
impl WorkUnit for ConstrainedI64 {
    fn name(&self) -> &str {
        "ci64"
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
fluent_wvr::impl_component!(ConstrainedI64);

#[test]
fn h3_fraction_rejected_for_integer() {
    // Oracle: "1.9".parse::<u16>() is Err. Fixed: strict native parse rejects.
    assert!("1.9".parse::<u16>().is_err());
    let mut c = ConstrainedU16 { port: 80 };
    assert!(c.set_field("port", "1.9").is_err());
    assert_eq!(c.port, 80, "value unchanged after rejected write");
}

#[test]
fn h3_large_u64_exact_via_native_parse() {
    // 2^53+1 = 9007199254740993: strict native parse preserves exact value.
    let input = "9007199254740993";
    let oracle: u64 = input.parse().unwrap();
    assert_eq!(oracle, 9007199254740993u64);
    let mut c = ConstrainedU64 { big: 0 };
    c.set_field("big", input).unwrap();
    assert_eq!(
        c.big, 9007199254740993u64,
        "native parse preserves precision"
    );
}

#[test]
fn h3_plain_u64_strict_parse_rejects_fraction() {
    // No bounds -> direct str::parse -> correct strict behavior.
    let mut c = PlainU64 { big: 0 };
    assert!(c.set_field("big", "1.9").is_err());
    assert!(c.set_field("big", "9007199254740993").is_ok());
    assert_eq!(c.big, 9007199254740993u64);
}

#[test]
fn h3_i64_min_exact_via_native_parse() {
    let mut c = ConstrainedI64 { v: 0 };
    c.set_field("v", "-9223372036854775808").unwrap();
    assert_eq!(c.v, i64::MIN);
}

#[test]
fn h3_scientific_rejected_for_integer() {
    // Oracle: "1e3".parse::<u16>() is Err. Fixed: both plain and constrained reject.
    assert!("1e3".parse::<u16>().is_err());
    assert!(" 5 ".parse::<u16>().is_err());
    let mut c = PlainU64 { big: 0 };
    assert!(c.set_field("big", "1e3").is_err());
    let mut cc = ConstrainedU64 { big: 0 };
    assert!(
        cc.set_field("big", "1e3").is_err(),
        "constrained must match strict oracle"
    );
}
