use super::*;

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace_root() -> PathBuf {
    manifest_dir().join("..").join("..")
}

fn read_text(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn protopyne_crate_dirs() -> Vec<PathBuf> {
    let src = workspace_root().join("src");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&src)
        .unwrap_or_else(|e| panic!("list {}: {e}", src.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("protopyne-"))
        })
        .collect();
    dirs.sort();
    assert!(
        !dirs.is_empty(),
        "expected protopyne-* crates under {}",
        src.display()
    );
    dirs
}

/// Paint-edge crates: the only crates allowed to name paint dependencies.
/// `protopyne-egui` is the trading terminal over this scaffolding.
fn is_paint_edge_crate(dir: &std::path::Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "protopyne-egui")
}

#[test]
fn links_against_paint_dependency() {
    assert!(is_paint_edge());
}

#[test]
fn crate_is_a_workspace_member() {
    let text = read_text(&workspace_root().join("Cargo.toml"));
    assert!(
        text.contains("src/fluent-egui"),
        "workspace members must list src/fluent-egui"
    );
    assert!(
        text.contains("src/protopyne-egui"),
        "workspace members must list src/protopyne-egui"
    );
}

#[test]
fn own_manifest_declares_paint_deps_and_no_autotests() {
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    assert!(
        text.contains("autotests = false"),
        "autotests must stay off"
    );
    assert!(
        text.contains("egui"),
        "paint dependency must be declared here"
    );
    assert!(text.contains("eframe"), "demo shell must be declared here");
    assert!(
        text.contains("optional = true"),
        "demo shell must stay optional so the default build needs no windowing backend"
    );
}

#[test]
fn lib_forbids_unsafe_and_documents_one_way_boundary() {
    let text = read_text(&manifest_dir().join("src").join("lib.rs"));
    assert!(
        text.contains("forbid(unsafe_code)"),
        "paint edge must forbid unsafe"
    );
    assert!(
        text.contains("one-way"),
        "boundary direction must be documented"
    );
    assert!(
        text.contains("Decimal"),
        "money side of the boundary must be named"
    );
    assert!(
        text.contains("f64"),
        "display side of the boundary must be named"
    );
    assert!(
        !text.contains("pub use egui"),
        "paint dependency must not be re-exported from the crate root"
    );
}

#[test]
fn no_trading_or_headless_crate_depends_on_paint() {
    for dir in protopyne_crate_dirs() {
        if is_paint_edge_crate(&dir) {
            continue;
        }
        let manifest = dir.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let text = read_text(&manifest);
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') || trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !trimmed.contains("fluent-egui"),
                "{} must not depend on the paint edge: {trimmed}",
                dir.display()
            );
            assert!(
                !trimmed.contains("protopyne-egui"),
                "{} must not depend on the paint edge: {trimmed}",
                dir.display()
            );
            assert!(
                !trimmed.contains("egui"),
                "{} must not depend on paint crates: {trimmed}",
                dir.display()
            );
            assert!(
                !trimmed.contains("eframe"),
                "{} must not depend on paint crates: {trimmed}",
                dir.display()
            );
        }
    }
}
