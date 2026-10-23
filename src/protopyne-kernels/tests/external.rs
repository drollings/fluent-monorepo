//! External parity artifacts, hermetic half: the JSON format, the loader's
//! refusals, and the comparison's bite are all provable without credentials.
//! Fixture documents stay inline here — never committed as export files —
//! so nothing in the tree can be mistaken for third-party numbers. The
//! real export (operator action) lands under `tests/data/external/`.

use super::*;

const SCRIPT: &[u8] = b"fixture script for the external loader";

fn fixture_doc() -> String {
    serde_json::json!({
        "source": "tradingview",
        "source_version": "2.9",
        "exported_at": "2026-10-20T00:00:00Z",
        "symbol": "AMEX:SPY",
        "timeframe": "15",
        "script_sha256": script_sha256_hex(SCRIPT),
        "rows": [
            {"bar": 0, "reference": 1.0},
            {"bar": 1, "reference": 2.0},
            {"bar": 2, "reference": 3.0},
        ],
    })
    .to_string()
}

fn without(field: &str) -> String {
    // Drop the field outright: absence and emptiness must both refuse.
    let doc: serde_json::Value = serde_json::from_str(&fixture_doc()).expect("fixture parses");
    let mut map = doc.as_object().expect("fixture is an object").clone();
    map.remove(field);
    serde_json::Value::Object(map).to_string()
}

/// The loader refuses provenance-less documents: one case per provenance
/// field, each naming the field it missed.
#[test]
fn loader_refuses_a_document_missing_any_provenance_field() {
    for field in [
        "source",
        "source_version",
        "exported_at",
        "symbol",
        "timeframe",
    ] {
        let refused =
            load_external(&without(field), SCRIPT).expect_err(&format!("{field} refuses"));
        assert!(
            refused.to_string().contains(field),
            "{field} refusal names the field, got: {refused}"
        );
    }
}

/// A script edit invalidates the claim rather than silently aging it: the
/// same document loads against the bytes it names and refuses against one
/// flipped byte.
#[test]
fn loader_binds_the_document_to_the_exact_script_bytes() {
    load_external(&fixture_doc(), SCRIPT).expect("named bytes load");
    let mut edited = SCRIPT.to_vec();
    edited[0] ^= 0x01;
    let refused = load_external(&fixture_doc(), &edited).expect_err("edited bytes refuse");
    assert!(
        refused.to_string().contains("script_sha256"),
        "got: {refused}"
    );
}

/// The binding is against the real shipped script, not a second fixture:
/// the hash in a document names the actual library bytes.
#[test]
fn shipped_script_bytes_match_the_hash_a_document_names() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs/libraries/_libMAs.pine");
    let shipped = std::fs::read(&path).expect("the shipped moving-average library reads");
    assert!(!shipped.is_empty());
    let mut doc: serde_json::Value = serde_json::from_str(&fixture_doc()).expect("fixture parses");
    doc["script_sha256"] = serde_json::Value::String(script_sha256_hex(&shipped));
    load_external(&doc.to_string(), &shipped).expect("shipped bytes load");
}

/// An empty row set refuses: there is nothing to compare.
#[test]
fn loader_refuses_a_document_with_no_rows() {
    let mut doc: serde_json::Value = serde_json::from_str(&fixture_doc()).expect("fixture parses");
    doc["rows"] = serde_json::Value::Array(Vec::new());
    let refused = load_external(&doc.to_string(), SCRIPT).expect_err("no rows refuses");
    assert!(refused.to_string().contains("rows"), "got: {refused}");
}

/// A mutated row in a fixture document fails parity within the frozen eps —
/// proving the comparison can fail — while the clean document passes.
#[test]
fn a_mutated_row_fails_parity_within_the_frozen_eps() {
    let doc = load_external(&fixture_doc(), SCRIPT).expect("fixture loads");
    let ours = [1.0, 2.0, 3.0];
    check_external(&doc, &ours, 1e-9).expect("clean rows pass");
    let mut mutated = doc.clone();
    mutated.rows[1].reference = 99.0;
    let failed = check_external(&mutated, &ours, 1e-9).expect_err("the mutation fails");
    assert!(
        failed.to_string().contains("bar 1"),
        "the failure names the mutated bar, got: {failed}"
    );
}
