//! Row-translator characterization (included via `#[path]` forwarder
//! from `src/query/db_storage.rs`, per repo convention).
//!
//! M14.1: pins the `FileInfo -> FileRecord` and
//! `EntityFragment -> FragmentRecord -> FragmentRow -> EntityFragment`
//! translations through a real in-memory `GuidanceDb`, including the lossy
//! edges (ephemeral `content_hash`/`index_status` dropped on read-back;
//! image bytes dropped, format reset to PNG). Any M14.2 move must preserve
//! these outputs exactly.

use super::*;
use crate::search_types::{
    CodeEntityModifier, CodeSymbolType, EntityFragment, EntityMetadata, FileInfo, FileKind,
    FragmentContent, FragmentSpan, ImageFormat, StorageFilter,
};
use search_vector::db::GuidanceDb;

fn file_fixture() -> FileInfo {
    FileInfo {
        id: "file-1".to_string(),
        absolute_path: "/repo/src/a.ts".to_string(),
        relative_path: "src/a.ts".to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 64,
        last_modified_time: 100,
        content_hash: Some("abc123".to_string()),
        kind: Some(FileKind::Code),
        format: "typescript".to_string(),
        index_status: None,
    }
}

fn code_fragment() -> EntityFragment {
    EntityFragment {
        id: "frag-1".to_string(),
        group: None,
        file_id: "file-1".to_string(),
        range: FragmentSpan::Text {
            start_line: 1,
            end_line: 3,
            start_offset: 0,
            end_offset: 42,
        },
        content: FragmentContent::Text {
            text: "export function alpha() {}".to_string(),
        },
        metadata: Some(EntityMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some("alpha".to_string()),
            scope: Some("mod".to_string()),
            node_type: None,
            signature: Some("export function alpha() {}".to_string()),
            doc: Some("does alpha".to_string()),
            modifiers: vec![CodeEntityModifier::Exported, CodeEntityModifier::Async],
        }),
    }
}

/// Write one file + fragment through the real DB seam and read the
/// fragment back through `entity_fragment`, the same path recall uses.
fn write_read(file: &FileInfo, fragment: &EntityFragment) -> EntityFragment {
    let db = GuidanceDb::open_in_memory().expect("mem db");
    let committed = db
        .upsert_fragments(&file_record(file), &[fragment_record(fragment)], &[])
        .expect("upsert");
    assert_eq!(committed, 1);
    let rows = db.fragments_for_entity(&fragment.id).expect("read back");
    assert_eq!(rows.len(), 1, "one row per fragment id");
    entity_fragment(&rows[0])
}

#[test]
fn file_record_preserves_identity_fields() {
    let record = file_record(&file_fixture());
    assert_eq!(record.id, "file-1");
    assert_eq!(record.absolute_path, "/repo/src/a.ts");
    assert_eq!(record.relative_path, "src/a.ts");
    assert_eq!(record.root_path, "/repo");
    assert_eq!(record.size_bytes, 64);
    assert_eq!(record.last_modified_time, 100);
    assert_eq!(record.kind.as_deref(), Some("code"));
    assert_eq!(record.format, "typescript");
    assert_eq!(record.content_hash.as_deref(), Some("abc123"));
    // Ingestion marks the row indexed with a clean failure slate.
    assert_eq!(record.index_status.as_deref(), Some("indexed"));
    assert_eq!(record.fail_count, 0);
    assert_eq!(record.last_error, None);
}

#[test]
fn file_record_file_info_round_trip_drops_ephemeral() {
    // Read-back drops what the fragment tables do not surface: the
    // caller-side content hash and the caller-side index status.
    let back = file_info(&file_record(&file_fixture()));
    let mut expected = file_fixture();
    expected.content_hash = None;
    expected.index_status = None;
    assert_eq!(back, expected);
}

#[test]
fn file_record_without_kind_stays_kindless() {
    let mut file = file_fixture();
    file.kind = None;
    let record = file_record(&file);
    assert_eq!(record.kind, None);
    assert_eq!(file_info(&record).kind, None);
}

#[test]
fn fragment_record_write_row_pins_code_fields() {
    let record = fragment_record(&code_fragment());
    assert_eq!(record.id, "frag-1");
    assert_eq!(record.file_id, "file-1");
    assert_eq!(record.content_kind, "text");
    assert_eq!(
        record.content_text.as_deref(),
        Some("export function alpha() {}")
    );
    assert_eq!(record.symbol_type.as_deref(), Some("function"));
    assert_eq!(record.symbol_name.as_deref(), Some("alpha"));
    assert_eq!(record.scope.as_deref(), Some("mod"));
    assert_eq!(
        record.signature.as_deref(),
        Some("export function alpha() {}")
    );
    assert_eq!(record.doc.as_deref(), Some("does alpha"));
    assert_eq!(record.modifiers, "exported async");
    assert_eq!(record.heading, None);
    assert_eq!(record.heading_level, None);
    assert_eq!(record.embedding, None);
}

