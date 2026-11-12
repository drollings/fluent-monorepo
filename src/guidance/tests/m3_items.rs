//! Item-hash pins: normalized sig/body/doc split, interface stability,
//! work keys, merge fallback, and parse-time hash stamping.

use std::collections::BTreeMap;

use crate::item_hashes::{interface_stable, item_hashes_for, normalize_code_text, work_key};
use crate::query::db_storage::{content_hash_for_entity, interface_hash_for_entity};
use crate::search_types::{
    CodeEntityModifier, CodeSymbolType, EntityFragment, EntityMetadata, FragmentContent,
    FragmentSpan,
};

fn rust_fn(symbol: &str, sig: &str, body: &str) -> EntityFragment {
    EntityFragment {
        id: "e1".to_string(),
        group: None,
        file_id: "f1".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Text {
            text: format!("{sig} {{ {body} }}"),
        },
        metadata: Some(EntityMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some(symbol.to_string()),
            scope: Some("crate::m".to_string()),
            node_type: Some("function_item".to_string()),
            signature: Some(sig.to_string()),
            doc: None,
            modifiers: vec![CodeEntityModifier::Public],
        }),
    }
}

#[test]
fn cosmetic_edits_keep_sig_stable() {
    let plain = item_hashes_for("rust", "fn f(x: u8) -> u8", "x + 1", None);
    let spaced = item_hashes_for("rust", "fn  f( x:u8 )->u8", "x + 1", None);
    assert_eq!(plain.sig, spaced.sig);
    assert_eq!(plain.body, spaced.body);
    assert_eq!(plain.item, spaced.item);
    let commented = item_hashes_for("rust", "fn f(x: u8) // trailing\n-> u8", "x + 1", None);
    assert_eq!(plain.sig, commented.sig);
    let blocked = item_hashes_for("rust", "fn f(/* arg */ x: u8) -> u8", "x + 1", None);
    assert_eq!(plain.sig, blocked.sig);
    let changed = item_hashes_for("rust", "fn f(x: u16) -> u8", "x + 1", None);
    assert_ne!(plain.sig, changed.sig);
    assert_ne!(plain.item, changed.item);
}

#[test]
fn doc_edits_never_cascade_to_sig_or_body() {
    let before = item_hashes_for("rust", "fn f(x: u8) -> u8", "x + 1", Some("/// Adds one."));
    let after = item_hashes_for(
        "rust",
        "fn f(x: u8) -> u8",
        "x + 1",
        Some("/// Adds one more."),
    );
    assert_eq!(before.sig, after.sig);
    assert_eq!(before.body, after.body);
    assert_ne!(before.doc, after.doc);
    assert_ne!(before.item, after.item);
}

#[test]
fn body_only_edits_move_item_not_sig() {
    let pairs = [
        ("let y = x + 1; y", "let y = x + 2; y"),
        ("x", "x /* bump */ + 0"),
        ("  x\n", "x"),
    ];
    for (a, b) in pairs {
        let first = item_hashes_for("rust", "fn f(x: u8) -> u8", a, None);
        let second = item_hashes_for("rust", "fn f(x: u8) -> u8", b, None);
        assert_eq!(first.sig, second.sig, "body edit must not move sig: {a:?}");
    }
    let real = item_hashes_for("rust", "fn f(x: u8) -> u8", "x + 1", None);
    let edited = item_hashes_for("rust", "fn f(x: u8) -> u8", "x + 2", None);
    assert_eq!(real.sig, edited.sig);
    assert_ne!(real.body, edited.body);
    assert_ne!(real.item, edited.item);
}

#[test]
fn normalize_is_comment_aware_for_rust_only() {
    assert_eq!(
        normalize_code_text("rust", "a // b"),
        normalize_code_text("rust", "a")
    );
    assert_eq!(
        normalize_code_text("rust", "a /* b */ c"),
        normalize_code_text("rust", "a c")
    );
    assert_eq!(
        normalize_code_text("rust", "let s = \"a // b\";"),
        normalize_code_text("rust", "let s = \"a // b\" ;")
    );
    assert_eq!(normalize_code_text("zig", "a // b"), "a // b");
    assert_eq!(normalize_code_text("python", "a  # b"), "a # b");
}

