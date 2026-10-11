use super::*;
use protopyne_types::Validate;
use std::path::PathBuf;

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn library_path() -> PathBuf {
    data_root().join("libraries/_libUtil.pine")
}

fn mas_path() -> PathBuf {
    data_root().join("libraries/_libMAs.pine")
}

fn indicator_path() -> PathBuf {
    data_root().join("indicators/_FastMAs.pine")
}

fn read_raw(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).expect("fixture readable")
}

#[test]
fn loads_lib_util_identity() {
    let path = library_path();
    let source = load_pine_source(&path).expect("libUtil loads");
    assert_eq!(source.text, read_raw(&path), "loaded text byte-identical");
    assert_eq!(source.version, Some(6), "version 6 recorded");
    assert_eq!(
        source.kind,
        SourceKind::Library {
            name: "libUtil".to_owned()
        }
    );
    assert!(source.imports.is_empty());
    assert!(source.warnings.is_empty(), "clean v6 file warns nothing");
}

#[test]
fn loads_lib_mas_identity_with_import() {
    let path = mas_path();
    let source = load_pine_source(&path).expect("libMAs loads");
    assert_eq!(source.text, read_raw(&path), "loaded text byte-identical");
    assert_eq!(source.version, Some(6));
    assert_eq!(
        source.kind,
        SourceKind::Library {
            name: "libMAs".to_owned()
        }
    );
    assert_eq!(
        source.imports,
        vec![ImportDecl {
            library: LibraryId {
                publisher: "showmethegrail".to_owned(),
                name: "libUtil".to_owned(),
                version: 12,
            },
            alias: "UTIL".to_owned(),
        }]
    );
    assert!(source.warnings.is_empty());
}

#[test]
fn loads_indicator_identity_with_two_imports() {
    let path = indicator_path();
    let source = load_pine_source(&path).expect("indicator loads");
    assert_eq!(source.text, read_raw(&path), "loaded text byte-identical");
    assert_eq!(source.version, Some(6));
    assert_eq!(source.kind, SourceKind::Indicator);
    let aliases: Vec<&str> = source
        .imports
        .iter()
        .map(|decl| decl.alias.as_str())
        .collect();
    assert_eq!(aliases, vec!["UTIL", "libMAs"]);
    assert_eq!(source.imports[0].library.name, "libUtil");
    assert_eq!(source.imports[0].library.version, 12);
    assert_eq!(source.imports[1].library.name, "libMAs");
    assert_eq!(source.imports[1].library.version, 95);
    assert!(source.warnings.is_empty());
}

#[test]
fn version_five_fixture_warns_without_refusal() {
    let text = "//@version=5\nlibrary('oldLib')\nimport a/b/3 as B\n";
    let source = parse_pine_source("old.pine", text);
    assert_eq!(source.text, text, "loaded text byte-identical");
    assert_eq!(source.version, Some(5), "declared version retained");
    assert_eq!(source.imports.len(), 1);
    assert_eq!(source.warnings.len(), 1, "mismatch warns exactly once");
    assert!(
        source.warnings[0].message.contains('5'),
        "warning names the declared version: {}",
        source.warnings[0].message
    );
    assert_eq!(source.warnings[0].declared, Some(5));
    source.validate().expect("warn-only: still valid");
}

#[test]
fn missing_header_fixture_warns_without_refusal() {
    let text = "library('noHeader')\n";
    let source = parse_pine_source("bare.pine", text);
    assert_eq!(source.text, text, "loaded text byte-identical");
    assert_eq!(source.version, None);
    assert_eq!(source.warnings.len(), 1, "missing header warns exactly once");
    assert_eq!(source.warnings[0].declared, None);
    source.validate().expect("warn-only: still valid");
}

#[test]
fn empty_source_refuses_with_reason() {
    let source = parse_pine_source("empty.pine", "");
    let err = source.validate().expect_err("empty source must refuse");
    assert!(
        err.to_string().contains("empty"),
        "refusal names the reason, got: {err}"
    );
    let whitespace = parse_pine_source("blank.pine", "  \n");
    assert!(whitespace.validate().is_err(), "blank source must refuse");
}

#[test]
fn load_refuses_empty_file_with_reason() {
    let path = std::env::temp_dir().join(format!("pine_empty_{}.pine", std::process::id()));
    std::fs::write(&path, "").expect("empty fixture writable");
    let err = load_pine_source(&path).expect_err("empty file must refuse");
    std::fs::remove_file(&path).ok();
    assert!(
        err.to_string().contains("empty"),
        "refusal names the reason, got: {err}"
    );
}

#[test]
fn load_refuses_unreadable_path_with_reason() {
    let missing = data_root().join("libraries/_does_not_exist.pine");
    let err = load_pine_source(&missing).expect_err("missing file must refuse");
    assert!(
        err.to_string().contains("_does_not_exist.pine"),
        "refusal names the path, got: {err}"
    );
}

#[test]
fn component_surface_validates_through_execute() {
    use fluent_wvr::prelude::{Component, WorkUnit};
    let path = library_path();
    let mut source = load_pine_source(&path).expect("libUtil loads");
    assert_eq!(source.field_names(), &["path"]);
    assert_eq!(
        source.get_field("path").expect("path readable"),
        path.to_string_lossy()
    );
    assert!(source.set_field("path", "x").is_err(), "loaded source immutable");
    let ctx = fluent_wvr::prelude::WorkContext::default();
    let output = source.execute(&ctx).expect("valid source executes");
    assert!(output.success);

    let mut empty = parse_pine_source("empty.pine", "");
    assert!(empty.execute(&ctx).is_err(), "empty source fails closed");
    assert!(empty.set_field("anything", "v").is_err());
    let _ = empty.as_any();
}
