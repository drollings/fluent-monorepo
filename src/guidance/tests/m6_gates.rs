//! Per-gate pins: each gate rejects its fixture and names its ID;
//! prose and test bytes are invariant across every applier path.

use crate::gates::{
    gates_passed, run_gates, BuildReport, ChangedFile, GateCtx, TestReport,
};

fn ctx<'a>() -> GateCtx<'a> {
    GateCtx {
        language: "rust",
        pre_hash: "pre".to_string(),
        live_hash: "pre".to_string(),
        protected_globs: &[],
        allow_pub_change: false,
        allow_cfg_change: false,
        before_diags: &[],
        after_diags: &[],
        max_bytes: 1_000_000,
        max_changed_lines: 500,
        fix_class: "generic",
        build: None,
        tests: None,
        rustfmt_config: None,
    }
}

fn file(before: &str, after: &str) -> ChangedFile {
    ChangedFile {
        path: "src/a.rs".to_string(),
        before: before.to_string(),
        after: after.to_string(),
    }
}

#[test]
fn p1_rejects_hash_mismatch() {
    let mut c = ctx();
    c.live_hash = "other".to_string();
    let verdicts = run_gates(&file("fn a() {}\n", "fn a() {}\n"), &c);
    assert!(verdicts.iter().any(|v| v.id == "P1" && v.is_fail()));
    assert!(!gates_passed(&verdicts));
}

#[test]
fn p2_refuses_test_paths_and_configs() {
    for path in [
        "src/tests/a.rs",
        "tests/e2e.rs",
        "benches/b.rs",
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "clippy.toml",
        "rustfmt.toml",
    ] {
        let f = ChangedFile {
            path: path.to_string(),
            before: "x\n".to_string(),
            after: "y\n".to_string(),
        };
        let verdicts = run_gates(&f, &ctx());
        assert!(
            verdicts.iter().any(|v| v.id == "P2" && v.is_fail()),
            "{path} must refuse"
        );
    }
}

#[test]
fn p2_refuses_test_span_rewrites() {
    let before = "pub fn a() {}\n\n#[test]\nfn t() {\n    assert!(true);\n}\n";
    let after = "pub fn a() {}\n\n#[test]\nfn t() {\n    assert!(false);\n}\n";
    let verdicts = run_gates(&file(before, after), &ctx());
    assert!(verdicts.iter().any(|v| v.id == "P2" && v.is_fail()));
    let kept = "pub fn a() {}\n\n#[test]\nfn t() {\n    assert!(true);\n}\n";
    let verdicts = run_gates(&file(before, kept), &ctx());
    assert!(verdicts.iter().all(|v| v.id != "P2" || !v.is_fail()));
}

#[test]
fn p2_refuses_new_suppression_and_panics() {
    for snippet in [
        "#[allow(dead_code)]\nfn a() {}\n",
        "fn a() {\n    todo!()\n}\n",
        "fn a() {\n    unsafe { f() }\n}\n",
        "fn a() {\n    panic!(\"x\")\n}\n",
    ] {
        let verdicts = run_gates(&file("fn a() {}\n", snippet), &ctx());
        assert!(
            verdicts.iter().any(|v| v.id == "P2" && v.is_fail()),
            "{snippet:?} must refuse"
        );
    }
}

#[test]
fn p2_refuses_pub_signature_change_without_flag() {
    let before = "pub fn a(x: u8) -> u8 {\n    x\n}\n";
    let after = "pub fn a(x: u16) -> u8 {\n    x as u8\n}\n";
    let verdicts = run_gates(&file(before, after), &ctx());
    assert!(verdicts.iter().any(|v| v.id == "P2" && v.is_fail()));
    let mut c = ctx();
    c.allow_pub_change = true;
    let verdicts = run_gates(&file(before, after), &c);
    assert!(verdicts.iter().all(|v| v.id != "P2" || !v.is_fail()));
}

#[test]
fn p3_rejects_prose_changes() {
    let before = "/// Adds one.\npub fn a() -> u8 {\n    let s = \"hi\";\n    1\n}\n";
    let after = "/// Adds two.\npub fn a() -> u8 {\n    let s = \"hi\";\n    1\n}\n";
    let verdicts = run_gates(&file(before, after), &ctx());
    assert!(verdicts.iter().any(|v| v.id == "P3" && v.is_fail()));
    let after_code = "/// Adds one.\npub fn a() -> u8 {\n    let s = \"hi\";\n    2\n}\n";
    let verdicts = run_gates(&file(before, after_code), &ctx());
    assert!(verdicts.iter().all(|v| v.id != "P3" || !v.is_fail()));
}

