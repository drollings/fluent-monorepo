//! Source identity path invariants.

use crate::search_types::{FileInfo, FileKind};
use crate::source_ref::{Citation, SourceRef};

#[test]
fn workspace_relative_display_survives_root_depth_and_separators() {
    let workspace = std::path::Path::new("/repo");
    for (input, expected) in [
        (
            "src/guidance/src/sync_engine.rs",
            "src/guidance/src/sync_engine.rs",
        ),
        (
            "./src/guidance/src/sync_engine.rs",
            "src/guidance/src/sync_engine.rs",
        ),
        (
            "src/guidance/../guidance/src/sync_engine.rs",
            "src/guidance/src/sync_engine.rs",
        ),
    ] {
        let source = SourceRef::from_workspace(workspace, std::path::Path::new(input));
        assert_eq!(source.display(), expected, "{input}");
        assert_eq!(
            source.live_path(),
            std::path::PathBuf::from("/repo").join(expected)
        );
    }
}

#[test]
fn file_record_paths_are_workspace_first() {
    let file = FileInfo {
        id: "f".to_string(),
        absolute_path: "/repo/src/guidance/src/main.rs".to_string(),
        relative_path: "src/guidance/src/main.rs".to_string(),
        root_path: "/repo/src".to_string(),
        size_bytes: 1,
        last_modified_time: 1,
        content_hash: None,
        kind: Some(FileKind::Code),
        format: "rust".to_string(),
        index_status: None,
    };
    let source = SourceRef::from_file(&file);
    assert_eq!(source.display(), "src/guidance/src/main.rs");
    assert_eq!(
        source.live_path(),
        std::path::PathBuf::from(&file.absolute_path)
    );
    assert_eq!(source.root_path, "/repo/src");
}

#[test]
fn citation_text_is_stable_and_status_is_separate() {
    let source = SourceRef::from_workspace(
        std::path::Path::new("/repo"),
        std::path::Path::new("src/a.rs"),
    );
    assert_eq!(source.citation(Some(3), Some(7)).text(), "src/a.rs:3-7");
    assert_eq!(source.citation(Some(3), Some(3)).text(), "src/a.rs:3");
    let stale: Citation = source.citation(Some(3), Some(7)).with_status(true, false);
    assert_eq!(stale.to_string(), "src/a.rs:3-7 (stale)");
    let unverified = source.citation(Some(3), None).with_status(false, true);
    assert_eq!(unverified.to_string(), "src/a.rs:3 (unverified)");
}
