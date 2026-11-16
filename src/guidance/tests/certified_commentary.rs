//! Certified commentary: bad-symbol, bad-line, and oversize prose never
//! lands; human prose takes the byte-identical path untouched.

use super::{apply_insertions, certified_write, collect_comment_insertions};
use crate::tests::common::make_test_doc;
use fluent_types::{GuidanceDoc, Member, MemberType, Meta};

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

fn member_doc(name: &str, comment: Option<&str>, line: u32) -> GuidanceDoc {
    GuidanceDoc {
        meta: Meta {
            source: "test.rs".into(),
            ..make_test_doc().meta
        },
        members: vec![Member {
            type_name: MemberType::FnDecl,
            name: name.into(),
            comment: comment.map(smol_str::SmolStr::new),
            line: Some(line),
            ..Member::default()
        }],
        ..make_test_doc()
    }
}

#[test]
fn curated_comment_never_re_lands_above_attributes() {
    let source = "/// Greets the user.\n#[allow(dead_code)]\npub fn hello() {}\n";
    let doc = member_doc("hello", Some("Greets the user."), 3);
    let insertions = collect_comment_insertions(source, &doc, None);
    assert!(
        insertions.is_empty(),
        "doc block above attributes must not duplicate: {insertions:?}"
    );
}

#[test]
fn curated_comment_lands_above_attributes_when_missing() {
    let source = "#[allow(dead_code)]\npub fn hello() {}\n";
    let doc = member_doc("hello", Some("Greets the user."), 2);
    let insertions = collect_comment_insertions(source, &doc, None);
    let result = apply_insertions(source, insertions);
    assert_eq!(
        result, "/// Greets the user.\n#[allow(dead_code)]\npub fn hello() {}\n",
        "docs land above the attribute block"
    );
}

#[test]
fn insertions_preserve_trailing_newline() {
    let source = "pub fn hello() void {}\n";
    let doc = member_doc("hello", Some("Greets."), 1);
    let insertions = collect_comment_insertions(source, &doc, None);
    let result = apply_insertions(source, insertions);
    assert_eq!(result, "/// Greets.\npub fn hello() void {}\n");
    assert!(
        result.ends_with('\n'),
        "trailing newline survives: {result:?}"
    );
}