#[test]
fn code_fragment_db_round_trip() {
    assert_eq!(write_read(&file_fixture(), &code_fragment()), code_fragment());
}

#[test]
fn markdown_fragment_db_round_trip() {
    let fragment = EntityFragment {
        id: "frag-md".to_string(),
        group: Some("group".to_string()),
        file_id: "file-1".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Text {
            text: "# Title".to_string(),
        },
        metadata: Some(EntityMetadata::Markdown {
            heading: Some("Title".to_string()),
            level: Some(1),
            scope: None,
        }),
    };
    let record = fragment_record(&fragment);
    assert_eq!(record.symbol_type, None);
    assert_eq!(record.modifiers, String::new());
    assert_eq!(record.heading.as_deref(), Some("Title"));
    assert_eq!(record.heading_level, Some(1));
    assert_eq!(write_read(&file_fixture(), &fragment), fragment);
}

#[test]
fn bare_fragment_db_round_trip() {
    let fragment = EntityFragment {
        id: "frag-bare".to_string(),
        group: None,
        file_id: "file-1".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Text {
            text: "plain".to_string(),
        },
        metadata: None,
    };
    assert_eq!(write_read(&file_fixture(), &fragment), fragment);
}

#[test]
fn image_fragment_db_round_trip_is_lossy_as_observed() {
    // Binary bytes are not persisted: read-back yields empty PNG bytes.
    // Pinned as-is (M14.2 must not silently "fix" the loss).
    let fragment = EntityFragment {
        id: "frag-img".to_string(),
        group: None,
        file_id: "file-1".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Image {
            data: vec![1, 2, 3],
            format: ImageFormat::Jpeg,
        },
        metadata: None,
    };
    let record = fragment_record(&fragment);
    assert_eq!(record.content_kind, "image");
    assert_eq!(record.content_text, None);
    assert_eq!(
        write_read(&file_fixture(), &fragment).content,
        FragmentContent::Image {
            data: Vec::new(),
            format: ImageFormat::Png,
        }
    );
}

#[test]
fn symbol_type_vocab_round_trips_closed() {
    for symbol_type in CodeSymbolType::all() {
        let mut fragment = code_fragment();
        fragment.id = format!("frag-{symbol_type:?}");
        fragment.metadata = Some(EntityMetadata::Code {
            symbol_type: *symbol_type,
            symbol_name: None,
            scope: None,
            node_type: None,
            signature: None,
            doc: None,
            modifiers: Vec::new(),
        });
        assert_eq!(
            write_read(&file_fixture(), &fragment),
            fragment,
            "symbol type {symbol_type:?}"
        );
    }
}

#[test]
fn modifier_vocab_round_trips_closed() {
    for modifier in CodeEntityModifier::all() {
        let mut fragment = code_fragment();
        fragment.id = format!("frag-{modifier:?}");
        fragment.metadata = Some(EntityMetadata::Code {
            symbol_type: CodeSymbolType::Value,
            symbol_name: None,
            scope: None,
            node_type: None,
            signature: None,
            doc: None,
            modifiers: vec![*modifier],
        });
        assert_eq!(
            write_read(&file_fixture(), &fragment),
            fragment,
            "modifier {modifier:?}"
        );
    }
}

#[test]
fn fragment_lemma_pins_fields() {
    let lemma = fragment_lemma("frag-1", "alpha", 1.0);
    assert_eq!(lemma.fragment_id, "frag-1");
    assert_eq!(lemma.lemma, "alpha");
    assert_eq!(lemma.confidence, 1.0);
}

#[test]
fn fragment_filter_passes_storage_filter_through() {
    let filter = StorageFilter {
        file_ids: Some(vec!["file-1".to_string()]),
        group_ids: vec!["group".to_string()],
        symbol_names: vec!["alpha".to_string()],
        symbol_types: vec![CodeSymbolType::Function],
        modified_after: Some(10),
        modified_before: Some(20),
        ..StorageFilter::default()
    };
    let pushed = fragment_filter(&filter);
    assert_eq!(pushed.file_ids, Some(vec!["file-1".to_string()]));
    assert_eq!(pushed.group_ids, vec!["group".to_string()]);
    assert_eq!(pushed.symbol_names, vec!["alpha".to_string()]);
    assert_eq!(pushed.symbol_types, vec!["function".to_string()]);
    assert_eq!(pushed.modified_after, Some(10));
    assert_eq!(pushed.modified_before, Some(20));
}
