//! Fix-rung pins: D1 splice convergence, D2 suggestion application,
//! D3 recipes with control groups, and the verified-apply loop.

use fluent_wvr_testutil::tempdir;

use crate::fix::{
    apply_suggestions, apply_verified_patch, format_rust, parse_clippy_suggestions, recipe_import,
    recipe_mut, ApplyCtx,
};

fn apply_ctx<'a>(pre_hash: &str) -> ApplyCtx<'a> {
    ApplyCtx {
        pre_hash: pre_hash.to_string(),
        language: "rust",
        protected_globs: &[],
        allow_pub_change: false,
        allow_cfg_change: false,
        fix_class: "generic",
        max_bytes: 1_000_000,
        max_changed_lines: 500,
        rustfmt_config: None,
        build: None,
        tests: None,
        before_diags: &[],
        after_diags: &[],
    }
}

#[test]
fn d1_keeps_test_bytes_while_formatting() {
    let dir = tempdir();
    let path = dir.path().join("a.rs");
    let before = "pub fn messy(  x:u8 )->u8 {\n    x\n}\n\n#[test]\nfn t() {\n    assert!(  true  );\n}\n";
    std::fs::write(&path, before).expect("write");
    let outcome = format_rust(&path, before);
    assert!(outcome.conflict.is_none(), "no prose conflict expected");
    assert!(outcome.text.contains("pub fn messy(x: u8) -> u8"), "code formatted");
    assert!(
        outcome.text.contains("assert!(  true  );"),
        "test bytes survive byte-identically"
    );
    // Convergence: formatting the result is a fixed point.
    let again = format_rust(&path, &outcome.text);
    assert_eq!(again.text, outcome.text, "format must converge, not oscillate");
}

#[test]
fn d1_reports_fmt_conflict_instead_of_looping() {
    let dir = tempdir();
    std::fs::write(
        dir.path().join("rustfmt.toml"),
        "wrap_comments = true\nnormalize_comments = true\n",
    )
    .expect("write config");
    let path = dir.path().join("a.rs");
    let before = "pub fn a() {}\n\n// A very long comment that rustfmt with wrapping enabled would certainly want to reflow across lines here.\n\npub fn b() {}\n";
    std::fs::write(&path, before).expect("write");
    let outcome = format_rust(&path, before);
    assert!(outcome.conflict.is_some(), "prose pressure must surface as one finding");
    let again = format_rust(&path, &outcome.text);
    assert_eq!(again.text, outcome.text, "conflict must not oscillate");
}

#[test]
fn d2_applies_machine_applicable_in_offset_order() {
    let text = "fn a() {\n    let x = &5;\n    let y = &6;\n    println!(\"{x} {y}\");\n}\n";
    let diagnostics = r#"{"message": "unused borrow", "spans": [{"byte_start": 21, "byte_end": 22, "suggested_replacement": "", "suggestion_applicability": "MachineApplicable"}]}
{"message": "unused borrow", "spans": [{"byte_start": 37, "byte_end": 38, "suggested_replacement": "", "suggestion_applicability": "MachineApplicable"}]}
{"message": "maybe-dirty", "spans": [{"byte_start": 21, "byte_end": 22, "suggested_replacement": "x", "suggestion_applicability": "MaybeIncorrect"}]}"#;
    let suggestions = parse_clippy_suggestions(diagnostics);
    assert_eq!(suggestions.len(), 2, "only MachineApplicable parses");
    let outcome = apply_suggestions(text, &suggestions, &[]);
    assert_eq!(outcome.applied, 2);
    assert_eq!(outcome.deferred, 0);
    assert!(outcome.text.contains("let x = 5;"), "{}", outcome.text);
    assert!(outcome.text.contains("let y = 6;"), "{}", outcome.text);
}

#[test]
fn d2_defers_overlaps_and_respects_excluded_spans() {
    let text = "fn a() {\n    let x = &5;\n}\n";
    let overlapping = r#"{"message": "m", "spans": [{"byte_start": 21, "byte_end": 23, "suggested_replacement": "5", "suggestion_applicability": "MachineApplicable"}]}
{"message": "m", "spans": [{"byte_start": 21, "byte_end": 22, "suggested_replacement": "", "suggestion_applicability": "MachineApplicable"}]}"#;
    let suggestions = parse_clippy_suggestions(overlapping);
    let outcome = apply_suggestions(text, &suggestions, &[]);
    assert_eq!(outcome.applied, 1);
    assert_eq!(outcome.deferred, 1, "overlaps defer to the next wave");
    let excluded = [(0u64, text.len() as u64)];
    let outcome = apply_suggestions(text, &suggestions, &excluded);
    assert_eq!(outcome.applied, 0, "excluded spans never apply");
}

