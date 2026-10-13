//! Audit ledger: allocation shape, dependency direction, and bloat.
//!
//! Same posture as the scaffolding ledger (shape and source text, never
//! wall-time; budgets observed in `benches/chart_budget.rs`), plus the
//! terminal-specific direction pins: the compute truth is read, never
//! forked (no second indicator table, no second regime constant, no second
//! error type), and the trading path (`server`/`strategy`/`venue`) stays a
//! dev-only round-trip proof — never a main dependency. Growth beyond this
//! ledger fails loudly until the ledger itself is edited with a
//! justification — that edit is the review.

use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_text(path: &PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Source file text for one `src/<name>.rs` module.
fn read_src(name: &str) -> String {
    read_text(&manifest_dir().join("src").join(name))
}

/// Non-comment lines: `//`, `//!`, and `///` lines document but never
/// depend, so direction scans skip them (same rule as the linkage test).
fn code_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('/'))
        .collect()
}

fn src_file_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(manifest_dir().join("src"))
        .expect("src lists")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rs"))
        .collect();
    names.sort();
    names
}

fn forbid_in_src(token: &str, why: &str) {
    for name in src_file_names() {
        for line in code_lines(&read_src(&name)) {
            assert!(
                !line.contains(token),
                "{name} must not contain {token:?}: {why} (line: {line})"
            );
        }
    }
}

#[test]
fn bloat_ledger_pins_source_files() {
    assert_eq!(
        src_file_names(),
        vec![
            "chart.rs",
            "export.rs",
            "feed.rs",
            "integration_lite.rs",
            "lib.rs",
            "oscillators.rs",
            "regime_labels.rs",
            "session.rs",
            "topology.rs",
            "widgets.rs",
        ],
        "a new source file must justify itself by editing this ledger"
    );
}

/// Parse the `[dependencies]` key list from the crate manifest.
fn main_dep_keys() -> Vec<String> {
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    let mut in_deps = false;
    let mut keys = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_deps = line == "[dependencies]";
            continue;
        }
        if !in_deps || line.is_empty() || line.starts_with('#') {
            continue;
        }
        keys.push(line.split('=').next().unwrap_or("").trim().to_owned());
    }
    keys.sort();
    keys
}

#[test]
fn bloat_ledger_pins_default_feature_deps() {
    assert_eq!(
        main_dep_keys(),
        vec![
            "bon",
            "common-core",
            "eframe",
            "egui",
            "fluent-concurrency",
            "fluent-dag",
            "fluent-egui",
            "fluent-wvr",
            "protopyne-chart-model",
            "protopyne-chart-scale",
            "protopyne-kernels",
            "protopyne-types",
            "serde",
            "serde_json",
            "tokio",
        ],
        "a new default-feature dependency must justify itself by editing this ledger"
    );
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    // Main `[dependencies]` section body: lines after the header up to the
    // next section header (a line starting with `[`).
    let mut in_deps = false;
    let mut body = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed == "[dependencies]";
            continue;
        }
        if in_deps {
            body.push_str(line);
            body.push('\n');
        }
    }
    let deps_section = body;
    for trading in ["protopyne-server", "protopyne-strategy", "protopyne-venue"] {
        assert!(
            !deps_section.contains(trading),
            "{trading} must not enter main dependencies (trading path stays a dev-only round-trip proof)"
        );
    }
    assert!(
        deps_section.contains("optional = true"),
        "the windowing shell stays optional so the default build needs no backend"
    );
}

#[test]
fn terminal_never_mutates_the_trading_path() {
    for token in [
        "protopyne_server",
        "protopyne_strategy",
        "protopyne_venue",
        "protopyne-server",
        "protopyne-strategy",
        "protopyne-venue",
    ] {
        forbid_in_src(token, "trading crates are read at dev time only, never a main dependency");
    }
}

#[test]
fn compute_truth_is_reused_never_forked() {
    // One table, one band, one error type: the terminal adapts kernel
    // outputs for display but never redefines them.
    forbid_in_src("enum EguiError", "the error type lives in the scaffolding (single truth)");
    forbid_in_src("REGIME_HYSTERESIS =", "the band constant lives in kernels (re-export only)");
    forbid_in_src("struct PanePlan", "the plan lives in the scaffolding (topology pours in)");
    forbid_in_src("struct PaneCache", "the cache lives in the scaffolding");
    forbid_in_src("struct Viewport", "the viewport lives in the scaffolding");
    forbid_in_src("struct ThemeConfig", "the theme lives in the scaffolding");
}

#[test]
fn display_floats_never_parse_back_into_money() {
    for token in ["rust_decimal", "Decimal::", "parse::<Decimal>", "Decimal>"] {
        forbid_in_src(token, "the Decimal -> f64 boundary is one-way");
    }
}

#[test]
fn frame_holds_no_per_widget_machinery() {
    forbid_in_src("oneshot", "no per-widget channels on the frame path");
    forbid_in_src("tokio::spawn", "tasks are Scope-owned, never raw-spawned");
    forbid_in_src("block_on", "no sync-over-async bridges in paint code");
    forbid_in_src("std::fs", "no blocking file I/O in paint code");
    forbid_in_src("std::net", "no blocking network I/O in paint code");
    forbid_in_src("tokio::time::sleep", "no sleeps in paint code");
    forbid_in_src("thread::sleep", "no sleeps in paint code");
    forbid_in_src("Middleware", "no middleware layer per frame");
    // Timing covers plan builds only: `chart.rs` (via `timed_plan_build`)
    // is the single module allowed to name the wrapper.
    for name in src_file_names() {
        if name == "chart.rs" {
            continue;
        }
        for line in code_lines(&read_src(&name)) {
            assert!(
                !line.contains("Instrumented"),
                "{name} must not time per-bar work (timing covers plan builds only)"
            );
        }
    }
    assert!(
        code_lines(&read_src("chart.rs")).iter().any(|line| line.contains("Instrumented")),
        "plan-build timing must stay composed (the wrapper has exactly one home)"
    );
}

#[test]
fn timing_is_observed_never_asserted() {
    for entry in std::fs::read_dir(manifest_dir().join("tests")).expect("tests list") {
        let entry = entry.expect("test entry");
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        if file_name == "budget.rs" {
            continue;
        }
        let text = read_text(&entry.path());
        for line in code_lines(&text) {
            assert!(
                !line.contains("Instant::now"),
                "{file_name} must not time itself (budgets are observed in benches/, never asserted)"
            );
            assert!(
                !line.contains("criterion"),
                "{file_name} must not bench itself (the harness lives in benches/)"
            );
        }
    }
    assert!(
        manifest_dir().join("benches").join("chart_budget.rs").is_file(),
        "domain budgets ride the bench harness, which must exist"
    );
}

#[test]
fn theme_ships_no_embedded_assets() {
    forbid_in_src("include_str!", "no embedded text blobs in paint code");
    forbid_in_src("include_bytes!", "no embedded binary blobs in paint code");
    forbid_in_src(".svg", "no SVG assets in paint code");
    let embedded: Vec<String> = walk_files(&manifest_dir())
        .into_iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("svg"))
        .map(|path| path.display().to_string())
        .collect();
    assert!(embedded.is_empty(), "no SVG assets ship with the crate: {embedded:?}");
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("list {}: {e}", dir.display()));
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}