#[test]
fn interface_hash_survives_body_edit() {
    let before = rust_fn("f", "fn f(x: u8) -> u8", "x + 1");
    let after = rust_fn("f", "fn f(x: u8) -> u8", "x + 2");
    assert_eq!(
        interface_hash_for_entity(&before),
        interface_hash_for_entity(&after)
    );
    assert_ne!(
        content_hash_for_entity(&before),
        content_hash_for_entity(&after)
    );
    assert!(interface_stable(
        "rust",
        "fn f(x: u8) -> u8",
        "fn  f( x:u8 ) -> u8"
    ));
    assert!(interface_stable(
        "rust",
        "fn f(x: u8) -> u8",
        "fn f(x: u8) -> u8 // same"
    ));
    // Whitespace-only variants also agree at the stored-hash layer.
    let spaced = rust_fn("f", "fn  f( x:u8 ) -> u8", "x + 1");
    assert_eq!(
        interface_hash_for_entity(&before),
        interface_hash_for_entity(&spaced)
    );
    assert!(!interface_stable(
        "rust",
        "fn f(x: u8) -> u8",
        "fn f(x: u16) -> u8"
    ));
}

#[test]
fn work_key_tracks_item_deps_and_components() {
    let comps: BTreeMap<String, String> = [("schema".to_string(), "s1".to_string())]
        .into_iter()
        .collect();
    let base = work_key("item1", &["depA:s1"], &comps);
    assert_eq!(base, work_key("item1", &["depA:s1"], &comps));
    assert_ne!(base, work_key("item2", &["depA:s1"], &comps));
    assert_ne!(base, work_key("item1", &["depA:s2"], &comps));
    let mut evolved = comps.clone();
    evolved.insert("prompt".to_string(), "p9".to_string());
    assert_ne!(base, work_key("item1", &["depA:s1"], &evolved));
    let bare: BTreeMap<String, String> = BTreeMap::new();
    assert_eq!(
        work_key("item1", &["depA:s1"], &bare),
        work_key("item1", &["depA:s1"], &bare)
    );
}

#[test]
fn merge_falls_back_to_stable_interface() {
    use crate::sync::json_store::merge_member;
    use crate::tests::common::make_test_member;
    let existing = make_test_member("foo", "fn foo() void", "abc123", Some("Kept"));
    // Hashless pair with a stable interface still preserves prose.
    let mut new = make_test_member("foo", "fn  foo() void", "abc123", None);
    new.match_hash = None;
    let mut existing_hashless = existing.clone();
    existing_hashless.match_hash = None;
    merge_member(&existing_hashless, &mut new);
    assert_eq!(new.comment.as_deref(), Some("Kept"));
    // An unstable interface never preserves, hashes or not.
    let mut changed = make_test_member("foo", "fn foo(x: i32) void", "def456", None);
    changed.match_hash = None;
    merge_member(&existing_hashless, &mut changed);
    assert!(changed.comment.is_none());
}

#[test]
fn parse_stamps_normalized_match_hashes() {
    let mut parser = crate::ast_parser::AstParser::new();
    let source = "/// Adds one.\npub fn add(x: u64, y: u64) -> u64 {\n    x + y\n}\n";
    let doc = parser
        .parse_file(std::path::Path::new("m.rs"), source)
        .expect("rust parses");
    assert!(!doc.members.is_empty());
    for member in &doc.members {
        let sig = member.signature.as_deref().unwrap_or("");
        assert_eq!(
            member.match_hash.as_deref(),
            Some(crate::sync::staleness::match_hash_from_signature(sig).as_str()),
            "member {} must carry its normalized hash",
            member.name.as_str()
        );
    }
}
