use super::{component_gate_for_path, prefix_for_package, snapshot_components};

#[test]
fn package_prefixes_resolve_for_scope_globs() {
    assert_eq!(prefix_for_package("guidance-core"), Some("src/guidance"));
    assert_eq!(prefix_for_package("fluent-router"), Some("src/router"));
    assert_eq!(
        prefix_for_package("search-vector"),
        Some("src/search-vector")
    );
    assert_eq!(prefix_for_package("no-such-package"), None);
}

// Every matrix component resolves through at least one source prefix,
// and the narrowest gate is the component's fast gate.
#[test]
fn matrix_prefixes_resolve_to_component_smoke_gates() {
    for (path, component) in [
        ("src/guidance/src/lib.rs", "guidance"),
        ("src/bin/guidance/src/main.rs", "guidance"),
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

#[test]
fn paths_for_package_uses_the_single_entry_table() {
    let paths = crate::component_map::paths_for_package("guidance-core");
    assert_eq!(paths, vec!["src/guidance".to_string()]);
    let paths = crate::component_map::paths_for_package("guidance");
    assert!(paths.contains(&"src/bin/guidance".to_string()), "{paths:?}");
}

#[test]
fn scope_names_crate_mentions_without_guessing() {
    use crate::component_map::scope_packages_for_query;
    // Exact package, directory, and component mentions resolve.
    assert_eq!(
        scope_packages_for_query("where is clean_build_cache in guidance"),
        vec!["guidance-core".to_string(), "guidance".to_string()]
    );
    assert_eq!(
        scope_packages_for_query("which crate owns calibration in common-core"),
        // Explicit common-core first; `core` weakly seconds
        // guidance-core (soft scope: render order only, recall stays
        // whole).
        vec!["common-core".to_string(), "guidance-core".to_string()]
    );
    // Substring mentions resolve weakly; exact beats weak.
    let scope = scope_packages_for_query("how does the router ladder work");
    assert!(
        scope.contains(&"fluent-router".to_string()),
        "router names fluent-router: {scope:?}"
    );
    // Nothing named: no scope instead of a guess. Recall stays
    // unscoped rather than filtering on a hallucinated crate.
    assert!(scope_packages_for_query("which crate owns calibration").is_empty());
    assert!(scope_packages_for_query("how does sync decide what to reprocess").is_empty());
    // Control-shaped nonsense stays silent.
    assert!(scope_packages_for_query("files in crate qqqNoSuchCrate").is_empty());
    // At most two packages, strongest first; ties keep table order
    // (common-core heads the table; `router` exactly names src/router's
    // directory, so fluent-router outranks guidance-core by position).
    let scope = scope_packages_for_query("guidance common-core router sync");
    assert_eq!(
        scope,
        vec!["common-core".to_string(), "fluent-router".to_string()],
        "{scope:?}"
    );
}
