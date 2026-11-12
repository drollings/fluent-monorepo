//! Workspace file → narrowest-gate map for automation.
//!
//! One checked-in table (path prefix → package → make component) read by
//! orchestrators and the isolated-copy verifier. Matrix packages resolve to
//! their component's fast gate; capability crates outside the make matrix
//! resolve to their own package gate; anything else takes the documented
//! default (`make smoke`). The component set is pinned against
//! `component_snapshot.json` — update that file with a reason, never the
//! table silently.

/// One table row: workspace-relative prefix, its Cargo package, and the
/// make-matrix component owning it (`None` outside the matrix).
struct Entry {
    prefix: &'static str,
    package: &'static str,
    component: Option<&'static str>,
}

// Inverted from the Makefile's `<component>_PKGS` map plus the package
// names in each `src/*/Cargo.toml`. Boundary matching keeps
// `src/fluent-wvr` from swallowing `src/fluent-wvr-macros`.
const ENTRIES: &[Entry] = &[
    Entry {
        prefix: "src/common-core",
        package: "common-core",
        component: Some("common-core"),
    },
    Entry {
        prefix: "src/fluent-wvr-macros",
        package: "fluent-wvr-macros",
        component: Some("fluent-wvr"),
    },
    Entry {
        prefix: "src/fluent-wvr-testutil",
        package: "fluent-wvr-testutil",
        component: None,
    },
    Entry {
        prefix: "src/fluent-wvr",
        package: "fluent-wvr",
        component: Some("fluent-wvr"),
    },
    Entry {
        prefix: "src/fluent-concurrency",
        package: "fluent-concurrency",
        component: Some("fluent-concurrency"),
    },
    Entry {
        prefix: "src/dag",
        package: "fluent-dag",
        component: Some("dag"),
    },
    Entry {
        prefix: "src/fluent-egui",
        package: "fluent-egui",
        component: Some("fluent-egui"),
    },
    Entry {
        prefix: "src/router",
        package: "fluent-router",
        component: Some("coral-router"),
    },
    Entry {
        prefix: "src/bin/coral-router",
        package: "coral-router",
        component: Some("coral-router"),
    },
    Entry {
        prefix: "src/guidance",
        package: "guidance-core",
        component: Some("guidance"),
    },
    Entry {
        prefix: "src/bin/guidance",
        package: "guidance",
        component: Some("guidance"),
    },
    Entry {
        prefix: "src/ontology",
        package: "guidance-ontology",
        component: Some("guidance"),
    },
    Entry {
        prefix: "src/coral",
        package: "coral-context",
        component: Some("coral"),
    },
    Entry {
        prefix: "src/bin/coral",
        package: "coral",
        component: Some("coral"),
    },
    Entry {
        prefix: "src/protopyne-types",
        package: "protopyne-types",
        component: Some("protopyne"),
    },
    Entry {
        prefix: "src/protopyne-kernels",
        package: "protopyne-kernels",
        component: Some("protopyne"),
    },
    Entry {
        prefix: "src/protopyne-store",
        package: "protopyne-store",
        component: Some("protopyne"),
    },
    Entry {
        prefix: "src/protopyne-strategy",
        package: "protopyne-strategy",
        component: Some("protopyne"),
    },
    Entry {
        prefix: "src/protopyne-venue",
        package: "protopyne-venue",
        component: Some("protopyne"),
    },
    Entry {
        prefix: "src/protopyne-server",
        package: "protopyne-server",
        component: Some("protopyne"),
    },
    Entry {
        prefix: "src/protopyne-egui",
        package: "protopyne-egui",
        component: Some("protopyne-egui"),
    },
    Entry {
        prefix: "src/protopyne-chart-model",
        package: "protopyne-chart-model",
        component: None,
    },
    Entry {
        prefix: "src/protopyne-chart-scale",
        package: "protopyne-chart-scale",
        component: None,
    },
    Entry {
        prefix: "src/protopyne-view",
        package: "protopyne-view",
        component: None,
    },
    Entry {
        prefix: "src/orator",
        package: "orator",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-voice",
        package: "orator-voice",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-agent",
        package: "orator-agent",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-audio",
        package: "orator-audio",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-memory",
        package: "orator-memory",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-supervise",
        package: "orator-supervise",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-models",
        package: "orator-models",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-desktop",
        package: "orator-desktop",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-testkit",
        package: "orator-testkit",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-daemon",
        package: "orator-daemon",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/orator-cli",
        package: "orator-cli",
        component: Some("orator"),
    },
    Entry {
        prefix: "src/concept",
        package: "fluent-concept",
        component: None,
    },
    Entry {
        prefix: "src/db",
        package: "fluent-db",
        component: None,
    },
    Entry {
        prefix: "src/knowledge",
        package: "fluent-knowledge",
        component: None,
    },
    Entry {
        prefix: "src/llm",
        package: "fluent-llm",
        component: None,
    },
    Entry {
        prefix: "src/memory-plugin",
        package: "memory-plugin",
        component: None,
    },
    Entry {
        prefix: "src/search-vector",
        package: "search-vector",
        component: None,
    },
    Entry {
        prefix: "src/spacy-rs",
        package: "spacy-rs",
        component: None,
    },
    Entry {
        prefix: "src/types",
        package: "fluent-types",
        component: None,
    },
    Entry {
        prefix: "src/fluent-onnx",
        package: "fluent-onnx",
        component: None,
    },
    Entry {
        prefix: "src/wasm_ipc",
        package: "fluent-wasm-ipc",
        component: None,
    },
    Entry {
        prefix: "src/vector-math",
        package: "vector-math",
        component: None,
    },
    Entry {
        prefix: "src/xtask",
        package: "xtask",
        component: None,
    },
    Entry {
        prefix: "src/rdf",
        package: "guidance-rdf",
        component: None,
    },
    Entry {
        prefix: "src/bin/yamake-coral",
        package: "yamake-coral",
        component: None,
    },
];

