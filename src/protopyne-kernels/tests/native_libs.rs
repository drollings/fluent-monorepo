use super::*;
use fluent_wvr::validate::Validate;
use std::path::PathBuf;

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn outcome_of(publisher: &str, name: &str, version: Option<u64>) -> String {
    match find_native(publisher, name, version) {
        Ok(id) => format!("ok:{id}"),
        Err(err) => format!("err:{err}"),
    }
}

#[test]
fn version_pins_resolve_identically() {
    for (publisher, name) in [("showmethegrail", "libBars"), ("showmethegrail", "libApex")] {
        let versions = [Some(1), Some(6), Some(12), Some(88), Some(99), None];
        let first = outcome_of(publisher, name, versions[0]);
        for version in versions.iter().skip(1) {
            assert_eq!(
                outcome_of(publisher, name, *version),
                first,
                "version {version:?} must resolve identically for {publisher}/{name}"
            );
        }
        assert!(
            version_is_ignored(publisher, name),
            "version_is_ignored must hold for {publisher}/{name}"
        );
    }
}

#[test]
fn unknown_publisher_or_name_refuses_with_reason() {
    for (publisher, name) in [
        ("showmethegrail", "noSuchLib"),
        ("unknown-publisher", "libBars"),
        ("", "libBars"),
        ("showmethegrail", ""),
    ] {
        let err =
            find_native(publisher, name, Some(12)).expect_err("unknown native library must refuse");
        let text = err.to_string();
        assert!(
            !text.trim().is_empty(),
            "refusal must carry a reason for {publisher}/{name}"
        );
    }
    assert!(
        !version_is_ignored("showmethegrail", "noSuchLib"),
        "unknown libraries are not part of the native table"
    );
}

#[test]
fn audit_row_records_requested_vs_served() {
    let audit = NativeAudit::new(
        "showmethegrail",
        "libBars",
        Some(12),
        NativeServed::Pine,
        "deadbeef",
    );
    assert_eq!(audit.requested_publisher, "showmethegrail");
    assert_eq!(audit.requested_name, "libBars");
    assert_eq!(audit.requested_version, Some(12));
    assert_eq!(audit.served, NativeServed::Pine);
    assert_eq!(audit.pine_sha, "deadbeef");
    let rendered = audit.to_string();
    assert!(
        rendered.contains("libBars"),
        "audit display names the library: {rendered}"
    );
    assert!(
        rendered.contains("12"),
        "audit display names the requested version: {rendered}"
    );
}

#[test]
fn oracle_files_still_load_warn_only() {
    for file in ["libraries/_libBars.pine", "libraries/_libApex.pine"] {
        let path = data_root().join(file);
        let source =
            crate::load_pine_source(&path).unwrap_or_else(|_| panic!("oracle {file} loads"));
        assert!(
            !source.text.trim().is_empty(),
            "oracle {file} keeps its byte-identical text"
        );
        source
            .validate()
            .unwrap_or_else(|_| panic!("oracle {file} stays warn-only valid"));
    }
}

#[test]
fn non_native_libraries_stay_unmatched() {
    assert_eq!(
        NATIVE_LIBRARIES.len(),
        3,
        "native table holds exactly the three planned entries"
    );
    for entry in NATIVE_LIBRARIES {
        assert!(
            !entry.enabled,
            "native serving stays disabled: {}/{}",
            entry.publisher, entry.name
        );
    }
    let err = find_native("showmethegrail", "libDrawing", Some(6))
        .expect_err("non-native library must not match native");
    assert!(
        err.to_string().contains("libDrawing"),
        "refusal names the library, got: {err}"
    );
}