#[test]
fn d3_import_fires_only_when_unique() {
    let patch = recipe_import("E0432 unresolved import `foo`", &["crate::foo".to_string()], true);
    assert_eq!(patch.as_deref(), Some("use crate::foo;\n"));
    assert!(recipe_import("E0432 unresolved import", &[], true).is_none());
    assert!(recipe_import(
        "E0432 unresolved import",
        &["crate::a".to_string(), "crate::b".to_string()],
        true
    )
    .is_none(), "ambiguous imports never fire");
    assert!(recipe_import("E0432 unresolved import", &["crate::a".to_string()], false).is_none());
    assert!(recipe_import("E9999 something else", &["crate::a".to_string()], true).is_none());
}

#[test]
fn d3_mut_inserts_precisely() {
    let text = "fn a() {\n    let x = 1;\n    x = 2;\n}\n";
    let offset = text.find("x = 1").expect("binding") as u64;
    let patched = recipe_mut(text, offset, true).expect("fires");
    assert!(patched.contains("let mut x = 1;"), "{patched}");
    assert!(recipe_mut(text, offset, false).is_none());
    assert!(recipe_mut(text, 1_000_000, true).is_none(), "out-of-range never fires");
}

#[test]
fn d3_seeded_set_measures_precision_before_enablement() {
    // Seeded trigger set: positives fire, control-group negatives
    // (ambiguous, unlisted class, disabled policy) never do. Run this
    // before enabling any recipe for auto-apply.
    let positives = [
        recipe_import("error[E0432]: unresolved import", &["crate::only".to_string()], true)
            .is_some(),
        recipe_mut("fn a() {\n    let x = 1;\n}\n", 17, true).is_some(),
    ];
    let negatives = [
        recipe_import("error[E0432]: unresolved import", &["a".to_string(), "b".to_string()], true)
            .is_none(),
        recipe_import("error[E0432]: unresolved import", &["crate::a".to_string()], false)
            .is_none(),
        recipe_import("error[E0001]: other", &["crate::a".to_string()], true).is_none(),
        recipe_mut("fn a() {\n    let x = 1;\n}\n", 17, false).is_none(),
        recipe_mut("fn a() {\n    let x = 1;\n}\n", 999, true).is_none(),
    ];
    assert!(positives.iter().all(|v| *v), "recall must be total on seeds");
    assert!(negatives.iter().all(|v| *v), "control group must never fire");
}

#[test]
fn apply_keeps_gated_patches_and_reverts_the_rest() {
    let dir = tempdir();
    let path = dir.path().join("a.rs");
    let before = "fn  a() {}\n";
    std::fs::write(&path, before).expect("write");
    let pre_hash = common_core::hash::sha256_hex(before.as_bytes());
    let mut good = apply_ctx(&pre_hash);
    good.fix_class = "rustfmt";
    let report = apply_verified_patch(&path, "fn a() {}\n", &good);
    assert!(report.is_kept(), "gated patch keeps: {:?}", report.gates);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "fn a() {}\n");
    // A test-span rewrite reverts and restores byte-identically.
    let testy = "fn a() {}\n\n#[test]\nfn t() {\n    assert!(true);\n}\n";
    std::fs::write(&path, testy).expect("write");
    let pre_hash = common_core::hash::sha256_hex(testy.as_bytes());
    let bad = apply_ctx(&pre_hash);
    let evil = "fn a() {}\n\n#[test]\nfn t() {\n    assert!(false);\n}\n";
    let report = apply_verified_patch(&path, evil, &bad);
    assert!(!report.is_kept(), "refused patch reverts");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), testy);
    // A concurrent human edit discards instead of overwriting.
    std::fs::write(&path, "fn human() {}\n").expect("human edit");
    let report = apply_verified_patch(&path, "fn a() {}\n", &good);
    assert!(!report.is_kept(), "stale preconditions never overwrite");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "fn human() {}\n");
}
