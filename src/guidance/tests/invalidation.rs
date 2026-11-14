//! Invalidation pins: interface diffs, edge dependents, macro widening.

use crate::invalidation::{defines_macros, dependents_of, file_sig_changed, interface_changes};

#[test]
fn interface_diff_finds_new_deleted_and_rehashed() {
    let stored = vec![
        ("a".to_string(), "h1".to_string()),
        ("b".to_string(), "h2".to_string()),
        ("c".to_string(), "h3".to_string()),
    ];
    let fresh = vec![
        ("a".to_string(), "h1".to_string()),
        ("b".to_string(), "h9".to_string()),
        ("d".to_string(), "h4".to_string()),
    ];
    assert_eq!(
        interface_changes(&stored, &fresh),
        vec!["b".to_string(), "c".to_string(), "d".to_string()]
    );
    assert!(file_sig_changed(&stored, &fresh));
    assert!(!file_sig_changed(&stored, &stored));
    assert!(!file_sig_changed(&[], &[]));
}

#[test]
fn dependents_cover_stem_resolved_and_glob() {
    let edges = vec![
        ("b.rs".to_string(), "crate::a::foo".to_string(), None),
        ("c.rs".to_string(), "unrelated".to_string(), None),
        ("d.rs".to_string(), "crate::*".to_string(), None),
        (
            "e.rs".to_string(),
            "other".to_string(),
            Some("src/a.rs".to_string()),
        ),
    ];
    let found = dependents_of(&edges, &["src/a.rs".to_string()]);
    assert_eq!(
        found,
        vec!["b.rs".to_string(), "d.rs".to_string(), "e.rs".to_string()]
    );
}

#[test]
fn macro_files_widen_to_their_root() {
    let scanned = vec![
        ("r/a.rs".to_string(), "r".to_string()),
        ("r/b.rs".to_string(), "r".to_string()),
        ("s/c.rs".to_string(), "s".to_string()),
    ];
    assert!(defines_macros("macro_rules! m { () => {}; }"));
    assert!(!defines_macros("pub fn f() {}"));
    let widened = crate::invalidation::macro_expansion(&scanned, &["r/a.rs".to_string()]);
    assert_eq!(widened, vec!["r/b.rs".to_string()]);
    let empty = crate::invalidation::macro_expansion(&scanned, &["s/c.rs".to_string()]);
    assert!(empty.is_empty());
}

fn gate_entity(name: &str, scope: &str, signature: &str) -> crate::search_types::EntityFragment {
    use crate::search_types::{
        CodeSymbolType, EntityFragment, EntityMetadata, FragmentContent, FragmentSpan,
    };
    EntityFragment {
        id: String::new(),
        group: None,
        file_id: "file-a".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Text {
            text: String::new(),
        },
        metadata: Some(EntityMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some(name.to_string()),
            scope: Some(scope.to_string()),
            node_type: Some("function_item".to_string()),
            signature: Some(signature.to_string()),
            doc: None,
            modifiers: Vec::new(),
        }),
    }
}

fn gate_hash(name: &str, signature: &str) -> String {
    crate::query::db_storage::interface_hash_for_entity(&gate_entity(name, "m", signature))
}

fn stable_pair(name: &str, hash: &str) -> (String, String) {
    (
        crate::query::db_storage::stable_symbol_id("file-a", Some(name), Some("m")),
        hash.to_string(),
    )
}

#[test]
fn sig_gate_equivalent_on_body_only_edit() {
    // Same positions, same interfaces: neither scheme fires, and no
    // dependent is pulled either way.
    let (ha, hb) = (
        gate_hash("alpha", "fn alpha()"),
        gate_hash("beta", "fn beta()"),
    );
    let old = vec![
        ("file-a#0".to_string(), ha.clone()),
        ("file-a#1".to_string(), hb.clone()),
    ];
    let new = vec![stable_pair("alpha", &ha), stable_pair("beta", &hb)];
    assert!(!file_sig_changed(&old, &old));
    assert!(!file_sig_changed(&new, &new));
    let edges = vec![(
        "importer.rs".to_string(),
        "use file_a::alpha".to_string(),
        None,
    )];
    assert!(dependents_of(&edges, &[]).is_empty());
}

#[test]
fn sig_gate_equivalent_on_signature_edit() {
    // Same symbol moved in both schemes: identical changed-name sets
    // and identical dependent sets.
    let (ha, hb, hb2) = (
        gate_hash("alpha", "fn alpha()"),
        gate_hash("beta", "fn beta()"),
        gate_hash("beta", "fn beta(x: u8)"),
    );
    let old_stored = vec![
        ("file-a#0".to_string(), ha.clone()),
        ("file-a#1".to_string(), hb.clone()),
    ];
    let old_fresh = vec![
        ("file-a#0".to_string(), ha.clone()),
        ("file-a#1".to_string(), hb2.clone()),
    ];
    let new_stored = vec![stable_pair("alpha", &ha), stable_pair("beta", &hb)];
    let new_fresh = vec![stable_pair("alpha", &ha), stable_pair("beta", &hb2)];
    assert!(file_sig_changed(&old_stored, &old_fresh));
    assert!(file_sig_changed(&new_stored, &new_fresh));
    assert_eq!(
        interface_changes(&old_stored, &old_fresh),
        vec!["file-a#1".to_string()]
    );
    let changed_name = |id: &str| {
        if id == stable_pair("beta", &hb2).0 {
            "beta"
        } else {
            "?"
        }
    };
    assert_eq!(
        interface_changes(&new_stored, &new_fresh)
            .iter()
            .map(|id| changed_name(id))
            .collect::<Vec<_>>(),
        vec!["beta"]
    );
    // Same file moved, same dependents pulled.
    let edges = vec![("importer.rs".to_string(), "file_a::beta".to_string(), None)];
    let moved = vec!["src/file_a.rs".to_string()];
    assert_eq!(
        dependents_of(&edges, &moved),
        vec!["importer.rs".to_string()]
    );
}

#[test]
fn sig_gate_window_count_shift_stops_firing() {
    // Same symbol set, new chunking: alpha splits into major +
    // window, shifting beta's positional id. The positional scheme
    // reports new + rehashed ids (the over-invalidation); the stable
    // scheme reports nothing — strictly fewer reprocesses, never more.
    let (ha, hb) = (
        gate_hash("alpha", "fn alpha()"),
        gate_hash("beta", "fn beta()"),
    );
    let old_stored = vec![
        ("file-a#0".to_string(), ha.clone()),
        ("file-a#1".to_string(), hb.clone()),
    ];
    let old_fresh = vec![
        ("file-a#0".to_string(), ha.clone()),
        ("file-a#1".to_string(), ha.clone()),
        ("file-a#2".to_string(), hb.clone()),
    ];
    let new_pairs = vec![stable_pair("alpha", &ha), stable_pair("beta", &hb)];
    assert!(
        file_sig_changed(&old_stored, &old_fresh),
        "positional ids fire on a pure window-count shift"
    );
    assert!(
        !file_sig_changed(&new_pairs, &new_pairs),
        "stable ids hold on the same shift"
    );
    let edges = vec![("importer.rs".to_string(), "file_a::beta".to_string(), None)];
    assert_eq!(
        dependents_of(&edges, &["src/file_a.rs".to_string()]),
        vec!["importer.rs".to_string()]
    );
    assert!(dependents_of(&edges, &[]).is_empty());
}
