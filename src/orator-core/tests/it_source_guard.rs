//! Source hygiene guards: project code never references the plan
//! that built it, every crate root forbids unsafe, and the
//! de-bloated dependency edges stay split.
//!
//! These scan the working tree (read-only). They fail on drift, not
//! on formatting.

use std::path::{Path, PathBuf};

/// Workspace `src/` holding every `orator-*` crate.
fn crates_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace src/")
        .to_path_buf()
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("readable dir");
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn is_word_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whole-word, case-insensitive matches of the forbidden plan words.
fn forbidden_words(source: &str) -> Vec<String> {
    const WORDS: [&str; 3] = [concat!("road", "map"), concat!("mile", "stone"), concat!("pha", "se")];
    let mut hits = Vec::new();
    for word in source
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(str::to_lowercase)
    {
        if WORDS.contains(&word.as_str()) {
            hits.push(word);
        }
    }
    hits
}

/// Whole-word `R` + digits references (`R0`–`R16` style), case-sensitive:
/// lowercase identifiers like `r2` are ordinary names, not plan ids.
fn plan_ids(source: &str) -> Vec<String> {
    plan_ids_with(source, b'R')
}

/// Whole-word `P` + digits references (`P0`–`P16` style), case-sensitive:
/// ordinary names like `Pcm` carry no digits and never match.
fn parity_ids(source: &str) -> Vec<String> {
    plan_ids_with(source, b'P')
}

/// Whole-word `Q` + digits references (`Q0`–`Q15` style), case-sensitive:
/// quantization tags like `Q4_0` carry a word char after the digits and
/// never match; the only `Q4` in prose was rewritten to `4-bit`.
fn section_ids(source: &str) -> Vec<String> {
    plan_ids_with(source, b'Q')
}

/// Whole-word `<letter>` + digits scanner shared by both plan-id kinds.
fn plan_ids_with(source: &str, letter: u8) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut hits = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == letter
            && (index == 0 || !is_word_char(bytes[index - 1]))
            && index + 1 < bytes.len()
            && bytes[index + 1].is_ascii_digit()
        {
            let mut end = index + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end >= bytes.len() || !is_word_char(bytes[end]) {
                hits.push(source[index..end].to_string());
            }
            index = end;
        } else {
            index += 1;
        }
    }
    hits
}

#[test]
fn no_plan_references_in_source() {
    let root = crates_src();
    let mut files = Vec::new();
    let entries = std::fs::read_dir(&root).expect("workspace src readable");
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && name.starts_with("orator-") {
            rs_files(&entry.path().join("src"), &mut files);
        }
    }
    assert!(!files.is_empty(), "scanned no source files");
    let mut violations = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).expect("source readable");
        for hit in forbidden_words(&source) {
            violations.push(format!("{}: forbidden word `{hit}`", path.display()));
        }
        for hit in plan_ids(&source) {
            violations.push(format!("{}: plan id `{hit}`", path.display()));
        }
        for hit in parity_ids(&source) {
            violations.push(format!("{}: parity id `{hit}`", path.display()));
        }
        for hit in section_ids(&source) {
            violations.push(format!("{}: section id `{hit}`", path.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "plan references in source:\n{}",
        violations.join("\n")
    );
}

#[test]
fn every_crate_root_forbids_unsafe() {
    let root = crates_src();
    let mut roots = Vec::new();
    let entries = std::fs::read_dir(&root).expect("workspace src readable");
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && name.starts_with("orator-") {
            for candidate in ["src/lib.rs", "src/main.rs"] {
                let path = entry.path().join(candidate);
                if path.is_file() {
                    roots.push(path);
                }
            }
        }
    }
    assert!(!roots.is_empty(), "found no crate roots");
    let mut violations = Vec::new();
    for path in &roots {
        let source = std::fs::read_to_string(path).expect("root readable");
        let first = source.lines().find(|line| !line.trim().is_empty());
        if first != Some("#![forbid(unsafe_code)]") {
            violations.push(format!("{}: missing `#![forbid(unsafe_code)]`", path.display()));
        }
    }
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn parity_scanner_catches_ids_and_ignores_ordinary_names() {
    assert_eq!(
        parity_ids("P0 P16"),
        vec!["P0".to_string(), "P16".to_string()]
    );
    assert!(
        parity_ids("Pcm PcmCodec open").is_empty(),
        "ordinary names carry no digits"
    );
    assert!(
        parity_ids("r2 p3").is_empty(),
        "lowercase never matches"
    );
    assert_eq!(
        parity_ids("XP10y"),
        Vec::<String>::new(),
        "embedded matches never fire"
    );
    assert_eq!(plan_ids("R0 R16").len(), 2, "R check kept");
    assert_eq!(
        section_ids("Q0 Q15"),
        vec!["Q0".to_string(), "Q15".to_string()]
    );
    assert!(
        section_ids("Q4_0 quant").is_empty(),
        "quantization tags never match"
    );
    assert!(
        section_ids("q5 Q").is_empty(),
        "lowercase and bare letters never match"
    );
}

/// Whether `manifest` declares a dependency literally named `name`:
/// line-based, so `tokio-tungstenite` never counts as `tungstenite`.
fn has_dep(manifest: &str, name: &str) -> bool {
    manifest.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with(name)
            && trimmed[name.len()..]
                .starts_with([' ', '=', '{', '\t'])
    })
}

#[test]
fn dependency_edges_stay_split() {
    let root = crates_src();
    let models = std::fs::read_to_string(root.join("orator-models/Cargo.toml"))
        .expect("models manifest readable");
    assert!(
        !models.contains("orator-s2s"),
        "orator-models must stay a leaf (no runtime-crate edge)"
    );
    let s2s =
        std::fs::read_to_string(root.join("orator-s2s/Cargo.toml")).expect("s2s manifest readable");
    assert!(
        !s2s.contains("orator-stt"),
        "orator-s2s must not depend on orator-stt (WAV lives in orator-audio)"
    );
    assert!(
        s2s.contains("orator-audio"),
        "orator-s2s reads WAV through orator-audio"
    );
    // One WS crate for both sides: the daemon speaks tokio-tungstenite
    // (same line as the workspace axum) and nothing else.
    let daemon =
        std::fs::read_to_string(root.join("orator-daemon/Cargo.toml"))
            .expect("daemon manifest readable");
    assert!(
        has_dep(&daemon, "tokio-tungstenite"),
        "orator-daemon serves WS over the single shared crate"
    );
    assert!(
        !has_dep(&daemon, "tungstenite"),
        "no second WS crate beside tokio-tungstenite"
    );
    assert!(
        !has_dep(&daemon, "axum"),
        "no second HTTP stack on the daemon"
    );
    // One TUI stack, composed through the ratatui re-export: the CLI
    // declares ratatui itself and never crossterm directly.
    let cli = std::fs::read_to_string(root.join("orator-cli/Cargo.toml"))
        .expect("cli manifest readable");
    assert!(
        has_dep(&cli, "ratatui"),
        "orator-cli composes the workspace TUI stack"
    );
    assert!(
        !has_dep(&cli, "crossterm"),
        "crossterm arrives only via the ratatui re-export"
    );
    assert!(
        has_dep(&cli, "orator-vad"),
        "the TUI mic gate reuses the calibrated detector"
    );
    assert!(
        has_dep(&cli, "tokio-tungstenite"),
        "the CLI WS client shares the single WS crate"
    );
}
