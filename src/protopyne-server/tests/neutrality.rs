//! Adapter-boundary audit: no server/view/egui library code names a
//! TastyTrade-specific type outside its adapter, and the venue links no C++.
//!
//! The audit scans library sources only. Tests may name adapter types (they
//! exercise the adapter), and the server binary's `main.rs` is the explicit
//! composition root: `--live` opts into one named venue, and the M14 chaos
//! suite owns that path. Everything importable — every route, view, and
//! widget — must stay venue-neutral so an IB-shaped broker drops into the
//! same seams.

use std::path::{Path, PathBuf};

fn package_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(root).expect("sources list");
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Whether the line names a TastyTrade-specific type: a `tastytrade::` path
/// or a `Tastytrade` CamelCase type. Lowercase venue spellings (broker ids,
/// env names, log text) are directory data, not types, and stay out of this.
fn names_tastytrade_type(line: &str) -> bool {
    if line.contains("tastytrade::") {
        return true;
    }
    let mut rest = line;
    while let Some(pos) = rest.find("Tastytrade") {
        let after = &rest[pos + "Tastytrade".len()..];
        if after.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return true;
        }
        rest = after;
    }
    false
}

#[test]
fn no_tastytrade_types_outside_the_adapter() {
    let roots = [
        package_dir().join("src"),
        package_dir().join("../protopyne-view/src"),
        package_dir().join("../protopyne-egui/src"),
    ];
    let mut violations = Vec::new();
    for root in &roots {
        let mut files = Vec::new();
        rs_files(root, &mut files);
        for file in files {
            if file
                .components()
                .any(|part| part.as_os_str() == "tests" || part.as_os_str() == "examples")
            {
                continue;
            }
            if file.file_name().is_some_and(|name| name == "main.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).expect("source reads");
            for (index, line) in text.lines().enumerate() {
                if names_tastytrade_type(line) {
                    violations.push(format!("{}:{}: {line}", file.display(), index + 1));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "venue-neutral code names TastyTrade types:\n{}",
        violations.join("\n")
    );
}

#[test]
fn venue_links_no_cpp() {
    let venue = package_dir().join("../protopyne-venue");
    assert!(
        !venue.join("build.rs").exists(),
        "the venue builds with cargo alone"
    );
    let manifest = std::fs::read_to_string(venue.join("Cargo.toml")).expect("venue manifest reads");
    for line in manifest.lines() {
        let lowered = line.to_ascii_lowercase();
        assert!(
            !lowered.contains("cxx") && !lowered.contains("cc =") && !lowered.contains("links"),
            "no C++ linkage in the venue manifest: {line}"
        );
    }
}
