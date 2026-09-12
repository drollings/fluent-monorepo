//! P2.4 scanner-utils port: discovery rules, type-aware size limits,
//! diagnostics. Mirrors `scanner-utils.test.mjs` discovery cases.

use super::*;
use crate::search_types::FileKind;
use tempfile::TempDir;

fn tree() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    for d in ["src", "node_modules", ".guidance", "assets"] {
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
    std::fs::write(root.join(".guidance/manifest.json"), "{}").expect("write");
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

// M7.1 characterization: the extension matrix across all five tables,
// pinned verbatim before the `fluent_types::file_kind` extraction. This
// is the shared fixture M7.3 migrates against — including the three live
// divergences (documented, never silently unified):
//   D1 `h`: adapter format `c`, selection format `cpp` (both load-bearing).
//   D2 `mts`/`cts`: adapter knows `typescript`, selection falls back to text.
//   D3 `"image"` tag: ingest parse falls back to `Text`, DB parse keeps `Image`.

#[test]
fn m7_extension_matrix_across_tables() {
    use crate::extractor::adapter::format_for_extension;

    // Table 1 — adapter formats (tree-sitter authority).
    let adapter: &[(&str, Option<&str>)] = &[
        ("rs", Some("rust")),
        ("py", Some("python")),
        ("ts", Some("typescript")),
        ("mts", Some("typescript")),
        ("cts", Some("typescript")),
        ("tsx", Some("tsx")),
        ("js", Some("javascript")),
        ("mjs", Some("javascript")),
        ("cjs", Some("javascript")),
        ("jsx", Some("jsx")),
        ("go", Some("go")),
        ("java", Some("java")),
        ("c", Some("c")),
        ("h", Some("c")),
        ("cc", Some("cpp")),
        ("cpp", Some("cpp")),
        ("cxx", Some("cpp")),
        ("hpp", Some("cpp")),
        ("RS", Some("rust")),
        ("zig", None),
        ("md", None),
        ("json", None),
        ("", None),
    ];
    for (ext, expected) in adapter {
        assert_eq!(format_for_extension(ext), *expected, "adapter:{ext}");
    }

    // Table 2 — selection detect (ingest authority: kind + format).
    let selection: &[(&str, Option<(FileKind, &str)>)] = &[
        ("a.rs", Some((FileKind::Code, "rust"))),
        ("a.py", Some((FileKind::Code, "python"))),
        ("a.ts", Some((FileKind::Code, "typescript"))),
        ("a.mts", Some((FileKind::Text, "mts"))),
        ("a.cts", Some((FileKind::Text, "cts"))),
        ("a.tsx", Some((FileKind::Code, "tsx"))),
        ("a.js", Some((FileKind::Code, "javascript"))),
        ("a.jsx", Some((FileKind::Code, "jsx"))),
        ("a.go", Some((FileKind::Code, "go"))),
        ("a.java", Some((FileKind::Code, "java"))),
        ("a.c", Some((FileKind::Code, "c"))),
        ("a.h", Some((FileKind::Code, "cpp"))),
        ("a.cc", Some((FileKind::Code, "cpp"))),
        ("a.rb", Some((FileKind::Code, "ruby"))),
        ("a.php", Some((FileKind::Code, "php"))),
        ("a.zig", Some((FileKind::Text, "zig"))),
        ("a.zon", Some((FileKind::Text, "zon"))),
        ("a.md", Some((FileKind::Text, "markdown"))),
        ("a.mdx", Some((FileKind::Text, "markdown"))),
        ("a.txt", Some((FileKind::Text, "text"))),
        ("a.json", Some((FileKind::Data, "json"))),
        ("a.jsonc", Some((FileKind::Data, "json"))),
        ("a.yaml", Some((FileKind::Data, "yaml"))),
        ("a.yml", Some((FileKind::Data, "yaml"))),
        ("a.toml", Some((FileKind::Data, "toml"))),
        ("a.csv", Some((FileKind::Data, "csv"))),
        ("a.png", Some((FileKind::Image, "png"))),
        ("a.jpg", Some((FileKind::Image, "jpeg"))),
        ("a.gif", Some((FileKind::Image, "gif"))),
        ("a.webp", Some((FileKind::Image, "webp"))),
        ("a.zip", None),
        ("a.exe", None),
        ("a.pdf", None),
        ("a.mp3", None),
        ("Dockerfile", Some((FileKind::Code, "dockerfile"))),
        ("Makefile", Some((FileKind::Code, "makefile"))),
        ("a.xyz", Some((FileKind::Text, "xyz"))),
    ];
    for (name, expected) in selection {
        let got = detect_file_type(std::path::Path::new(name));
        assert_eq!(
            got,
            expected.map(|(k, f)| (k, f.to_string())),
            "selection:{name}"
        );
    }
    // Case-insensitivity rides the extension lowercasing.
    assert_eq!(
        detect_file_type(std::path::Path::new("A.RS")),
        Some((FileKind::Code, "rust".to_string()))
    );

    // Table 3 — ingest kind tags (round-trip through the scan boundary).
    use crate::index_pipeline::{kind_name, parse_scan_kind};
    assert_eq!(kind_name(FileKind::Text), "text");
    assert_eq!(kind_name(FileKind::Code), "code");
    assert_eq!(kind_name(FileKind::Data), "data");
    assert_eq!(kind_name(FileKind::Image), "image");
    assert_eq!(parse_scan_kind(Some("code")), FileKind::Code);
    assert_eq!(parse_scan_kind(Some("data")), FileKind::Data);
    assert_eq!(parse_scan_kind(Some("text")), FileKind::Text);
    assert_eq!(parse_scan_kind(Some("image")), FileKind::Text);
    assert_eq!(parse_scan_kind(Some("bogus")), FileKind::Text);
    assert_eq!(parse_scan_kind(None), FileKind::Text);

    // Table 4 — DB kind tags (persisted-record boundary).
    use crate::query::db_storage::{file_info, file_record};
    use crate::search_types::FileInfo;
    for (kind, tag) in [
        (FileKind::Text, "text"),
        (FileKind::Code, "code"),
        (FileKind::Data, "data"),
        (FileKind::Image, "image"),
    ] {
        let info = FileInfo {
            kind: Some(kind),
            ..Default::default()
        };
        assert_eq!(file_record(&info).kind.as_deref(), Some(tag), "{tag}");
    }
    assert_eq!(file_record(&FileInfo::default()).kind, None);
    for (tag, expected) in [
        ("text", Some(FileKind::Text)),
        ("code", Some(FileKind::Code)),
        ("data", Some(FileKind::Data)),
        ("image", Some(FileKind::Image)),
        ("bogus", None),
    ] {
        let mut record = file_record(&FileInfo::default());
        record.kind = Some(tag.to_string());
        assert_eq!(file_info(&record).kind, expected, "db:{tag}");
    }
    // D3: the two parse directions disagree on `"image"` — pinned.
    assert_eq!(parse_scan_kind(Some("image")), FileKind::Text);
    let mut record = file_record(&FileInfo::default());
    record.kind = Some("image".to_string());
    assert_eq!(file_info(&record).kind, Some(FileKind::Image));

    // Table 5 — walk scope (discovery authority).
    assert_eq!(
        common_core::walk::SOURCE_EXTENSIONS,
        &["zig", "zon", "py", "rs", "md"]
    );
    // The walker's zero-domain default cannot depend on fluent-types, so
    // the two lists are pinned together here instead of unified.
    let mut walker = common_core::walk::SOURCE_EXTENSIONS.to_vec();
    let mut canonical = fluent_types::file_kind::SOURCE_EXTENSIONS.to_vec();
    walker.sort();
    canonical.sort();
    assert_eq!(walker, canonical);

    // D1/D2 restated as cross-table assertions so no migration can blur them.
    assert_eq!(format_for_extension("h"), Some("c"));
    assert_eq!(
        detect_file_type(std::path::Path::new("a.h")),
        Some((FileKind::Code, "cpp".to_string()))
    );
    assert_eq!(format_for_extension("mts"), Some("typescript"));
    assert_eq!(
        detect_file_type(std::path::Path::new("a.mts")),
        Some((FileKind::Text, "mts".to_string()))
    );
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

#[test]
fn select_files_skips_hidden_files_by_default() {
    // Apples-to-apples: rg/zg skip hidden files unless asked. guidance
    // ingested its own `.guidance.db-wal` as a fragment (161 vs 160
    // files on the scale corpus) because only hidden DIRS were skipped.
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("visible.rs"), "fn visible() {}\n").expect("write");
    std::fs::write(dir.path().join(".hidden.rs"), "fn hidden() {}\n").expect("write");
    let sel = FileSelection {
        roots: vec![dir.path().to_path_buf()],
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extra_skip_dirs: Vec::new(),
        honor_gitignore: false,
        max_bytes_override: None,
    };
    let mut diag = ScanDiagnostics::default();
    let files = select_files(&sel, &mut diag).expect("select");
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
    assert!(names.contains(&"visible.rs".to_string()), "{names:?}");
    assert!(!names.contains(&".hidden.rs".to_string()), "{names:?}");
}

#[test]
fn select_files_explicit_glob_restores_hidden_files() {
    // Must-NOT-fire control: explicit includes bypass the hidden skip,
    // mirroring the raster precedent (skipped by default, discoverable).
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join(".hidden.rs"), "fn hidden() {}\n").expect("write");
    let sel = FileSelection {
        roots: vec![dir.path().to_path_buf()],
        include_globs: vec!["*.rs".to_string()],
        exclude_globs: Vec::new(),
        extra_skip_dirs: Vec::new(),
        honor_gitignore: false,
        max_bytes_override: None,
    };
    let mut diag = ScanDiagnostics::default();
    let files = select_files(&sel, &mut diag).expect("select");
    assert!(
        files.iter().any(|f| f.path.ends_with(".hidden.rs")),
        "explicit include restores hidden files"
    );
}
