//! `file_kind` unit pins: canonical extension table, kind tags, and the
//! documented divergences (D1/D2/D3 — see the module docs).

use super::*;
use crate::file_kind::{
    code_format_for_extension, is_binary_extension, kind_format_for_extension, named_file_kind,
    CODE_EXTENSIONS, SOURCE_EXTENSIONS,
};

#[test]
fn kind_tags_round_trip_per_direction() {
    assert_eq!(FileKind::Text.as_str(), "text");
    assert_eq!(FileKind::Code.as_str(), "code");
    assert_eq!(FileKind::Data.as_str(), "data");
    assert_eq!(FileKind::Image.as_str(), "image");
    assert_eq!(FileKind::parse_scan(Some("code")), FileKind::Code);
    assert_eq!(FileKind::parse_scan(Some("data")), FileKind::Data);
    assert_eq!(FileKind::parse_scan(Some("text")), FileKind::Text);
    assert_eq!(FileKind::parse_scan(Some("image")), FileKind::Text);
    assert_eq!(FileKind::parse_scan(Some("bogus")), FileKind::Text);
    assert_eq!(FileKind::parse_scan(None), FileKind::Text);
    assert_eq!(FileKind::parse_stored(Some("text")), Some(FileKind::Text));
    assert_eq!(FileKind::parse_stored(Some("code")), Some(FileKind::Code));
    assert_eq!(FileKind::parse_stored(Some("data")), Some(FileKind::Data));
    assert_eq!(FileKind::parse_stored(Some("image")), Some(FileKind::Image));
    assert_eq!(FileKind::parse_stored(Some("bogus")), None);
    assert_eq!(FileKind::parse_stored(None), None);
}

#[test]
fn kind_serde_wire_names() {
    for (kind, wire) in [
        (FileKind::Text, "\"text\""),
        (FileKind::Code, "\"code\""),
        (FileKind::Data, "\"data\""),
        (FileKind::Image, "\"image\""),
    ] {
        let json = serde_json::to_string(&kind).expect("serialize");
        assert_eq!(json, wire);
        let back: FileKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, kind);
    }
}

#[test]
fn adapter_map_covers_tree_sitter_formats() {
    let cases = [
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
        ("zig", None),
        ("md", None),
        ("json", None),
        ("", None),
    ];
    for (ext, expected) in cases {
        assert_eq!(code_format_for_extension(ext), expected, "{ext}");
    }
}

#[test]
fn ingest_map_covers_kind_and_format() {
    let cases = [
        ("rs", Some((FileKind::Code, "rust"))),
        ("h", Some((FileKind::Code, "cpp"))),
        ("mts", None),
        ("json", Some((FileKind::Data, "json"))),
        ("yml", Some((FileKind::Data, "yaml"))),
        ("md", Some((FileKind::Text, "markdown"))),
        ("txt", Some((FileKind::Text, "text"))),
        ("png", Some((FileKind::Image, "png"))),
        ("jpg", Some((FileKind::Image, "jpeg"))),
        ("xyz", None),
        ("", None),
    ];
    for (ext, expected) in cases {
        assert_eq!(kind_format_for_extension(ext), expected, "{ext}");
    }
    assert_eq!(
        named_file_kind("Dockerfile"),
        Some((FileKind::Code, "dockerfile"))
    );
    assert_eq!(
        named_file_kind("Makefile"),
        Some((FileKind::Code, "makefile"))
    );
    assert_eq!(named_file_kind("README.md"), None);
    assert!(is_binary_extension("zip"));
    assert!(is_binary_extension("sqlite"));
    assert!(is_binary_extension("mp3"));
    assert!(!is_binary_extension("rs"));
    assert!(!is_binary_extension(""));
}

#[test]
fn divergences_stay_explicit() {
    // D1: headers parse as C but ingest as C++.
    assert_eq!(code_format_for_extension("h"), Some("c"));
    assert_eq!(
        kind_format_for_extension("h"),
        Some((FileKind::Code, "cpp"))
    );
    // D2: module TS parses as typescript but ingests as text.
    assert_eq!(code_format_for_extension("mts"), Some("typescript"));
    assert_eq!(kind_format_for_extension("mts"), None);
    // D3: scan tags fall back to Text, stored tags keep Image.
    assert_eq!(FileKind::parse_scan(Some("image")), FileKind::Text);
    assert_eq!(FileKind::parse_stored(Some("image")), Some(FileKind::Image));
}

