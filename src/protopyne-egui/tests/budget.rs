//! Audit ledger: allocation shape, dependency direction, and bloat.
//!
//! Same posture as the scaffolding ledger (shape and source text, never
//! wall-time; budgets observed in `benches/chart_budget.rs`), plus the
//! terminal-specific direction pins: the compute truth is read, never
//! forked (no second indicator table, no second regime constant, no second
//! error type); `strategy`/`venue` stay a dev-only round-trip proof — never
//! a main dependency — while the server enters only as an optional
//! composition-root dependency (the example outside `src/` names it; render
//! modules never do). Growth beyond this ledger fails loudly until the
//! ledger itself is edited with a justification — that edit is the review.

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

/// Every `*.rs` file under `src/`, recursively, as `src`-relative
/// slash-separated paths in sorted order. Recursion is deliberate: a render
/// submodule (e.g. `views/portfolio.rs`) must appear in the ledger, not slip
/// past because it lives one level down. The non-render trees (`tests/`,
/// `benches/`, `examples/`) are siblings of `src/`, so they are exempt by
/// construction and never enter this list.
fn src_file_names() -> Vec<String> {
    let mut names = Vec::new();
    collect_rs(&manifest_dir().join("src"), Path::new(""), &mut names);
    names.sort();
    names
}

fn collect_rs(dir: &Path, prefix: &Path, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("list {}: {e}", dir.display()));
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let rel = prefix.join(entry.file_name());
        if path.is_dir() {
            collect_rs(&path, &rel, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
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
            "grant_form.rs",
            "integration_lite.rs",
            "lib.rs",
            "oscillators.rs",
            "pine_overlay.rs",
            "regime_labels.rs",
            "session.rs",
            "topology.rs",
            "views/cache.rs",
            "views/health.rs",
            "views/journal.rs",
            "views/mod.rs",
            "views/portfolio.rs",
            "widgets.rs",
        ],
        "a new source file must justify itself by editing this ledger"
    );
}

/// Parse the `[dependencies]` key list from the crate manifest.
///
/// Handles both TOML spellings of the same set — inline keys under
/// `[dependencies]` and per-dependency table headers `[dependencies.NAME]` —
/// and tracks inline-table nesting so a multi-line value's continuation lines
/// are never mistaken for new dependency names.
fn main_dep_keys() -> Vec<String> {
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    let mut keys = Vec::new();
    let mut in_deps = false;
    let mut depth = 0i32;
    for line in text.lines().map(str::trim) {
        if depth == 0 && line.starts_with('[') {
            if let Some(rest) = line.strip_prefix("[dependencies") {
                if let Some(name) = rest.strip_prefix('.').and_then(|n| n.strip_suffix(']')) {
                    if !name.is_empty() {
                        keys.push(name.trim().to_owned());
                    }
                    in_deps = false;
                } else {
                    in_deps = line == "[dependencies]";
                }
            } else {
                in_deps = false;
            }
            continue;
        }
        if !in_deps {
            continue;
        }
        if depth == 0 && !line.is_empty() && !line.starts_with('#') {
            let key = line.split('=').next().unwrap_or("").trim();
            if !key.is_empty() {
                keys.push(key.to_owned());
            }
        }
        depth = (depth + bracket_delta(line)).max(0);
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Net inline-table depth on one line, ignoring quoted strings and trailing
/// `#` comments, so nested manifests parse without a TOML dependency.
fn bracket_delta(line: &str) -> i32 {
    let mut delta = 0i32;
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '#' => break,
                '{' | '[' => delta += 1,
                '}' | ']' => delta -= 1,
                _ => {}
            },
        }
    }
    delta
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
            "protopyne-server",
            "protopyne-types",
            "protopyne-view",
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
    for trading in ["protopyne-strategy", "protopyne-venue"] {
        assert!(
            !deps_section.contains(trading),
            "{trading} must not enter main dependencies (trading path stays a dev-only round-trip proof)"
        );
    }
    // The server is the one exception, and only as an optional dependency:
    // the composition-root example outside `src/` constructs `ServerGateway`
    // behind the `server-gateway` feature, while render modules keep reading
    // through the gateway abstraction (pinned by
    // `terminal_never_mutates_the_trading_path` and the gateway ledger test).
    let server_line = deps_section
        .lines()
        .find(|line| line.contains("protopyne-server"))
        .expect("the composition root declares its server dependency");
    assert!(
        server_line.contains("optional = true"),
        "protopyne-server must stay optional so the default build never links the trading path \
         (line: {server_line})"
    );
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
fn display_fence_marks_kernel_preview_frames() {
    // Positive pin: the preview boundary fences display legs — the fence
    // type must be named exactly where display floats enter kernel frames,
    // so a future refactor cannot silently drop the mark.
    assert!(
        code_lines(&read_src("integration_lite.rs")).iter().any(|line| line.contains("DisplayOnly")),
        "integration_lite.rs must route preview frames through the DisplayOnly fence"
    );
}

#[test]
fn display_escape_stays_out_of_decision_crates() {
    // The fence's type and module path must never be named outside the
    // paint crates and its home crate: display floats cross into kernel
    // preview frames only at the terminal, never into decisions.
    let root = manifest_dir().join("..");
    for dir in [
        "protopyne-types",
        "protopyne-kernels",
        "protopyne-strategy",
        "protopyne-venue",
        "protopyne-server",
        "protopyne-chart-scale",
    ] {
        for path in walk_files(&root.join(dir)) {
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let text = read_text(&path);
            for line in code_lines(&text) {
                for token in ["DisplayOnly", "display_only"] {
                    assert!(
                        !line.contains(token),
                        "{} must not name the display escape {token:?} (line: {line})",
                        path.display()
                    );
                }
            }
        }
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
