//! Repository policy: every `#[ignore]` in a trading crate carries a reason
//! drawn from one closed vocabulary, so a gate can tell a slow probe from a
//! data- or credential-missing one without re-reading the test.
//!
//! The scan is textual on purpose. A compiled test target sees only the
//! feature combination it was built with, so it cannot police probes that live
//! behind `pine`, `research`, or `live-ai`.
//!
//! # The three approved reasons
//!
//! - `soak: …` — correct but slow, or heavy enough to belong to the soak tier.
//!   Must say what is slow or what the regular tier already pins.
//! - `live-ai: …` — needs credentials or a live endpoint. Only reachable
//!   through `make test-live` / `make venue-test-live`.
//! - `generator: …` — not a probe at all: writes fixture files. Never trusted
//!   as evidence.
//!
//! A reason that merely names a missing file, a missing payload, or a
//! historical condition is not on this list. If a probe is a correctness probe,
//! it belongs in the regular gate.

use std::path::{Path, PathBuf};

/// Prefixes an `#[ignore]` reason may begin with.
const APPROVED: [&str; 3] = ["soak: ", "live-ai: ", "generator: "];

/// The lowest number of attributes the tree is known to carry. A scan that
/// suddenly finds fewer has stopped matching, which would make this file pass
/// vacuously.
const TREE_FLOOR: usize = 8;

/// Extract the reason of every single-line `#[ignore]` attribute in `text`.
///
/// A bare `#[ignore]` yields an empty reason; an attribute whose `]` is not on
/// the same line yields `<multi-line>`.
#[must_use]
fn ignore_reasons(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("#[ignore") {
            continue;
        }
        if !trimmed.contains(']') {
            out.push("<multi-line>".to_owned());
            continue;
        }
        match (trimmed.find("= \""), trimmed.rfind('"')) {
            (Some(start), Some(end)) if end > start + 2 => {
                let reason = &trimmed[start + 3..end];
                let escaped = reason.replace("\\\"", "\"");
                out.push(escaped);
            }
            _ => out.push(String::new()),
        }
    }
    out
}

/// True when `reason` starts with one of [`APPROVED`] and says something after
/// the prefix.
#[must_use]
fn is_approved(reason: &str) -> bool {
    APPROVED.iter().any(|prefix| {
        reason
            .strip_prefix(prefix)
            .is_some_and(|rest| !rest.trim().is_empty())
    })
}

/// Collect every `.rs` file under `dir`, recursively.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => panic!("read {}: {error}", dir.display()),
    };
    for entry in entries {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Offending `(file, reason)` pairs under `<repo>/src`.
fn offenders() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("src");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&src).expect("repository src is readable") {
        let path = entry.expect("source entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.starts_with("protopyne-") {
            rs_files(&path, &mut files);
        }
    }
    let mut bad = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("source file is readable");
        for reason in ignore_reasons(&text) {
            if !is_approved(&reason) {
                bad.push((file.display().to_string(), reason));
            }
        }
    }
    bad.sort();
    bad
}

// ---------------------------------------------------------------------------
// The scanner itself
// ---------------------------------------------------------------------------

#[test]
fn scanner_reads_the_reason_of_a_single_line_attribute() {
    let text = "  #[ignore = \"soak: minutes in debug\"]\nfn heavy() {}\n";
    assert_eq!(
        ignore_reasons(text),
        vec!["soak: minutes in debug".to_owned()]
    );
}

#[test]
fn scanner_treats_a_bare_attribute_as_an_empty_reason() {
    assert_eq!(ignore_reasons("#[ignore]\n"), vec![String::new()]);
}

#[test]
fn scanner_treats_an_unterminated_attribute_as_multi_line() {
    assert_eq!(
        ignore_reasons("#[ignore = \"soak:\n"),
        vec!["<multi-line>".to_owned()]
    );
}

#[test]
fn scanner_ignores_doc_prose_that_mentions_the_attribute() {
    let text = "//! every test is `#[ignore]`d\n/// #[ignore] lives below\n";
    assert!(ignore_reasons(text).is_empty());
}

#[test]
fn retired_reasons_are_refused() {
    // Each of these was believed true at some point and was not. None of them
    // names a cost, a credential, or a fixture writer.
    for reason in [
        "requires trimmed CSV history fixtures no longer shipped in the tree",
        "needs data",
        "slow",
        "ignored for now",
    ] {
        assert!(!is_approved(reason), "{reason:?} must not be approved");
    }
}

#[test]
fn the_three_tiers_are_approved() {
    for reason in [
        "soak: minutes in debug",
        "live-ai: needs TASTYTRADE_*",
        "generator: writes goldens",
    ] {
        assert!(is_approved(reason), "{reason:?} must be approved");
    }
    assert!(!is_approved("soak: "), "a bare prefix carries no reason");
    assert!(!is_approved(""), "an empty reason is an offence");
}

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

#[test]
fn every_ignore_in_the_trading_crates_names_an_approved_reason() {
    let bad = offenders();
    assert!(
        bad.is_empty(),
        "{} `#[ignore]` attribute(s) name an unapproved reason:\n{:#?}",
        bad.len(),
        bad
    );
}

#[test]
fn the_scan_sees_the_attributes_that_exist() {
    // Guard against a silent no-match: the tree is known to carry at least
    // this many, so a scan that finds none is broken, not clean.
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("src");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&src).expect("repository src is readable") {
        let path = entry.expect("source entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.starts_with("protopyne-") {
            rs_files(&path, &mut files);
        }
    }
    let total: usize = files
        .iter()
        .map(|file| {
            let text = std::fs::read_to_string(file).expect("source file is readable");
            ignore_reasons(&text).len()
        })
        .sum();
    assert!(
        total >= TREE_FLOOR,
        "found {total} `#[ignore]` attributes across {} files; the scan has stopped matching",
        files.len()
    );
}
