//! Crate-typed test fixtures shared by guidance-core Tier-1 suites.
//!
//! **Why Tier 1, not `tests/common/mod.rs`:** the ≥8 `GuidanceDoc` fixture
//! builders live inside inline `#[cfg(test)] mod tests` in `src/`, which per
//! Rust's integration-test rule can only import crate-internal items — a
//! Tier-2 `tests/common` is a separate crate linked against the public API.
//! So the shared fixtures live here (`src/tests/common.rs`, Tier 1) and the
//! e2e-only SyncEngine setup lives in `tests/common/mod.rs` (Tier 2).

use fluent_types::{GuidanceDoc, Member, MemberType, Meta, Param};

/// A `GuidanceDoc` for a hypothetical `src/test.zig` module with two public
/// functions (`helloWorld`, `addNumbers`). The canonical shape — every suite
/// that needs a different member set or module name overrides via
/// struct-update on top of this.
pub fn make_test_doc() -> GuidanceDoc {
    GuidanceDoc {
        meta: Meta {
            module: "test".into(),
            source: "src/test.zig".into(),
            language: "zig".into(),
        },
        comment: Some("Test module for query engine.".into()),
        members: vec![
            Member {
                type_name: MemberType::FnDecl,
                name: "helloWorld".into(),
                signature: Some("fn helloWorld() void".into()),
                comment: Some("Prints hello world.".into()),
                is_pub: true,
                line: Some(1),
                ..Member::default()
            },
            Member {
                type_name: MemberType::FnDecl,
                name: "addNumbers".into(),
                signature: Some("fn addNumbers(a: i32, b: i32) i32".into()),
                comment: Some("Adds two integers.".into()),
                is_pub: true,
                line: Some(5),
                ..Member::default()
            },
        ],
        ..GuidanceDoc::default()
    }
}

/// A `Member` with a `match_hash` — used by the json-store round-trip
/// suites (formerly duplicated as `json_store.rs::make_test_member`).
pub fn make_test_member(name: &str, sig: &str, hash: &str, comment: Option<&str>) -> Member {
    Member {
        type_name: MemberType::FnDecl,
        name: name.into(),
        signature: Some(sig.into()),
        match_hash: Some(hash.into()),
        comment: comment.map(Into::into),
        ..Member::default()
    }
}

/// A `GuidanceDoc` for a `src/test.zig` module with a single `hello`
/// function carrying one `name` parameter — the shape the json-writer
/// round-trip suites assert on.
pub fn make_test_doc_hello() -> GuidanceDoc {
    GuidanceDoc {
        meta: Meta {
            module: "test_module".into(),
            source: "src/test.zig".into(),
            language: "zig".into(),
        },
        comment: Some("A test module.".into()),
        members: vec![Member {
            type_name: MemberType::FnDecl,
            name: "hello".into(),
            signature: Some("fn hello(name: []const u8) -> []const u8".into()),
            params: vec![Param {
                name: "name".into(),
                type_name: Some("[]const u8".into()),
                default: None,
            }],
            returns: Some("[]const u8".into()),
            is_pub: true,
            line: Some(3),
            ..Member::default()
        }],
        ..GuidanceDoc::default()
    }
}

/// A seeded-index `FileRecord` for a real file under `workspace`.
pub fn db_file_record(workspace: &std::path::Path, name: &str) -> search_vector::db::FileRecord {
    let absolute = workspace.join(name);
    let meta = std::fs::metadata(&absolute).expect("seeded file reads");
    search_vector::db::FileRecord {
        id: name.to_string(),
        absolute_path: absolute.to_string_lossy().into_owned(),
        relative_path: name.to_string(),
        root_path: workspace.to_string_lossy().into_owned(),
        size_bytes: meta.len(),
        last_modified_time: meta
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_millis() as i64,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

/// A text-kind fragment row spanning lines 1..=`end_line`.
pub fn db_text_fragment(
    file_id: &str,
    symbol: &str,
    text: &str,
    end_line: u32,
) -> search_vector::db::FragmentRecord {
    search_vector::db::FragmentRecord {
        id: format!("{file_id}:{symbol}"),
        group: None,
        file_id: file_id.to_string(),
        range_json: format!(
            "{{\"kind\":\"text\",\"start_line\":1,\"end_line\":{end_line},\"start_offset\":0,\"end_offset\":{}}}",
            text.len()
        ),
        content_kind: "text".to_string(),
        content_text: Some(text.to_string()),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some(symbol.to_string()),
        scope: None,
        signature: None,
        doc: None,
        modifiers: String::new(),
        heading: None,
        heading_level: None,
        embedding: None,
    }
}

/// A project-card row with an explicit doc anchor.
pub fn db_project_card(
    key: &str,
    kind: &str,
    title: &str,
    body: &str,
    source_path: &str,
    anchor: &str,
) -> search_vector::db::ProjectCard {
    search_vector::db::ProjectCard {
        key: key.to_string(),
        kind: kind.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        source_path: source_path.to_string(),
        anchor: anchor.to_string(),
        aliases_json: "[]".to_string(),
        code_paths_json: "[]".to_string(),
        symbols_json: "[]".to_string(),
        content_hash: common_core::hash::blake3_hex(body.as_bytes()),
        generated_at: 0,
    }
}
