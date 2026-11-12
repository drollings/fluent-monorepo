//! Symbol identity: body edits keep the interface hash, signature edits move it.

use super::{content_hash_for_entity, interface_hash_for_entity, symbol_record};
use crate::search_types::{
    CodeSymbolType, EntityFragment, EntityMetadata, FragmentContent, FragmentSpan,
};

fn code_entity(id: &str, signature: &str, body: &str) -> EntityFragment {
    EntityFragment {
        id: id.to_string(),
        group: None,
        file_id: "file-a".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Text {
            text: body.to_string(),
        },
        metadata: Some(EntityMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some("render".to_string()),
            scope: Some("module".to_string()),
            node_type: Some("function".to_string()),
            signature: Some(signature.to_string()),
            doc: None,
            modifiers: Vec::new(),
        }),
    }
}

#[test]
fn repeated_rows_are_identical() {
    let entity = code_entity("frag-1", "fn render(x: u8)", "fn render(x: u8) { body }");
    let first = symbol_record(&entity).expect("definition yields a row");
    let second = symbol_record(&entity).expect("definition yields a row");
    assert_eq!(first, second);
}

#[test]
fn body_only_edit_keeps_interface_hash() {
    let before = code_entity("frag-1", "fn render(x: u8)", "fn render(x: u8) { one }");
    let after = code_entity(
        "frag-1",
        "fn render(x: u8)",
        "fn render(x: u8) { two + more }",
    );
    assert_eq!(
        interface_hash_for_entity(&before),
        interface_hash_for_entity(&after)
    );
    assert_ne!(
        content_hash_for_entity(&before),
        content_hash_for_entity(&after)
    );
}

#[test]
fn signature_edit_moves_interface_hash() {
    let before = code_entity("frag-1", "fn render(x: u8)", "fn render(x: u8) { body }");
    let after = code_entity("frag-1", "fn render(x: u16)", "fn render(x: u16) { body }");
    assert_ne!(
        interface_hash_for_entity(&before),
        interface_hash_for_entity(&after)
    );
}

#[test]
fn chunks_without_metadata_yield_no_row() {
    let plain = EntityFragment {
        id: "frag-plain".to_string(),
        group: None,
        file_id: "file-a".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Text {
            text: "hello".to_string(),
        },
        metadata: None,
    };
    assert!(symbol_record(&plain).is_none());
}

#[test]
fn symbols_round_trip_through_replace_symbols() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("memory db");
    let entity = code_entity("frag-1", "fn render(x: u8)", "fn render(x: u8) { body }");
    let row = symbol_record(&entity).expect("row");
    db.replace_symbols(std::slice::from_ref(&row))
        .expect("write symbols");
    let stored = db.symbols_for_file("file-a").expect("read symbols");
    assert_eq!(stored, vec![row]);
}
