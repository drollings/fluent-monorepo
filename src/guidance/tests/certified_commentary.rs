//! Certified commentary: bad-symbol, bad-line, and oversize prose never
//! lands; human prose takes the byte-identical path untouched.

use super::certified_write;

const SOURCE: &str = "pub fn foo() void {}\npub fn bar() void {}\n";
const KNOWN: &[&str] = &["foo", "bar"];

#[test]
fn valid_prose_passes() {
    let prose = "Calls `foo` once.";
    assert_eq!(
        certified_write(SOURCE, prose, Some(1), KNOWN, 100),
        Some(prose.to_string())
    );
}

#[test]
fn unknown_symbol_never_lands() {
    assert_eq!(
        certified_write(SOURCE, "Calls `qux`.", Some(1), KNOWN, 100),
        None
    );
}

#[test]
fn unresolvable_line_never_lands() {
    assert_eq!(
        certified_write(SOURCE, "Calls `foo`.", Some(99), KNOWN, 100),
        None
    );
    assert_eq!(
        certified_write(SOURCE, "Calls `foo`.", None, KNOWN, 100),
        None
    );
}

#[test]
fn oversize_prose_never_lands() {
    let prose = "word ".repeat(10_000);
    assert_eq!(certified_write(SOURCE, &prose, Some(1), KNOWN, 10), None);
}

#[test]
fn empty_prose_never_lands() {
    assert_eq!(certified_write(SOURCE, "   ", Some(1), KNOWN, 100), None);
    assert!(certified_write(SOURCE, "Calls `foo` and `bar`.", Some(2), KNOWN, 100).is_some());
}
