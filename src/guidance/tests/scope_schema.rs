//! Scope schema: workspace globs match index-relative rows, fixtures
//! stay out of default pools, and crate attribution reuses the gate.
use crate::component_map::component_gate_for_path;
use crate::query::glob::{path_pattern_matches, ripgrep_glob_matches};

#[test]
fn workspace_glob_matches_index_relative_rows() {
    let workspace = "src/guidance/src/sync_engine.rs";
    let index_relative = "guidance/src/sync_engine.rs";
    for pattern in ["src/guidance/**", "src/guidance/src/**"] {
        assert!(
            path_pattern_matches(pattern, workspace) || ripgrep_glob_matches(pattern, workspace),
            "workspace {workspace} under {pattern}"
        );
        assert!(
            path_pattern_matches(pattern, index_relative)
                || ripgrep_glob_matches(pattern, index_relative),
            "index row {index_relative} under {pattern}"
        );
    }
    assert!(
        path_pattern_matches("src/router/**", "src/router/dispatch.rs"),
        "router scope matches"
    );
    assert!(
        !path_pattern_matches("src/router/**", "src/guidance/src/sync_engine.rs"),
        "cross-crate does not match"
    );
}

#[test]
fn basename_fallback_matches_bare_names() {
    assert!(
        path_pattern_matches("sync_engine.rs", "src/guidance/src/sync_engine.rs"),
        "bare name falls back to basename"
    );
}

#[test]
fn fixture_paths_stay_out_of_default_pools() {
    use crate::search_types::{FileInfo, FileKind};
    let fixture = FileInfo {
        id: "f".to_string(),
        absolute_path: "/repo/src/guidance/fixture_read/src/hybrid.rs".to_string(),
        relative_path: "src/guidance/fixture_read/src/hybrid.rs".to_string(),
        root_path: "/repo/src".to_string(),
        size_bytes: 10,
        last_modified_time: 1,
        content_hash: None,
        kind: Some(FileKind::Code),
        format: "rust".to_string(),
        index_status: None,
    };
    assert_eq!(
        crate::source_scope::of_file(&fixture),
        crate::source_scope::SourceScope::Fixture
    );
    assert!(!crate::source_scope::of_file(&fixture).is_product());
    let product = FileInfo {
        relative_path: "src/guidance/src/sync_engine.rs".to_string(),
        ..fixture.clone()
    };
    assert!(crate::source_scope::of_file(&product).is_product());
}

#[test]
fn crate_attribution_reuses_component_gate() {
    for path in [
        "src/guidance/src/sync_engine.rs",
        "src/router/dispatch.rs",
        "src/common-core/src/hash.rs",
    ] {
        let gate = component_gate_for_path(path);
        let finding = crate::read::engine::Finding {
            id: "x".to_string(),
            producer: crate::read::engine::FindingProducer::Database,
            kind: crate::read::engine::FindingKind::Code,
            source: crate::source_ref::SourceRef::from_workspace(
                std::path::Path::new("/workspace"),
                std::path::Path::new(path),
            ),
            start_line: Some(1),
            end_line: Some(2),
            text: "excerpt".to_string(),
            producer_score: 1.0,
            provenance: "code".to_string(),
        };
        assert_eq!(finding.crate_name(), gate.package, "{path}");
    }
}