#[test]
fn extension_lists_match_walker_scope() {
    assert_eq!(SOURCE_EXTENSIONS, &["zig", "zon", "py", "rs", "md"]);
    for ext in ["rs", "py", "go", "java", "c", "h"] {
        assert!(
            CODE_EXTENSIONS.contains(&ext),
            "{ext} is code-classified and must be listed"
        );
    }
    for ext in CODE_EXTENSIONS {
        assert!(
            kind_format_for_extension(ext).is_some_and(|(kind, _)| kind == FileKind::Code),
            "{ext} listed as code must classify Code"
        );
    }
}

/// Bridge table (`FileKind::coarse_type` / `FileType::from_kind`): one shared,
/// total, lossy mapping — both spellings agree on every input, pinned here so
/// the two taxonomies cannot drift silently.
#[test]
fn kind_to_type_bridge_is_total_and_shared() {
    use crate::FileType;
    for kind in [
        FileKind::Text,
        FileKind::Code,
        FileKind::Data,
        FileKind::Image,
    ] {
        assert_eq!(
            FileType::from_kind(kind),
            kind.coarse_type(),
            "{kind:?} must map identically through both spellings"
        );
    }
    assert_eq!(FileKind::Code.coarse_type(), FileType::Source);
    assert_eq!(FileKind::Text.coarse_type(), FileType::Markdown);
    assert_eq!(FileKind::Data.coarse_type(), FileType::Data);
    // No image variant exists on the structure-rendering side.
    assert_eq!(FileKind::Image.coarse_type(), FileType::Unknown);
}

/// Explicit drift ledger: extensions where the ingest table (`kind_format`)
/// and the structure table (`FileType::from_extension`) disagree through the
/// bridge. Any new extension added to either map must update this ledger
/// deliberately — silent divergence is the defect this pins against.
#[test]
fn extension_tables_diverge_only_where_ledgered() {
    use crate::FileType;
    fn dotted(ext: &str) -> String {
        format!(".{ext}")
    }
    // Ingest says Code but the structure table does not say Source.
    let mut code_divergent = Vec::new();
    for ext in CODE_EXTENSIONS {
        let (kind, _) = kind_format_for_extension(ext).expect("listed code classifies");
        assert_eq!(kind, FileKind::Code);
        if FileType::from_extension(&dotted(ext)) != FileType::Source {
            code_divergent.push(*ext);
        }
    }
    assert_eq!(
        code_divergent,
        [
            "cc", "cxx", "mjs", "cjs", "php", "kts", "cs", "scala", "sh", "bash", "zsh", "sql",
            "css", "scss", "less", "vue", "svelte",
        ],
        "code-but-not-Source set changed: update the ledger deliberately"
    );
    // Structure table classifies but ingest is silent (no kind entry).
    let mut ingest_silent = Vec::new();
    for ext in [
        "zig", "zon", "lua", "markdown", "ini", "env", "cfg", "conf", "tsv", "sqlite", "db", "sql",
        "parquet", "pdf", "mp3", "wav", "flac", "ogg", "m4a", "wma",
    ] {
        if kind_format_for_extension(ext).is_none() && !is_binary_extension(ext) {
            ingest_silent.push(ext);
        }
    }
    // Note: `db`/`sqlite` are binary-excluded (not silent); `mp3` is
    // binary-excluded but `wav`/`flac`/`ogg`/`m4a`/`wma` are ingest-silent
    // and fall back to `Text` at the selection layer.
    assert_eq!(
        ingest_silent,
        [
            "zig", "zon", "lua", "markdown", "ini", "env", "cfg", "conf", "tsv", "parquet", "wav",
            "flac", "ogg", "m4a", "wma",
        ],
        "structure-but-ingest-silent set changed: update the ledger deliberately"
    );
}