#[test]
fn p4_rejects_new_parse_errors() {
    let before = "pub fn a() -> u8 {\n    1\n}\n";
    let after = "pub fn a( -> u8 {\n    1\n}\n";
    let verdicts = run_gates(&file(before, after), &ctx());
    assert!(verdicts.iter().any(|v| v.id == "P4" && v.is_fail()));
}

#[test]
fn p6_requires_strict_shrink() {
    let diags_before = ["E0308: mismatched types".to_string()];
    let mut c = ctx();
    c.before_diags = &diags_before;
    c.after_diags = &[];
    let verdicts = run_gates(&file("fn a() {}\n", "fn a() {}\n"), &c);
    assert!(verdicts.iter().any(|v| v.id == "P6" && v.is_pass()));
    let added = ["E0308: mismatched types".to_string(), "E0599: missing".to_string()];
    let mut c = ctx();
    c.before_diags = &diags_before;
    c.after_diags = &added;
    let verdicts = run_gates(&file("fn a() {}\n", "fn a() {}\n"), &c);
    assert!(verdicts.iter().any(|v| v.id == "P6" && v.is_fail()));
}

#[test]
fn p7_p8_honor_runner_reports() {
    let mut c = ctx();
    c.build = Some(BuildReport {
        clean: true,
        output: String::new(),
    });
    c.tests = Some(TestReport {
        new_failures: Vec::new(),
        output: String::new(),
    });
    let verdicts = run_gates(&file("fn a() {}\n", "fn a() {}\n"), &c);
    assert!(verdicts.iter().any(|v| v.id == "P7" && v.is_pass()));
    assert!(verdicts.iter().any(|v| v.id == "P8" && v.is_pass()));
    let mut c = ctx();
    c.build = Some(BuildReport {
        clean: false,
        output: "error[E0308]".to_string(),
    });
    c.tests = Some(TestReport {
        new_failures: vec!["t_foo".to_string()],
        output: String::new(),
    });
    let verdicts = run_gates(&file("fn a() {}\n", "fn a() {}\n"), &c);
    assert!(verdicts.iter().any(|v| v.id == "P7" && v.is_fail()));
    assert!(verdicts.iter().any(|v| v.id == "P8" && v.is_fail()));
    let verdicts = run_gates(&file("fn a() {}\n", "fn a() {}\n"), &ctx());
    assert!(verdicts.iter().any(|v| v.id == "P7" && v.is_skip()));
    assert!(verdicts.iter().any(|v| v.id == "P8" && v.is_skip()));
}

#[test]
fn p9_enforces_size_caps() {
    let mut c = ctx();
    c.max_bytes = 9;
    let verdicts = run_gates(&file("fn a() {}\n", "fn a() {}\n"), &c);
    assert!(verdicts.iter().any(|v| v.id == "P9" && v.is_fail()));
}

#[test]
fn p10_checks_declared_edit_shape() {
    let mut c = ctx();
    c.fix_class = "rustfmt";
    let verdicts = run_gates(&file("fn  a() {}\n", "fn a() {}\n"), &c);
    assert!(verdicts.iter().any(|v| v.id == "P10" && v.is_pass()));
    let verdicts = run_gates(&file("fn a() {}\n", "fn b() {}\n"), &c);
    assert!(verdicts.iter().any(|v| v.id == "P10" && v.is_fail()));
    let mut c = ctx();
    c.fix_class = "import";
    let verdicts = run_gates(&file("fn a() {}\n", "use x::y;\nfn a() {}\n"), &c);
    assert!(verdicts.iter().any(|v| v.id == "P10" && v.is_pass()));
}

#[test]
fn prose_units_are_stable_under_code_mutation() {
    // Property: every code-only mutation below preserves the prose
    // multiset, and every prose mutation is detected.
    let prefixed = [
        "/// Doc.\n",
        "// plain\n",
        "const _U: &str = \"lit\";\n",
        "const _V: &str = \"a // b\";\n",
    ];
    for prefix in prefixed {
        let before = format!("{prefix}pub fn a() -> u8 {{\n    1\n}}\n");
        let after = format!("{prefix}pub fn a() -> u8 {{\n    2\n}}\n");
        let verdicts = run_gates(&file(&before, &after), &ctx());
        assert!(
            verdicts.iter().all(|v| v.id != "P3" || !v.is_fail()),
            "{prefix:?} must survive code mutation"
        );
    }
    let before = "/// Doc.\npub fn a() -> u8 {\n    1\n}\n";
    let changed = "/// Other.\npub fn a() -> u8 {\n    1\n}\n";
    let verdicts = run_gates(&file(before, changed), &ctx());
    assert!(verdicts.iter().any(|v| v.id == "P3" && v.is_fail()));
}
