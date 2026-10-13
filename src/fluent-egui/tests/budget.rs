//! Audit ledger: allocation shape, dependency direction, and bloat.
//!
//! Every assert here is API shape or source text, never wall-time: frame
//! budgets are observed in `benches/frame_budget.rs` and recorded beside
//! the roadmap, while gates pin what the frame may *hold* (no per-widget
//! channels, no raw spawns, no blocking I/O), which way dependencies point,
//! and exactly which files and default-feature dependencies exist. Growth
//! beyond this ledger fails loudly until the ledger itself is edited with a
//! justification — that edit is the review.
//!
//! Boundary rule pinned here: this crate is domain-blind scaffolding. It may
//! reuse display vocabulary (`protopyne-chart-model` rows), pure scale math
//! (`protopyne-chart-scale`), and the shared validation bridge
//! (`protopyne-types`), but it names no compute truth (`protopyne-kernels`),
//! no trading path (`server`/`strategy`/`venue`), and no trading-domain
//! vocabulary (oscillators, regimes, Pine). Domain display truth lives in
//! the terminal crate, which carries its own ledger.

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
            "config.rs",
            "error.rs",
            "feed.rs",
            "lib.rs",
            "paint.rs",
            "plan.rs",
            "replay.rs",
            "view.rs",
            "viewport.rs",
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
            "fluent-wvr",
            "protopyne-chart-model",
            "protopyne-chart-scale",
            "protopyne-types",
            "serde",
            "serde_json",
            "thiserror",
            "tokio",
        ],
        "a new default-feature dependency must justify itself by editing this ledger"
    );
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    // Main `[dependencies]` section body: lines after the header up to the
    // next section header (a line starting with `[`). Values may contain
    // `[` themselves (`features = ["pine"]`), so only a leading `[` ends it.
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
    for trading in [
        "protopyne-kernels",
        "protopyne-server",
        "protopyne-strategy",
        "protopyne-venue",
    ] {
        assert!(
            !deps_section.contains(trading),
            "{trading} must not enter main dependencies (scaffolding stays off compute truth and the trading path)"
        );
    }
    assert!(
        deps_section.contains("optional = true"),
        "the windowing shell stays optional so the default build needs no backend"
    );
}

#[test]
fn scaffolding_names_no_compute_truth_or_trading_path() {
    for token in [
        "protopyne_kernels",
        "protopyne-kernels",
        "protopyne_server",
        "protopyne_strategy",
        "protopyne_venue",
        "protopyne-server",
        "protopyne-strategy",
        "protopyne-venue",
    ] {
        forbid_in_src(token, "compute truth and trading crates are never a scaffolding dependency");
    }
}

#[test]
fn scaffolding_names_no_trading_domain_vocabulary() {
    for token in [
        "OscillatorKind",
        "normalize_oscillator",
        "normalize_series",
        "MarketRegime",
        "PineEngine",
        "detect_regime",
    ] {
        forbid_in_src(token, "trading-domain vocabulary lives in the terminal crate");
    }
}

#[test]
fn display_floats_never_parse_back_into_money() {
    for token in ["rust_decimal", "Decimal::", "parse::<Decimal>", "Decimal>"] {
        forbid_in_src(token, "the Decimal -> f64 boundary is one-way");
    }
}

#[test]
fn frame_holds_no_per_widget_machinery() {
    // Channels belong to pools, never to widgets: `ResultPool` owns its
    // oneshot-per-submit internally; paint code holds no channel at all.
    forbid_in_src("oneshot", "no per-widget channels on the frame path");
    forbid_in_src("tokio::spawn", "tasks are Scope-owned, never raw-spawned");
    forbid_in_src("block_on", "no sync-over-async bridges in paint code");
    forbid_in_src("std::fs", "no blocking file I/O in paint code");
    forbid_in_src("std::net", "no blocking network I/O in paint code");
    forbid_in_src("tokio::time::sleep", "no sleeps in paint code");
    forbid_in_src("thread::sleep", "no sleeps in paint code");
    forbid_in_src("Middleware", "no middleware layer per frame");
    // Timing lives in the terminal crate (plan-build timing over the domain
    // topology): no scaffolding module may name the wrapper.
    forbid_in_src("Instrumented", "timing covers domain plan builds in the terminal crate only");
    forbid_in_src("LatencyHistogram", "timing covers domain plan builds in the terminal crate only");
}

#[test]
fn timing_is_observed_never_asserted() {
    // Gates pin shape; the bench harness owns wall-time. No gate may sprout
    // a stopwatch: tests assert structure, benches record numbers.
    for entry in std::fs::read_dir(manifest_dir().join("tests")).expect("tests list") {
        let entry = entry.expect("test entry");
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        // This ledger names the forbidden tokens in its own literals, so it
        // exempts itself: the rule binds gate code, not the ledger text.
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
        manifest_dir().join("benches").join("frame_budget.rs").is_file(),
        "frame budgets ride the bench harness, which must exist"
    );
}

#[test]
fn theme_ships_no_embedded_assets() {
    // No icon catalog, no SVG embedding, no baked-in blobs: the theme is
    // six role-named hex strings resolved at paint time, lazily by
    // construction (nothing to load until a frame asks for a color).
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
