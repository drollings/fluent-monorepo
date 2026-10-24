//! Unknown `#[field(...)]` arguments refuse at expansion time.
//!
//! The closed vocabulary is `skip`, `desc`, `min`, `max`, `clamp`,
//! `required`, `format`, `max_len`, `sanitize`, `pattern`, `coerce`,
//! `parse`, `variants`, `empty_is_none`. Anything else — a typo like
//! `max_lne` or a planned-not-implemented `version_added` — must fail
//! expansion with an error naming the argument, never silently drop.

use super::parse_field_attrs;

fn make_field(attrs: &str, ty: &str, name: &str) -> syn::Field {
    let input: syn::DeriveInput =
        syn::parse_str(&format!("struct Foo {{ #[field({attrs})] {name}: {ty} }}"))
            .expect("fixture must parse");
    match input.data {
        syn::Data::Struct(s) => match s.fields {
            syn::Fields::Named(named) => named.named.into_iter().next().expect("one field"),
            _ => panic!("expected named fields"),
        },
        _ => panic!("expected struct"),
    }
}

#[test]
fn version_added_refuses_naming_the_argument() {
    let field = make_field(r#"version_added = "1.1""#, "Option<String>", "nickname");
    let err = parse_field_attrs(&field).expect_err("version_added must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("version_added"),
        "error must name the unknown argument, got: {msg}"
    );
    for known in [
        "skip",
        "desc",
        "min",
        "max",
        "clamp",
        "required",
        "format",
        "max_len",
        "sanitize",
        "pattern",
        "coerce",
        "parse",
        "variants",
        "empty_is_none",
    ] {
        assert!(
            msg.contains(known),
            "error must list the closed vocabulary (missing {known}), got: {msg}"
        );
    }
}

#[test]
fn version_removed_refuses_naming_the_argument() {
    let field = make_field(r#"version_removed = "2.0""#, "Option<String>", "old");
    let err = parse_field_attrs(&field).expect_err("version_removed must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("version_removed"),
        "error must name the unknown argument, got: {msg}"
    );
}

#[test]
fn misspelled_arg_refuses_naming_the_argument() {
    let field = make_field("max_lne = 100", "String", "name");
    let err = parse_field_attrs(&field).expect_err("typo must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("max_lne"),
        "error must name the unknown argument, got: {msg}"
    );
}

#[test]
fn unknown_sanitize_mode_refuses() {
    let field = make_field(r#"sanitize = "invalid""#, "String", "name");
    let err = parse_field_attrs(&field).expect_err("unknown sanitize mode must refuse");
    assert!(
        err.to_string().contains("invalid"),
        "error must name the bad mode, got: {}",
        err
    );
}
