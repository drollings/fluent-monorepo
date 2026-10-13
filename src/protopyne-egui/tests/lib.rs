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

#[test]
fn links_against_paint_dependency_and_scaffolding() {
    assert!(is_terminal_edge());
    assert!(fluent_egui::is_paint_edge());
}

#[test]
fn crate_is_a_workspace_member() {
    let text = read_text(&workspace_root().join("Cargo.toml"));
    assert!(
        text.contains("src/protopyne-egui"),
        "workspace members must list src/protopyne-egui"
    );
}

#[test]
fn own_manifest_declares_paint_deps_and_no_autotests() {
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    assert!(text.contains("autotests = false"), "autotests must stay off");
    assert!(text.contains("egui"), "paint dependency must be declared here");
    assert!(text.contains("eframe"), "demo shell must be declared here");
    assert!(
        text.contains("optional = true"),
        "demo shell must stay optional so the default build needs no windowing backend"
    );
    assert!(text.contains("fluent-egui"), "terminal composes the scaffolding, never forks it");
}

#[test]
fn lib_forbids_unsafe_and_documents_one_way_boundary() {
    let text = read_text(&manifest_dir().join("src").join("lib.rs"));
    assert!(text.contains("forbid(unsafe_code)"), "paint edge must forbid unsafe");
    assert!(text.contains("one-way"), "boundary direction must be documented");
    assert!(text.contains("Decimal"), "money side of the boundary must be named");
    assert!(text.contains("f64"), "display side of the boundary must be named");
    assert!(
        !text.contains("pub use egui"),
        "paint dependency must not be re-exported from the crate root"
    );
}
