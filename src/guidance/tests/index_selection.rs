//! P2.4 scanner-utils port: discovery rules, type-aware size limits,
//! diagnostics. Mirrors `scanner-utils.test.mjs` discovery cases.

use super::*;
use crate::zg_types::FileKind;
use tempfile::TempDir;

fn tree() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    for d in ["src", "node_modules", ".zvec-grep", "assets"] {
        std::fs::create_dir_all(root.join(d)).expect("mkdir");
    }
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write");
    std::fs::write(root.join("src/big.rs"), vec![b'x'; 2 * 1024 * 1024]).expect("write");
    std::fs::write(root.join("src/notes.md"), "# hi\n").expect("write");
    std::fs::write(root.join("src/data.json"), "{}\n").expect("write");
    std::fs::write(root.join("src/photo.png"), vec![0x89u8, 0x50, 0x4e, 0x47]).expect("write");
    std::fs::write(root.join("src/app.zip"), vec![0x50u8, 0x4b]).expect("write");
    std::fs::write(root.join("src/Cargo.lock"), "lock").expect("write");
    std::fs::write(root.join("node_modules/dep.js"), "dep").expect("write");
    std::fs::write(root.join(".zvec-grep/manifest.json"), "{}").expect("write");
    std::fs::write(root.join("assets/logo.jpg"), vec![0xffu8, 0xd8]).expect("write");
    let mut blob = vec![0x00u8; 3000];
    blob.extend(vec![b'a'; 7000]);
    std::fs::write(root.join("src/blob.bin"), blob).expect("write");
    dir
}

fn selection(root: &std::path::Path) -> FileSelection {
    FileSelection {
        roots: vec![root.to_path_buf()],
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extra_skip_dirs: Vec::new(),
        honor_gitignore: false,
        max_bytes_override: None,
    }
}

#[test]
fn detect_file_type_covers_kinds() {
    let cases = [
        ("a.rs", Some((FileKind::Code, "rust"))),
        ("a.ts", Some((FileKind::Code, "typescript"))),
        ("a.md", Some((FileKind::Text, "markdown"))),
        ("a.json", Some((FileKind::Data, "json"))),
        ("a.png", Some((FileKind::Image, "png"))),
        ("a.zip", None),
        ("a.sqlite", None),
        ("Dockerfile", Some((FileKind::Code, "dockerfile"))),
        ("Makefile", Some((FileKind::Code, "makefile"))),
        // Unknown extensions fall back to text (zvec parity).
        ("a.xyz", Some((FileKind::Text, "xyz"))),
    ];
    for (name, expected) in cases {
        let got = detect_file_type(std::path::Path::new(name));
        assert_eq!(got, expected.map(|(k, f)| (k, f.to_string())), "{name}");
    }
}

#[test]
fn select_files_applies_discovery_rules() {
    let dir = tree();
    let mut diag = ScanDiagnostics::default();
    let files = select_files(&selection(dir.path()), &mut diag).expect("select");
    let names: Vec<String> = files
        .iter()
        .map(|f| {
            f.path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(names.contains(&"main.rs".to_string()), "{names:?}");
    assert!(names.contains(&"notes.md".to_string()), "{names:?}");
    assert!(names.contains(&"data.json".to_string()), "{names:?}");
    // Rasters are skipped by the default patterns but stay discoverable:
    // an explicit include restores them with their kind tag (G0.5).
    for banned in ["big.rs", "Cargo.lock", "dep.js", "app.zip", "blob.bin", "manifest.json", "photo.png", "logo.jpg"] {
        assert!(!names.contains(&banned.to_string()), "{names:?}");
    }
    assert!(diag.skipped_large >= 1, "{diag:?}");
    assert!(diag.skipped_binary >= 1, "{diag:?}");
    assert!(diag.scanned >= 3, "{diag:?}");

    let mut sel = selection(dir.path());
    sel.include_globs = vec!["src/*.png".to_string()];
    let mut diag = ScanDiagnostics::default();
    let files = select_files(&sel, &mut diag).expect("select");
    let photo = files.iter().find(|f| f.path.ends_with("photo.png")).expect("explicit png restored");
    assert_eq!(photo.kind, FileKind::Image);
    assert_eq!(diag.image_files, 1, "{diag:?}");
}

#[test]
fn select_files_honors_include_and_exclude_globs() {
    let dir = tree();
    let mut sel = selection(dir.path());
    sel.include_globs = vec!["src/*.rs".to_string()];
    let mut diag = ScanDiagnostics::default();
    let files = select_files(&sel, &mut diag).expect("select");
    assert!(files.iter().all(|f| f.path.extension().is_some_and(|e| e == "rs")), "{files:?}");

    let mut sel = selection(dir.path());
    sel.exclude_globs = vec!["src/*.md".to_string()];
    let mut diag = ScanDiagnostics::default();
    let files = select_files(&sel, &mut diag).expect("select");
    assert!(files.iter().all(|f| !f.path.ends_with("notes.md")), "{files:?}");
    assert!(diag.skipped_glob >= 1, "{diag:?}");
}

#[test]
fn validate_roots_rejects_missing_and_overlapping() {
    let dir = tree();
    let sel = FileSelection {
        roots: vec![dir.path().join("nope")],
        ..selection(dir.path())
    };
    assert!(validate_roots(&sel).is_err());

    let sel = FileSelection {
        roots: vec![dir.path().to_path_buf(), dir.path().join("src")],
        ..selection(dir.path())
    };
    assert!(validate_roots(&sel).is_err());

    assert!(validate_roots(&selection(dir.path())).is_ok());
}


#[test]
fn default_file_patterns_match_without_recompiling_per_file() {
    // Finding 3 (index wall-clock): `matches_default_file_pattern`
    // compiled 24 fresh regexes per file (~39 ms/file in debug). The
    // compiled set must be built once and reused — this pins the cache.
    for name in [
        "a.min.js",
        "b.bundle.css",
        "c.generated.ts",
        "go.sum",
        "Cargo.lock",
        "x.map",
        "y.designer.cs",
    ] {
        assert!(matches_default_file_pattern(name), "{name} must be ignored");
    }
    for name in ["main.rs", "notes.md", "diagram.svg", "app.ts"] {
        assert!(
            !matches_default_file_pattern(name),
            "{name} must stay visible"
        );
    }
    assert!(
        default_ignored_matchers_initialized(),
        "default matchers must be compiled once and cached"
    );
}

#[test]
fn explicit_globs_still_match_per_call() {
    // Must-NOT-fire control: user-supplied globs keep per-call
    // semantics — the cache covers the static default list only.
    assert!(crate::query::glob::path_pattern_matches("src/*.rs", "src/main.rs"));
    assert!(!crate::query::glob::path_pattern_matches("src/*.rs", "src/nested/main.rs"));
}
