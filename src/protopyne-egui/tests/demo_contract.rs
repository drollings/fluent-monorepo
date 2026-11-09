//! Shell-contract E2E: the built `demo`/`terminal` examples, run headless.
//!
//! Integration tier (`make test`, never `make smoke`): each case spawns
//! example binaries and waits on real subprocess I/O. The loading logic
//! itself stays pinned in the fast gate by the pure suites.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example_binary(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let binary = exe.parent()?.parent()?.join("examples").join(name);
    binary.is_file().then_some(binary)
}

fn run_check(binary: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new(binary)
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

/// Drop the volatile bar count from a directory-mode check line: the
/// monthlies under the git-ignored history root grow over time, so no
/// quantity is pinned — only that tracked code handles the use case.
fn without_bars(line: &str) -> String {
    let (head, tail) = line.split_once("bars=").expect("bars field");
    let end = tail.find(' ').expect("bars field end");
    format!("{head}bars={{}}{}", &tail[end..])
}

/// `--check` contracts pinned as golden text, minus the census count: the
/// chart shell's directory and legacy-CSV outputs plus the mock terminal's
/// trading-view output.
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
    // Directory mode loads the local monthlies, so no quantity is pinned —
    // only that tracked code handles the use case: the command succeeds
    // and renders the contract lines byte-for-byte apart from the count.
    let output = run_check(&demo, &["--check"]);
    let golden_text = golden("demo_check.txt");
    let mut lines = output.lines();
    let mut golden_lines = golden_text.lines();
    let first = lines.next().expect("check prints a summary line");
    let golden_first = golden_lines.next().expect("golden summary line");
    assert_eq!(without_bars(first), without_bars(golden_first));
    let rest: Vec<&str> = lines.collect();
    let golden_rest: Vec<&str> = golden_lines.collect();
    assert_eq!(rest, golden_rest, "tickers and notice lines");
    let csv = manifest_dir()
        .join("../../data/protopyne-rs/historical/SPY_15M.csv")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        run_check(&demo, &["--check", csv.as_str()]),
        golden("demo_csv_check.txt"),
        "the legacy CSV contract"
    );
    assert_eq!(
        run_check(&terminal, &["--mock", "--check"]),
        golden("terminal_mock_check.txt")
    );
}

/// Daily fold over the real history root: a session history folds to
/// daily bars under KeepPartial. Integration tier: walks the local
/// monthlies like the shell does.
#[test]
fn daily_fold_over_real_history_is_non_empty() {
    use protopyne_types::{FoldEdges, Timeframe};
    let dir = manifest_dir().join("../../data/protopyne-rs/historical");
    let Ok(root) = protopyne_server::HistoryRoot::open(&dir) else {
        eprintln!("skip: no history root at {}", dir.display());
        return;
    };
    let mut cache = protopyne_server::HistoryCache::new(root);
    let Ok(load) = cache.load_for("SPY") else {
        eprintln!("skip: no SPY history under {}", dir.display());
        return;
    };
    let frames = protopyne_server::fold_frames(
        &load.bars,
        protopyne_server::HISTORY_ROOT_BASE,
        &[Timeframe::D1],
        FoldEdges::KeepPartial,
    )
    .expect("fold SPY to daily");
    assert!(
        !frames[0].1.is_empty(),
        "a session history folds to daily bars under KeepPartial"
    );
}
