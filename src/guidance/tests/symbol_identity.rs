//! Symbol identity: body edits keep the interface hash, signature edits move it.

use super::{content_hash_for_entity, interface_hash_for_entity, stable_symbol_id, symbol_record};
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

#[test]
fn stable_ids_ignore_positional_index() {
    let first = stable_symbol_id("file-a", Some("render"), Some("module"));
    let second = stable_symbol_id("file-a", Some("render"), Some("module"));
    assert_eq!(first, second);
    assert_eq!(first, "file-a#module::render");
    assert_eq!(stable_symbol_id("f", Some("render"), None), "f#render");
}

#[test]
fn stable_ids_separate_scope_and_name() {
    let scoped = stable_symbol_id("f", Some("render"), Some("module"));
    let top = stable_symbol_id("f", Some("render"), None);
    let other = stable_symbol_id("f", Some("other"), Some("module"));
    assert_ne!(scoped, top);
    assert_ne!(scoped, other);
    assert_ne!(top, other);
}

#[test]
fn stable_ids_cover_unnamed_and_markdown_shapes() {
    let anon = stable_symbol_id("f", None, None);
    let headed = stable_symbol_id("f", Some("Guide"), None);
    assert_ne!(anon, headed);
    assert_eq!(stable_symbol_id("f", None, None), anon);
    let scoped_headed = stable_symbol_id("f", Some("Body"), Some("Guide"));
    assert_ne!(scoped_headed, headed);
}

#[test]
fn reordered_and_windowed_harvests_keep_stable_ids() {
    // Reorder and window-count edits move positional ids but never
    // the (file, symbol, scope) triple: the stable id is invariant.
    fn named(id: &str, name: &str) -> EntityFragment {
        let mut entity = code_entity(id, "fn f()", "fn f() { 1 }");
        if let Some(EntityMetadata::Code { symbol_name, .. }) = entity.metadata.as_mut() {
            *symbol_name = Some(name.to_string());
        }
        entity
    }
    let v1_alpha = symbol_record(&named("file-a#0", "alpha")).expect("row");
    let v1_beta = symbol_record(&named("file-a#1", "beta")).expect("row");
    // Reordered harvest: positional ids swap, stable ids do not.
    let v2_alpha = symbol_record(&named("file-a#1", "alpha")).expect("row");
    let v2_beta = symbol_record(&named("file-a#0", "beta")).expect("row");
    assert_eq!(v1_alpha.symbol_id, v2_alpha.symbol_id);
    assert_eq!(v1_beta.symbol_id, v2_beta.symbol_id);
    assert_ne!(v1_alpha.symbol_id, v1_beta.symbol_id);
    // Windowed harvest: extra same-symbol chunks reuse the id.
    let v3_alpha_window = symbol_record(&named("file-a#7", "alpha")).expect("row");
    assert_eq!(v1_alpha.symbol_id, v3_alpha_window.symbol_id);
}
