//! Writer parity: live and batch shapes serve the same file-level recall.

use super::{FragmentWriter, RowShape, WRITER_SCHEMA_VERSION};
use crate::query::db_storage::{file_record, fragment_record};
use crate::search_types::{
    CodeSymbolType, EntityFragment, EntityMetadata, FileInfo, FragmentContent, FragmentSpan,
};

fn live_file(id: &str) -> FileInfo {
    FileInfo {
        id: id.to_string(),
        absolute_path: format!("/tmp/{id}.txt"),
        relative_path: format!("{id}.txt"),
        root_path: "/tmp".to_string(),
        size_bytes: 11,
        last_modified_time: 0,
        content_hash: None,
        kind: None,
        format: "text".to_string(),
        index_status: None,
    }
}

#[test]
fn schema_version_is_two() {
    assert_eq!(WRITER_SCHEMA_VERSION, 2);
}

#[test]
fn offline_live_write_serves_file_recall() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("memory db");
    let file = live_file("live-a");
    let stats = FragmentWriter::write_live(&db, &file, "hello world", None, None).expect("live");
    assert_eq!(stats.fragments, 1);
    assert_eq!(stats.skipped_embedding, 1);
    assert_eq!(FragmentWriter::row_shape(&db, "live-a"), RowShape::WholeFile);
    assert!(FragmentWriter::needs_rebuild(&db, "live-a", "live-a"));
}

#[test]
fn batch_shape_serves_chunked_recall() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("memory db");
    let file = live_file("batch-a");
    let mk = |index: u32, name: &str| EntityFragment {
        id: format!("batch-a-{index}"),
        group: None,
        file_id: "batch-a".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Text { text: format!("fn {name}() {{}}") },
        metadata: Some(EntityMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some(name.to_string()),
            scope: None,
            node_type: Some("function".to_string()),
            signature: Some(format!("fn {name}()")),
            doc: None,
            modifiers: Vec::new(),
        }),
    };
    let fragments = [mk(0, "aaa"), mk(1, "bbb")];
    let records: Vec<search_vector::db::FragmentRecord> =
        fragments.iter().map(fragment_record).collect();
    db.replace_file(&file_record(&file), &records, &[]).expect("batch commit");
    assert_eq!(FragmentWriter::row_shape(&db, "batch-a-0"), RowShape::Chunked);
    assert!(!FragmentWriter::needs_rebuild(&db, "batch-a", "batch-a-0"));
}

#[test]
fn backfill_clears_rebuild_flag() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("memory db");
    let file = live_file("live-b");
    FragmentWriter::write_live(&db, &file, "fn solo() {}", None, None).expect("live");
    assert!(FragmentWriter::needs_rebuild(&db, "live-b", "live-b"));
    let fragment = EntityFragment {
        id: "live-b".to_string(),
        group: None,
        file_id: "live-b".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Text { text: "fn solo() {}".to_string() },
        metadata: Some(EntityMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some("solo".to_string()),
            scope: None,
            node_type: Some("function".to_string()),
            signature: Some("fn solo()".to_string()),
            doc: None,
            modifiers: Vec::new(),
        }),
    };
    FragmentWriter::backfill_symbols(&db, std::slice::from_ref(&fragment)).expect("backfill");
    assert!(!FragmentWriter::needs_rebuild(&db, "live-b", "live-b"));
    assert_eq!(FragmentWriter::row_shape(&db, "live-b"), RowShape::WholeFile);
}
