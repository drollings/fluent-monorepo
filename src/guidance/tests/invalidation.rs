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
        ("e.rs".to_string(), "other".to_string(), Some("src/a.rs".to_string())),
    ];
    let found = dependents_of(&edges, &["src/a.rs".to_string()]);
    assert_eq!(found, vec!["b.rs".to_string(), "d.rs".to_string(), "e.rs".to_string()]);
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
