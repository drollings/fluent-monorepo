use super::{component_gate_for_path, snapshot_components};

// Every matrix component resolves through at least one source prefix,
// and the narrowest gate is the component's fast gate.
#[test]
fn matrix_prefixes_resolve_to_component_smoke_gates() {
    for (path, component) in [
        ("src/guidance/src/lib.rs", "guidance"),
        ("src/bin/guidance/src/main.rs", "guidance"),
        ("src/ontology/src/lib.rs", "guidance"),
        ("src/common-core/src/lib.rs", "common-core"),
        ("src/fluent-wvr/src/lib.rs", "fluent-wvr"),
        ("src/fluent-wvr-macros/src/lib.rs", "fluent-wvr"),
        ("src/fluent-concurrency/src/lib.rs", "fluent-concurrency"),
        ("src/dag/src/lib.rs", "dag"),
        ("src/fluent-egui/src/lib.rs", "fluent-egui"),
        ("src/router/src/lib.rs", "coral-router"),
        ("src/bin/coral-router/src/main.rs", "coral-router"),
        ("src/coral/src/lib.rs", "coral"),
        ("src/bin/coral/src/main.rs", "coral"),
        ("src/protopyne-server/src/lib.rs", "protopyne"),
        ("src/protopyne-egui/src/lib.rs", "protopyne-egui"),
        ("src/orator/src/lib.rs", "orator"),
    ] {
        let resolved = component_gate_for_path(path);
        assert_eq!(resolved.component.as_deref(), Some(component), "{path}");
        assert_eq!(resolved.gate, format!("{component}-smoke"), "{path}");
    }
}

// Capability crates outside the make matrix resolve to their own
// package gate — never to an unrelated component's gate.
#[test]
fn non_matrix_prefixes_resolve_to_package_gates() {
    for (path, package) in [
        ("src/llm/src/decision/ladder.rs", "fluent-llm"),
        ("src/search-vector/src/lib.rs", "search-vector"),
        ("src/types/src/lib.rs", "fluent-types"),
        ("src/concept/src/lib.rs", "fluent-concept"),
    ] {
        let resolved = component_gate_for_path(path);
        assert_eq!(resolved.component, None, "{path}");
        assert_eq!(resolved.package, package, "{path}");
        assert_eq!(resolved.gate, format!("cargo test -p {package}"), "{path}");
    }
}

// Unknown paths take the documented default: no component, repo-wide
// fast gate. A safe over-approximation, never a wrong narrow gate.
#[test]
fn unknown_paths_resolve_to_documented_default() {
    for path in ["doc/guidance/VISION.md", "Makefile", "src", ".", ""] {
        let resolved = component_gate_for_path(path);
        assert_eq!(resolved.component, None, "{path}");
        assert_eq!(resolved.gate, "make smoke", "{path}");
    }
}

// The table's component set matches the checked-in snapshot of the
// Makefile's component list. A Makefile component change must update
// the snapshot with a recorded reason — never silently.
#[test]
fn component_set_matches_snapshot() {
    let mut table = snapshot_components();
    table.sort();
    table.dedup();
    let snapshot = super::load_snapshot_components();
    assert_eq!(table, snapshot);
}

// Every workspace crate directory is covered by the table, so a new
// crate without a table entry fails here instead of silently taking
// the default gate.
#[test]
fn every_workspace_crate_has_a_table_entry() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.parent().expect("src dir");
    let mut missing = Vec::new();
    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(src)
        .expect("read src")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("Cargo.toml").is_file())
        .collect();
    if src.join("bin").is_dir() {
        dirs.extend(
            std::fs::read_dir(src.join("bin"))
                .expect("read src/bin")
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_dir() && path.join("Cargo.toml").is_file()),
        );
    }
    for dir in dirs {
        let relative = dir
            .strip_prefix(src.parent().expect("workspace root"))
            .expect("relative");
        let prefix = relative.to_string_lossy().replace('\\', "/");
        let resolved = component_gate_for_path(&format!("{prefix}/lib.rs"));
        if resolved.gate == "make smoke" {
            missing.push(prefix);
        }
    }
    assert!(missing.is_empty(), "unmapped crate dirs: {missing:?}");
}