/// Narrowest gate for one workspace path.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Narrowest gate for one workspace path.
pub struct ComponentGate {
    /// Owning Cargo package, empty when the path maps to nothing.
    pub package: String,
    /// Make-matrix component, `None` outside the matrix.
    pub component: Option<String>,
    /// Command to run: `<component>-smoke`, `cargo test -p <package>`,
    /// or the documented `make smoke` default.
    pub gate: String,
}

/// Resolve `path` (workspace-relative, leading `./` tolerated) to its
/// narrowest gate by longest boundary-matching prefix.
#[must_use]
/// Resolve `path` (workspace-relative, leading `./` tolerated) to its
/// narrowest gate by longest boundary-matching prefix.
pub fn component_gate_for_path(path: &str) -> ComponentGate {
    let normalized = path.trim().trim_start_matches("./").trim_end_matches('/');
    let mut best: Option<&Entry> = None;
    for entry in ENTRIES {
        let matches =
            normalized == entry.prefix || normalized.starts_with(&format!("{}/", entry.prefix));
        if matches && best.is_none_or(|b: &Entry| entry.prefix.len() > b.prefix.len()) {
            best = Some(entry);
        }
    }
    match best {
        Some(entry) => match entry.component {
            Some(component) => ComponentGate {
                package: entry.package.to_string(),
                component: Some(component.to_string()),
                gate: format!("{component}-smoke"),
            },
            None => ComponentGate {
                package: entry.package.to_string(),
                component: None,
                gate: format!("cargo test -p {}", entry.package),
            },
        },
        // Documented default: safe over-approximation, never a wrong gate.
        None => ComponentGate {
            package: String::new(),
            component: None,
            gate: "make smoke".to_string(),
        },
    }
}

/// Distinct components in the table (unsorted; tests pin the set).
#[must_use]
/// Distinct components in the table (unsorted; tests pin the set).
pub fn snapshot_components() -> Vec<String> {
    let mut components: Vec<String> = ENTRIES
        .iter()
        .filter_map(|entry| entry.component.map(str::to_string))
        .collect();
    components.sort();
    components.dedup();
    components
}

/// Components recorded in the checked-in snapshot file (sorted).
/// Update that file with a reason when the matrix changes; the test
/// failure names the file so the fix is mechanical.
#[must_use]
/// Components recorded in the checked-in snapshot file (sorted).
/// Update that file with a reason when the matrix changes; the test
/// failure names the file so the fix is mechanical.
pub fn load_snapshot_components() -> Vec<String> {
    let path = format!("{}/component_snapshot.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).expect("snapshot readable");
    let value: serde_json::Value = serde_json::from_str(&text).expect("snapshot parses");
    let mut components: Vec<String> = value["components"]
        .as_array()
        .expect("components array")
        .iter()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect();
    components.sort();
    components
}

#[cfg(test)]
#[path = "../tests/component_gate_map.rs"]
mod component_gate_map_tests;