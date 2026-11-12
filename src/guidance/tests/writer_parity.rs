//! Writer parity: live and batch shapes serve the same file-level recall.

use super::{FragmentWriter, RowShape, VectorPolicy, WRITER_SCHEMA_VERSION};
use crate::extractor::{FragmentMetadata, FragmentRange, PreparedFragment};
use crate::query::db_storage::{file_record, fragment_record};
use crate::search_types::{
    CodeSymbolType, EntityFragment, EntityMetadata, FileInfo, FileKind, FragmentContent,
    FragmentSpan,
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
fn schema_version_is_three() {
    assert_eq!(WRITER_SCHEMA_VERSION, 3);
}

fn item_file(id: &str) -> search_vector::db::FileRecord {
    search_vector::db::FileRecord {
        id: id.to_string(),
        absolute_path: format!("/repo/{id}.rs"),
        relative_path: format!("{id}.rs"),
        root_path: "/repo".to_string(),
        size_bytes: 64,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

fn code_fragment(
    id: &str,
    name: &str,
    scope: Option<&str>,
    doc: &str,
    text: &str,
    start_line: usize,
) -> PreparedFragment {
    PreparedFragment {
        id: id.to_string(),
        content_text: text.to_string(),
        embedding_text: None,
        range: FragmentRange {
            start_line,
            end_line: start_line + 2,
            start_offset: 0,
            end_offset: text.len(),
        },
        group: None,
        metadata: Some(FragmentMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some(name.to_string()),
            scope: scope.map(str::to_string),
            node_type: "function_item".to_string(),
            signature: Some(format!("fn {name}()")),
            doc: Some(doc.to_string()),
            modifiers: Vec::new(),
        }),
        calls: Vec::new(),
    }
}

fn scope_block(
    node_key: &str,
    name: &str,
    scope: &str,
    text: &str,
) -> crate::ast_parser::CommentBlock {
    crate::ast_parser::CommentBlock {
        node_key: node_key.to_string(),
        node_kind: "function_item".to_string(),
        name: name.to_string(),
        scope: scope.to_string(),
        start_byte: 0,
        end_byte: text.len(),
        start_line: 1,
        end_line: 1,
        text: text.to_string(),
        comment_hash: common_core::hash::blake3_hex(text.as_bytes()),
    }
}

#[test]
fn write_items_required_rejects_count_mismatch() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("memory db");
    let fragments = [
        code_fragment("fa", "alpha", None, "Adds one", "fn alpha() {}", 1),
        code_fragment("fb", "beta", None, "Removes two", "fn beta() {}", 4),
    ];
    let one_vector = vec![vec![0.5, -0.5]];
    let error = FragmentWriter::write_items(
        &db,
        &item_file("f"),
        FileKind::Code,
        &fragments,
        &[],
        &one_vector,
        None,
        VectorPolicy::Required,
    )
    .expect_err("one vector for two fragments must fail the file");
    assert!(error.contains("mismatch"), "{error}");
    let stats = FragmentWriter::write_items(
        &db,
        &item_file("f"),
        FileKind::Code,
        &fragments[..1],
        &[],
        &one_vector,
        None,
        VectorPolicy::Required,
    )
    .expect("matching counts commit");
    assert_eq!(stats, 1);
    assert!(
        db.fragment_freshness("f")
            .expect("fingerprint")
            .expect("row")
            .embedded,
        "required vectors must mark rows embedded"
    );
}

#[test]
fn write_items_optional_commits_without_vectors() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("memory db");
    let fragments = [code_fragment(
        "fa",
        "alpha",
        None,
        "Adds one",
        "fn alpha() {}",
        1,
    )];
    let count = FragmentWriter::write_items(
        &db,
        &item_file("f"),
        FileKind::Code,
        &fragments,
        &[],
        &[],
        None,
        VectorPolicy::Optional,
    )
    .expect("no-vector live commit");
    assert_eq!(count, 1);
    assert_eq!(FragmentWriter::row_shape(&db, "fa"), RowShape::Chunked);
    assert!(
        !db.fragment_freshness("f")
            .expect("fingerprint")
            .expect("row")
            .embedded,
        "vector-less rows must read back as unembedded"
    );
}

#[test]
fn write_items_attaches_block_lemmas_to_owning_fragment() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("memory db");
    let fragments = [
        code_fragment("fa", "alpha", None, "Adds one", "fn alpha() {}", 1),
        code_fragment("fb", "beta", None, "Removes two", "fn beta() {}", 4),
        PreparedFragment {
            id: "f#$file".to_string(),
            content_text: "Top of file".to_string(),
            embedding_text: None,
            range: FragmentRange {
                start_line: 1,
                end_line: 1,
                start_offset: 0,
                end_offset: 12,
            },
            group: None,
            metadata: Some(FragmentMetadata::Code {
                symbol_type: CodeSymbolType::Module,
                symbol_name: Some("$file".to_string()),
                scope: None,
                node_type: "module".to_string(),
                signature: None,
                doc: Some("Top of file".to_string()),
                modifiers: Vec::new(),
            }),
            calls: Vec::new(),
        },
    ];
    let blocks = [
        scope_block("alpha", "alpha", "", "Adds one"),
        scope_block("beta", "beta", "", "Removes two"),
        crate::ast_parser::CommentBlock {
            node_key: "$file".to_string(),
            node_kind: "module".to_string(),
            name: "$file".to_string(),
            scope: String::new(),
            start_byte: 0,
            end_byte: 12,
            start_line: 1,
            end_line: 1,
            text: "Top of file".to_string(),
            comment_hash: common_core::hash::blake3_hex(b"Top of file"),
        },
    ];
    FragmentWriter::write_items(
        &db,
        &item_file("f"),
        FileKind::Code,
        &fragments,
        &blocks,
        &[],
        None,
        VectorPolicy::Optional,
    )
    .expect("commit");
    let lemmas_for = |lemma: &str| {
        db.fragments_for_lemmas(&[lemma.to_string()], 10)
            .expect("lookup")
    };
    assert_eq!(lemmas_for("adds"), vec!["fa".to_string()]);
    assert_eq!(lemmas_for("removes"), vec!["fb".to_string()]);
    assert_eq!(lemmas_for("top"), vec!["f#$file".to_string()]);
}

#[test]
fn offline_live_write_serves_file_recall() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("memory db");
    let file = live_file("live-a");
    let stats = FragmentWriter::write_live(&db, &file, "hello world", None, None).expect("live");
    assert_eq!(stats.fragments, 1);
    assert_eq!(stats.skipped_embedding, 1);
    assert_eq!(
        FragmentWriter::row_shape(&db, "live-a"),
        RowShape::WholeFile
    );
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
        content: FragmentContent::Text {
            text: format!("fn {name}() {{}}"),
        },
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
    db.replace_file(&file_record(&file), &records, &[])
        .expect("batch commit");
    assert_eq!(
        FragmentWriter::row_shape(&db, "batch-a-0"),
        RowShape::Chunked
    );
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
        content: FragmentContent::Text {
            text: "fn solo() {}".to_string(),
        },
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
    assert_eq!(
        FragmentWriter::row_shape(&db, "live-b"),
        RowShape::WholeFile
    );
}
