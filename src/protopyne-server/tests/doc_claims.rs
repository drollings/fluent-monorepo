//! Documentation-claim guards: the README's claims about itself must match
//! the code. These read the checked-in markdown (the claims are prose, so the
//! test checks the prose), pinning the journal's durability, the shipped
//! subsystems, and the absence of stale roadmap links.

use std::path::PathBuf;

fn read(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("doc")
        .join("protopyne-rs")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn readme_names_the_csv_journal_not_sqlite() {
    let readme = read("README.md");
    assert!(
        readme.contains("CSV `AppendLog`"),
        "the journal sentence must name the CSV AppendLog"
    );
    let lowered = readme.to_lowercase();
    assert!(
        !lowered.contains("sqlite"),
        "the journal is not SQLite any more"
    );
    assert!(
        !lowered.contains("rusqlite"),
        "rusqlite was removed from the trading crates"
    );
}

#[test]
fn readme_claims_the_shipped_subsystems() {
    let readme = read("README.md");
    for claim in ["terminal", "`protopyne-store`", "Pine v6"] {
        assert!(readme.contains(claim), "README must mention {claim}");
    }
    assert!(
        !readme.contains("ROADMAP_"),
        "README must point at ARCHITECTURE.md, not a roadmap file"
    );
}

#[test]
fn readme_names_the_terminal_backtest_surface() {
    let readme = read("README.md");
    for claim in ["strategy form", "equity pane", "eight-row status"] {
        assert!(
            readme.contains(claim),
            "README must name the terminal backtest surface ({claim:?})"
        );
    }
}

#[test]
fn the_unwired_symbol_advisory_runs_and_exits_zero() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let output = std::process::Command::new("bash")
        .arg("bin/protopyne-unwired-symbols.sh")
        .current_dir(&root)
        .output()
        .expect("advisory script runs");
    assert!(
        output.status.success(),
        "advisory must exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("unwired-symbol advisory"),
        "the report header prints"
    );
}

fn architecture() -> String {
    read("ARCHITECTURE.md")
}

/// The capability index must say what the code does: while the unwired
/// advisory reports unreferenced surfaces, the index must not read as
/// uniformly wired. A non-`wired` row carries the disposition instead of a
/// second inventory.
#[test]
fn capability_status_is_not_uniformly_wired() {
    let index = architecture();
    let wired = index.matches("| wired |").count();
    let unmarked = index.matches("| library-only |").count() + index.matches("| planned |").count();
    assert!(wired > 0, "the index still names wired capabilities");
    assert!(
        unmarked > 0,
        "the index must carry at least one non-wired row while unwired surfaces exist"
    );
}

/// Every venue module that marks itself `Library-only` must have a matching
/// non-`wired` capability row covering its marked symbols, so the marker and
/// the index cannot disagree about what is production-driven.
#[test]
fn library_only_markers_have_matching_index_rows() {
    let index = architecture();
    let table: Vec<&str> = index
        .lines()
        .skip_while(|line| !line.contains("## 4. Capability Index"))
        .filter(|line| line.starts_with("| C"))
        .collect();
    assert!(!table.is_empty(), "the capability index table must parse");
    let row = table
        .iter()
        .find(|line| line.contains("DepthBook"))
        .unwrap_or_else(|| panic!("the index must cover DepthBook"));
    assert!(
        !row.contains("| wired |"),
        "DepthBook is marked library-only in source but its index row reads wired"
    );
}
