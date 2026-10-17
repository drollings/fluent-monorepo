//! Shell-contract pins for the demo collapse: `--check` golden outputs,
//! legacy CSV slot rows through the unified loader, and the deletion gate
//! for the twins earlier milestones made redundant.
//!
//! The golden runner executes the built example binaries directly (no cargo
//! recursion): `current_exe` is `target/<profile>/deps/<hash>`, so the
//! examples sit at `target/<profile>/examples/`. The examples only build
//! under their features (`demo`, `server-gateway`), so a feature-off run
//! skips with a message instead of failing — the M10 gate runs
//! `--all-features`, where the pin is enforced.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example_binary(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let binary = exe.parent()?.parent()?.join("examples").join(name);
    binary.is_file().then_some(binary)
}

fn run_check(binary: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new(binary)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run {}: {error}", binary.display()));
    assert!(
        output.status.success(),
        "{} {:?} failed: {}",
        binary.display(),
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "the check contract is stdout-only: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("check output is UTF-8")
}

fn golden(name: &str) -> String {
    std::fs::read_to_string(manifest_dir().join("tests/data").join(name))
        .unwrap_or_else(|error| panic!("read golden {name}: {error}"))
}

/// `--check` contracts pinned as golden text: the chart shell's directory
/// and legacy-CSV outputs plus the mock terminal's trading-view output.
/// Repointing `make protopyne-demo` must reproduce these byte-for-byte.
#[test]
fn check_contracts_are_pinned() {
    let Some(demo) = example_binary("demo") else {
        eprintln!("skip: demo example not built (needs --features demo)");
        return;
    };
    let Some(terminal) = example_binary("terminal") else {
        eprintln!("skip: terminal example not built (needs server-gateway,demo)");
        return;
    };
    assert_eq!(run_check(&demo, &["--check"]), golden("demo_check.txt"));
    let csv = manifest_dir()
        .join("../../data/protopyne-rs/historical/SPY_15M.csv")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        run_check(&demo, &["--check", csv.as_str()]),
        golden("demo_csv_check.txt"),
        "the legacy CSV contract"
    );
    assert_eq!(run_check(&terminal, &["--mock", "--check"]), golden("terminal_mock_check.txt"));
}

/// Legacy CSV slots through the unified loader: each file the old slot map
/// named must yield byte-identical rows through `load_bars_file` + `rows_of`
/// after the deletion. Counts plus boundary bars pin the whole series.
#[test]
fn legacy_csv_slots_match_unified_loader() {
    use protopyne_chart_model::rows_of;
    let dir = manifest_dir().join("../../data/protopyne-rs/historical");
    for (file, count, first, last) in [
        ("SPY_1M.csv", 436, (1767974400, 692.06), (1768006440, 694.4)),
        ("SPY_5M.csv", 346, (1767873600, 688.03), (1768006200, 694.4)),
        ("SPY_15M.csv", 116, (1767873600, 688.209), (1768005900, 694.4)),
    ] {
        let bars = protopyne_server::load_bars_file(&dir.join(file))
            .unwrap_or_else(|error| panic!("load {file}: {error}"));
        let rows = rows_of(&bars).rows;
        assert_eq!(rows.len(), count, "{file}: row count");
        assert_eq!((rows.first().expect("rows").time, rows.first().expect("rows").close), (first.0, first.1), "{file}: first bar");
        assert_eq!((rows.last().expect("rows").time, rows.last().expect("rows").close), (last.0, last.1), "{file}: last bar");
    }
}

/// Deletion gate: the twins earlier milestones made redundant stay gone.
/// Identifier-exact (a `sma_params` or a comment mentioning a name is not a
/// twin), over the library and both example shells.
#[test]
fn deleted_twins_stay_deleted() {
    const DELETED: &[&str] = &[
        "native_slots",
        "load_rows",
        "SUPPORTED_MA_KINDS",
        "ma_params",
        "kernel_params",
        "close_frame",
        "kernel_series_for",
    ];
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut hits = Vec::new();
    for entry in walk_rs(&manifest_dir().join("src")) {
        let text = std::fs::read_to_string(&entry).expect("read source");
        for token in DELETED {
            if text.split(|c| !is_ident(c)).any(|word| word == *token) {
                hits.push(format!("{} names {token}", entry.display()));
            }
        }
    }
    for example in ["demo.rs", "terminal.rs"] {
        let entry = manifest_dir().join("examples").join(example);
        let text = std::fs::read_to_string(&entry).expect("read example");
        for token in ["native_slots", "load_rows"] {
            if text.split(|c| !is_ident(c)).any(|word| word == token) {
                hits.push(format!("{} names {token}", entry.display()));
            }
        }
    }
    assert!(hits.is_empty(), "deleted twins resurrected: {hits:?}");
}

fn walk_rs(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).expect("read dir");
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}
